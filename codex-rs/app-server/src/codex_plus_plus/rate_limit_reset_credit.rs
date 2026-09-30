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
) -> Result<ConsumeRateLimitResetCreditResponse, JSONRPCErrorError> {
    let request_timeout = REQUEST_TIMEOUT;
    #[cfg(debug_assertions)]
    let request_timeout = std::env::var(REQUEST_TIMEOUT_ENV_VAR)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(request_timeout);
    let deadline = Instant::now() + request_timeout;
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
    let source = if processor.config.model_provider.is_cli_proxy() {
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
        Some(match selected {
            NativeCredentialSource::Root(_) => ResetCredentialSource::Root,
            NativeCredentialSource::Imported(_) => ResetCredentialSource::Imported,
        })
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
                .begin_manual(&params.idempotency_key, *source)
                .map_err(|error| internal_error(error.to_string()))?,
        ),
        (Some(_), None) => return Err(invalid_request("reset account is unavailable")),
        (None, _) => None,
    };
    let response =
        tokio::time::timeout(deadline.saturating_duration_since(Instant::now()), async {
            match params.credit_id.as_deref() {
                Some(credit_id) => {
                    client
                        .consume_rate_limit_reset_credit_by_id(&params.idempotency_key, credit_id)
                        .await
                }
                None => {
                    client
                        .consume_rate_limit_reset_credit(&params.idempotency_key)
                        .await
                }
            }
        })
        .await
        .map_err(|_| timeout_error())?
        .map_err(|err| internal_error(format!("failed to consume rate limit reset: {err}")))?;
    if let (Some(attempt), Some(lease)) = (attempt, lease.as_mut())
        && attempt != ManualResetAttempt::Completed
    {
        let confirmed = response.code == ConsumeRateLimitResetCreditCode::Reset
            || (response.code == ConsumeRateLimitResetCreditCode::AlreadyRedeemed
                && attempt == ManualResetAttempt::Pending);
        let result = if confirmed {
            lease.confirm_manual(
                &params.idempotency_key,
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX),
            )
        } else {
            lease.clear_redeeming(&params.idempotency_key)
        };
        result.map_err(|error| internal_error(error.to_string()))?;
    }
    Ok(response)
}

fn timeout_error() -> JSONRPCErrorError {
    internal_error("rate limit reset consume timed out")
}
