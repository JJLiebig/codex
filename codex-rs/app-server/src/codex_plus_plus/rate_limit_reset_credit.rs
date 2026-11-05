use super::*;
use codex_backend_client::ConsumeRateLimitResetCreditCode;
use codex_backend_client::ConsumeRateLimitResetCreditResponse;
use codex_login::AccountStore;
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
    let Some(auth) = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), auth_task)
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
    let client = BackendClient::from_auth(
        processor.config.chatgpt_base_url.clone(),
        &auth,
        processor.config.http_client_factory(),
    );
    let store = AccountStore::new(processor.config.codex_home.to_path_buf());
    let lease = store
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
    if let Some(lease) = &lease {
        let state = lease
            .state()
            .map_err(|error| internal_error(error.to_string()))?;
        if let Some(codex_login::ResetAttemptPhase::ManualRedeeming {
            redeem_request_id, ..
        }) = state.phase
        {
            return Err(invalid_request(format!(
                "A previous reset attempt ({redeem_request_id}) needs settlement. Contact support to confirm its outcome before using another reset for this account."
            )));
        }
        if state
            .completion
            .is_some_and(|receipt| receipt.id == params.idempotency_key)
        {
            return Ok(ConsumeRateLimitResetCreditResponse {
                code: ConsumeRateLimitResetCreditCode::AlreadyRedeemed,
                windows_reset: 0,
            });
        }
    }
    let request_id = params.idempotency_key.as_str();
    let credit_id = params.credit_id.as_deref();
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
    Ok(response)
}

fn timeout_error() -> JSONRPCErrorError {
    internal_error("rate limit reset consume timed out")
}
