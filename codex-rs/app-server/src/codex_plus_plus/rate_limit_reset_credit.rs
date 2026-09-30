use super::*;
use codex_backend_client::ConsumeRateLimitResetCreditCode;
use codex_backend_client::ConsumeRateLimitResetCreditResponse;
use codex_login::AccountStore;
use codex_login::ManualResetAttempt;
use codex_login::NativeCredentialSource;
use codex_login::ResetCredentialSource;
use std::time::Instant;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 10);
#[cfg(debug_assertions)]
const REQUEST_TIMEOUT_ENV_VAR: &str = "CODEX_TEST_RATE_LIMIT_RESET_REQUEST_TIMEOUT_MS";

pub(super) async fn consume(
    processor: &AccountRequestProcessor,
    params: &ConsumeAccountRateLimitResetCreditParams,
) -> Result<
    (
        ConsumeRateLimitResetCreditResponse,
        Option<codex_app_server_protocol::UsageResetCompletion>,
    ),
    JSONRPCErrorError,
> {
    let request_timeout = REQUEST_TIMEOUT;
    #[cfg(debug_assertions)]
    let request_timeout = std::env::var(REQUEST_TIMEOUT_ENV_VAR)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(request_timeout);
    let deadline = Instant::now() + request_timeout;
    let mut owned = processor.config.model_provider.is_cli_proxy();
    if let Some(thread_id) = &params.thread_id {
        let thread_id = codex_protocol::ThreadId::from_string(thread_id)
            .map_err(|error| invalid_request(format!("invalid thread id: {error}")))?;
        let thread = processor
            .thread_manager
            .get_thread(thread_id)
            .await
            .map_err(|_| invalid_request(format!("thread not found: {thread_id}")))?;
        owned |= thread.config().await.model_provider.is_cli_proxy();
    }
    let auth_manager = Arc::clone(&processor.auth_manager);
    let auth_task = tokio::spawn(async move { auth_manager.auth().await });
    let Some(mut auth) =
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), auth_task)
            .await
            .map_err(|_| timeout_error())?
            .map_err(|err| {
                internal_error(format!("failed to join rate limit reset auth task: {err}"))
            })?
    else {
        return Err(invalid_request(
            "codex account authentication required for rate limit reset credits",
        ));
    };
    if !auth.uses_codex_backend() {
        return Err(invalid_request(
            "chatgpt authentication required for rate limit reset credits",
        ));
    }
    let source = if owned {
        let snapshot = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            processor.auth_manager.export_native_credentials(),
        )
        .await
        .map_err(|_| timeout_error())?
        .map_err(|error| internal_error(format!("failed to capture reset account: {error}")))?;
        auth = processor
            .auth_manager
            .auth_cached()
            .ok_or_else(|| invalid_request("reset account changed"))?;
        let token = auth
            .get_token()
            .map_err(|error| internal_error(error.to_string()))?;
        let selected = snapshot
            .selected_source()
            .filter(|source| {
                snapshot.credentials().iter().any(|credential| {
                    &credential.source == *source
                        && credential.access_token == token
                        && auth.get_account_id().as_deref()
                            == Some(credential.upstream_account_id.as_str())
                })
            })
            .ok_or_else(|| invalid_request("reset account changed; retry the request"))?;
        Some(selected.clone())
    } else {
        None
    };
    let client = BackendClient::from_auth(
        processor.config.chatgpt_base_url.clone(),
        &auth,
        processor.config.http_client_factory(),
    );
    let store = AccountStore::new(processor.config.codex_home.to_path_buf());
    let mut lease = store
        .acquire_reset_mutation_lease_for_auth(&auth, deadline)
        .await
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::TimedOut {
                timeout_error()
            } else {
                internal_error(format!("failed to acquire rate limit reset lease: {err}"))
            }
        })?;
    if Instant::now() >= deadline {
        return Err(timeout_error());
    }
    let attempt = match (&source, lease.as_mut()) {
        (Some(source), Some(lease)) => Some(
            lease
                .begin_manual(
                    &params.idempotency_key,
                    match source {
                        NativeCredentialSource::Root(_) => ResetCredentialSource::Root,
                        NativeCredentialSource::Imported(_) => ResetCredentialSource::Imported,
                    },
                    params.credit_id.as_deref(),
                )
                .map_err(|error| internal_error(error.to_string()))?,
        ),
        (Some(_), None) => return Err(invalid_request("reset account is unavailable")),
        (None, _) => None,
    };
    // A reopened dialog may supply a new key: settle the original ambiguous attempt first.
    let (request_id, credit_id) = match &attempt {
        Some(ManualResetAttempt::Pending {
            redeem_request_id,
            credit_id,
        }) => (redeem_request_id.as_str(), credit_id.as_deref()),
        Some(ManualResetAttempt::Fresh | ManualResetAttempt::Completed) | None => {
            (params.idempotency_key.as_str(), params.credit_id.as_deref())
        }
    };
    let response =
        tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
            match credit_id {
                Some(credit_id) => {
                    client
                        .consume_rate_limit_reset_credit_by_id(request_id, credit_id)
                        .await
                }
                None => client.consume_rate_limit_reset_credit(request_id).await,
            }
        })
        .await
        .map_err(|_| timeout_error())?
        .map_err(|err| internal_error(format!("failed to consume rate limit reset: {err}")))?;
    if let (Some(attempt), Some(lease)) = (&attempt, lease.as_mut())
        && !matches!(attempt, ManualResetAttempt::Completed)
    {
        let confirmed = response.code == ConsumeRateLimitResetCreditCode::Reset
            || (response.code == ConsumeRateLimitResetCreditCode::AlreadyRedeemed
                && matches!(attempt, ManualResetAttempt::Pending { .. }));
        let result = if confirmed {
            lease.confirm_manual(
                request_id,
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX),
            )
        } else {
            lease.clear_redeeming(request_id)
        };
        result.map_err(|error| internal_error(error.to_string()))?;
    }
    let completion = if source.is_some() {
        lease
            .as_ref()
            .map(codex_login::ResetMutationLease::state)
            .transpose()
            .map_err(|error| internal_error(error.to_string()))?
            .and_then(|state| state.completion)
            .filter(|completion| completion.id == request_id)
    } else {
        None
    };
    drop(lease); // Runtime attachment must precede reacquiring the native reset lease.
    if let (Some(completion), Some(source)) = (&completion, &source)
        && completion.needs_proxy_reconciliation()
    {
        let (NativeCredentialSource::Root(account_id)
        | NativeCredentialSource::Imported(account_id)) = source;
        let reconciliation =
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
                let usage = client.get_rate_limits_with_reset_credits().await?;
                codex_model_provider::reconcile_cli_proxy_reset(
                    &processor.config.codex_home,
                    processor.config.http_client_factory(),
                    account_id,
                    completion,
                    usage.account_id.as_deref(),
                    &usage.rate_limits,
                    usage.ordinary_usage_allowed,
                )
                .await?;
                anyhow::Ok(())
            })
            .await;
        if !matches!(reconciliation, Ok(Ok(()))) {
            tracing::warn!("usage reset confirmed; proxy cooldown reconciliation remains pending");
        }
    }
    if request_id != params.idempotency_key || credit_id != params.credit_id.as_deref() {
        return Err(invalid_request(
            "Previous reset attempt resolved. No new reset was used; refresh usage before trying again.",
        ));
    }
    let reset_completion = completion.zip(source).map(|(completion, source)| {
        let (source, account_id) = match source {
            NativeCredentialSource::Root(id) => (
                codex_protocol::inference_attribution::InferenceNativeSource::Root,
                id,
            ),
            NativeCredentialSource::Imported(id) => (
                codex_protocol::inference_attribution::InferenceNativeSource::Imported,
                id,
            ),
        };
        codex_app_server_protocol::UsageResetCompletion {
            id: completion.id,
            source,
            account_id: account_id.to_string(),
            completed_at: completion.completed_at / 1_000_000_000,
            completed_at_ns: completion.completed_at.to_string(),
        }
    });
    Ok((response, reset_completion))
}

fn timeout_error() -> JSONRPCErrorError {
    internal_error("rate limit reset consume timed out")
}
