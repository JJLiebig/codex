use super::*;
use super::super::super::create_auth_storage_with_store;
use super::super::super::FileAuthStorage;
use codex_keyring_store::tests::MockKeyringStore;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[test]
fn observed_storage_forwards_guarded_operations_and_preserves_rollback_file() -> anyhow::Result<()>
{
    for (mode, kind) in [
        (
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::Direct,
        ),
        (
            AuthCredentialsStoreMode::Keyring,
            AuthKeyringBackendKind::Direct,
        ),
        (
            AuthCredentialsStoreMode::Keyring,
            AuthKeyringBackendKind::Secrets,
        ),
    ] {
        let home = tempdir()?;
        let storage = create_auth_storage_with_store(
            home.path().to_path_buf(),
            mode,
            Arc::new(MockKeyringStore::default()),
            kind,
        );
        let guard = AuthRefreshGuard::acquire(home.path())?;
        let auth: AuthDotJson = serde_json::from_value(serde_json::json!({"OPENAI_API_KEY": "guarded"}))?;
        storage.save_with_guard(&auth, &guard)?;
        assert_eq!(storage.load_with_guard(&guard)?, Some(auth.clone()));
        let fallback = AuthDotJson { openai_api_key: Some("rollback-file".into()), ..auth.clone() };
        FileAuthStorage::new(home.path().to_path_buf()).save_with_guard(&fallback, &guard)?;
        if mode == AuthCredentialsStoreMode::Keyring {
            storage.save_preserving_file(&auth)?;
            assert_eq!(
                FileAuthStorage::new(home.path().to_path_buf()).load_with_guard(&guard)?,
                Some(fallback)
            );
        }
        assert!(storage.delete_with_guard(&guard)?);
        assert_eq!(storage.load_with_guard(&guard)?, None);
    }
    Ok(())
}
