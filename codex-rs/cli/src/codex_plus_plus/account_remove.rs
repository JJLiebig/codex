use anyhow::Context;
use codex_core::config::Config;
use codex_login::AccountStore;

pub(super) async fn run(config: &Config) -> anyhow::Result<()> {
    let profile = codex_tui::choose_account_to_remove(config)
        .await
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    let Some(profile) = profile else {
        return Ok(());
    };
    let store = AccountStore::new(config.codex_home.to_path_buf());
    let store_mode = config.cli_auth_credentials_store_mode;
    let keyring_backend_kind = config.auth_keyring_backend_kind();
    let account_id = profile.id.clone();
    let removed = tokio::task::spawn_blocking(move || {
        store.remove(&account_id, store_mode, keyring_backend_kind)
    })
    .await?
    .context("failed to remove account")?;
    if removed {
        println!("Removed account {}", profile.label);
    }
    Ok(())
}
