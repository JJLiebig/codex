use super::super::super::AuthKeyringBackendKind;
use super::*;
use codex_keyring_store::tests::MockKeyringStore;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tempfile::tempdir;

#[test]
fn account_grant_respects_file_authority_over_stale_keyring() -> anyhow::Result<()> {
    for backend in [
        AuthKeyringBackendKind::Direct,
        AuthKeyringBackendKind::Secrets,
    ] {
        let home = tempdir()?;
        let storage = AutoAuthStorage::new(
            home.path().to_path_buf(),
            Arc::new(MockKeyringStore::default()),
            backend,
        );
        let stale: AuthDotJson = serde_json::from_value(serde_json::json!({
            "auth_mode": "apikey", "OPENAI_API_KEY": "stale-keyring-account"
        }))?;
        let current: AuthDotJson = serde_json::from_value(serde_json::json!({
            "auth_mode": "apikey", "OPENAI_API_KEY": "current-file-account"
        }))?;
        storage.keyring_storage.save(&stale)?;
        storage.file_storage.save(&current)?;
        storage.file_authority.activate()?;

        assert_eq!(storage.load_account_grant()?, Some(current));
        std::fs::remove_file(home.path().join("auth.json"))?;
        assert!(storage.load_account_grant().is_err());
        assert_eq!(storage.keyring_storage.load()?, Some(stale));
    }
    Ok(())
}
