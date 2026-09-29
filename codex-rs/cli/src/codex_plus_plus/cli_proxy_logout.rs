use codex_core::config::Config;
use codex_login::AuthManager;

pub(crate) async fn cleanup_after_logout(
    config: &Config,
    manager: &AuthManager,
) -> std::io::Result<()> {
    codex_model_provider::cleanup_cli_proxy_credentials(
        &config.codex_home,
        manager,
        config.http_client_factory(),
    )
    .await
}

pub(crate) fn finish_proxy_cleanup(result: std::io::Result<()>) {
    if result.is_err() {
        eprintln!("Native logout completed, but proxy credential cleanup failed. Retry logout.");
        std::process::exit(1);
    }
}
