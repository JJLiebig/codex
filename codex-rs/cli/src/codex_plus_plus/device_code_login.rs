use codex_login::ServerOptions;

/// Runs the CLI device login flow without exiting, so callers can import the login.
pub async fn login_with_device_code(opts: ServerOptions) -> std::io::Result<()> {
    super::clear_existing_auth_before_login(
        &opts.codex_home,
        opts.cli_auth_credentials_store_mode,
        opts.auth_keyring_backend_kind,
        &opts.auth_route_config,
    )
    .await;
    codex_login::run_device_code_login(opts).await
}

#[cfg(test)]
#[path = "device_code_login_tests.rs"]
mod tests;
