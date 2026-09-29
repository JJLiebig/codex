use super::Config;
use super::JSONRPCErrorError;
use super::internal_error;
use codex_login::AuthManager;

pub(super) async fn cleanup_after_logout(
    config: &Config,
    manager: &AuthManager,
) -> Result<(), JSONRPCErrorError> {
    codex_model_provider::cleanup_cli_proxy_credentials(
        &config.codex_home,
        manager,
        config.http_client_factory(),
    )
    .await
    .map_err(|_| {
        internal_error(
            "Native logout completed, but proxy credential cleanup failed. Retry logout."
                .to_string(),
        )
    })
}
