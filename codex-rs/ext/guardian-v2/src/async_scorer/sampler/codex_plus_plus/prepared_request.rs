//! Owned HTTP request preparation leaves native connection pooling unchanged.

use super::*;
use codex_model_provider::PreparedModelRequest;

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
