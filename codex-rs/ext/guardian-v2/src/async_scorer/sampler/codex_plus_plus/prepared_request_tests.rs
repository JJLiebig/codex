use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn unattributed_owned_auth_errors_do_not_consume_native_recovery() -> anyhow::Result<()> {
    let config = Arc::new(super::super::super::tests::sampler_config(
        "http://127.0.0.1:1/v1".into(),
    ));
    let pool = ConnectionPool::new(config.clone());
    let manager = codex_login::AuthManager::from_auth_for_testing(
        CodexAuth::create_dummy_chatgpt_auth_for_testing(),
    );
    let mut recovery = Some(manager.unauthorized_recovery());
    assert!(recovery.as_ref().unwrap().has_next());
    let mut prepared =
        codex_model_provider::prepare_configured_request(config.provider.as_ref(), "classifier")
            .await?;
    prepared.http_client = Some(
        config
            .http_client_factory
            .build_client("http://127.0.0.1:1/v1", ClientRouteClass::Api)?,
    );
    let permit = pool.classifications.clone().acquire_owned().await?;
    let lease = lease(&pool, permit, prepared)?;
    for (status, code) in [
        (401, "auth_unavailable"),
        (503, "upstream_authentication_required"),
    ] {
        let error = LunaSamplerError::Api(ApiError::Transport(TransportError::Http {
            status: http::StatusCode::from_u16(status)?,
            url: None,
            retry_after: None,
            headers: Some(HeaderMap::from_iter([(
                http::HeaderName::from_static("x-cpa-trace-id"),
                HeaderValue::from_static("20260930010000-0000000000000001-aabbccdd"),
            )])),
            body: Some(
                serde_json::json!({"error":{"type":"authentication_error","code":code}})
                    .to_string(),
            ),
        }));
        assert_eq!(
            lease.retry_owned_auth(&error, &mut recovery).await,
            Some(false)
        );
        assert_eq!(recovery.as_ref().unwrap().step_name(), "reload");
    }
    Ok(())
}
