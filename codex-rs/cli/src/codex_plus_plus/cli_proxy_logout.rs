use codex_core::config::Config;
use codex_login::AuthManager;

pub(crate) async fn cleanup_after_logout(config: &Config, manager: &AuthManager) {
    if codex_model_provider::cleanup_cli_proxy_credentials(
        &config.codex_home,
        manager,
        config.http_client_factory(),
    )
    .await
    .is_err()
    {
        eprintln!("Native logout completed, but proxy credential cleanup failed. Retry logout.");
        std::process::exit(1);
    }
}
