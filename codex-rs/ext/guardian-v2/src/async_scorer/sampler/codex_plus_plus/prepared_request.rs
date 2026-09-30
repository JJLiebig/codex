//! Owned HTTP request preparation leaves native connection pooling unchanged.

use super::*;
use codex_model_provider::PreparedModelRequest;

#[cfg(test)]
#[path = "prepared_request_tests.rs"]
mod tests;

impl ConnectionLease {
    pub(in super::super) fn accepts_owned_output(&self, event: &codex_api::ResponseEvent) -> bool {
        use codex_api::ResponseEvent;
        self.prepared_request().is_some()
            && matches!(
                event,
                ResponseEvent::OutputItemAdded(_)
                    | ResponseEvent::OutputItemDone(_)
                    | ResponseEvent::OutputTextDelta(_)
                    | ResponseEvent::ToolCallInputDelta { .. }
                    | ResponseEvent::ReasoningSummaryDelta { .. }
                    | ResponseEvent::ReasoningSummaryDone { .. }
                    | ResponseEvent::ReasoningContentDelta { .. }
                    | ResponseEvent::ReasoningSummaryPartAdded { .. }
                    | ResponseEvent::Completed { .. }
            )
    }

    pub(in super::super) fn prepared_request(&self) -> Option<&PreparedModelRequest> {
        match &self.connection {
            Connection::Http { prepared, .. } => prepared.as_deref(),
            Connection::Websocket(_) => None,
        }
    }

    pub(in super::super) async fn retry_owned_auth(
        &self,
        error: &LunaSamplerError,
        recovery: &mut Option<codex_login::UnauthorizedRecovery>,
    ) -> Option<bool> {
        let prepared = self.prepared_request()?;
        match error {
            LunaSamplerError::Api(ApiError::Transport(error)) => {
                if codex_model_provider::ProxyRequestRoute::is_suspended_auth_error(error) {
                    return Some(false);
                }
                if !matches!(error, TransportError::Http { status, .. } if *status == http::StatusCode::UNAUTHORIZED)
                {
                    return None;
                }
                let expected = prepared
                    .route
                    .as_ref()
                    .and_then(|route| route.native_auth_failure(error));
                let retry = match (expected, recovery.as_mut()) {
                    (Some(expected), Some(recovery)) if recovery.has_next() => {
                        recovery.next_for_native_request(expected).await.is_ok()
                    }
                    _ => false,
                };
                Some(retry)
            }
            LunaSamplerError::Api(ApiError::Api { status, .. })
                if *status == http::StatusCode::UNAUTHORIZED =>
            {
                Some(false)
            }
            _ => None,
        }
    }
}

pub(super) fn lease(
    pool: &Arc<ConnectionPool>,
    permit: OwnedSemaphorePermit,
    mut request: PreparedModelRequest,
) -> Result<ConnectionLease, LunaSamplerError> {
    // The sampler owns retry attempts for this path too.
    request.provider.retry.max_attempts = 0;
    let client = request.http_client.clone().ok_or_else(|| {
        LunaSamplerError::Api(ApiError::Transport(TransportError::Build(
            "Prepared request has no HTTP client".into(),
        )))
    })?;
    let client = ResponsesClient::new(
        ReqwestTransport::from_http_client(client),
        request.provider.clone(),
        request.auth.auth.clone(),
    );
    Ok(ConnectionLease {
        thread_id: ThreadId::new().to_string(),
        request_kind: RequestMode::Regular,
        connection: Connection::Http {
            client,
            prepared: Some(Box::new(request)),
        },
        pool: Arc::clone(pool),
        _permit: permit,
    })
}
