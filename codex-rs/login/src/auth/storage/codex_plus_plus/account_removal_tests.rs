use super::*;
use codex_keyring_store::tests::MockKeyringStore;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[test]
fn keyring_removal_and_rollback_preserve_unrelated_file_auth() -> anyhow::Result<()> {
    for backend in [
        AuthKeyringBackendKind::Direct,
        AuthKeyringBackendKind::Secrets,
    ] {
        let home = tempdir()?;
        let keyring: Arc<dyn KeyringStore> = Arc::new(MockKeyringStore::default());
        let auth: AuthDotJson =
            serde_json::from_str(r#"{"auth_mode":"apikey","OPENAI_API_KEY":"keyring-account"}"#)?;
        let storage = create_auth_storage_with_store(
            home.path().to_path_buf(),
            AuthCredentialsStoreMode::Keyring,
            keyring.clone(),
            backend,
        );
        storage.save(&auth)?;
        let unrelated = br#"{"auth_mode":"apikey","OPENAI_API_KEY":"file-account"}"#;
        std::fs::write(home.path().join("auth.json"), unrelated)?;
        let guard = AuthRefreshGuard::acquire(home.path())?;
        assert!(delete_with_store(
            home.path(),
            AuthCredentialsStoreMode::Keyring,
            backend,
            &guard,
            keyring.clone()
        )?);
        assert_eq!(storage.load()?, None);
        assert_eq!(std::fs::read(home.path().join("auth.json"))?, unrelated);
        restore_with_store(
            home.path(),
            &auth,
            AuthCredentialsStoreMode::Keyring,
            backend,
            &guard,
            keyring,
        )?;
        assert_eq!(storage.load()?, Some(auth));
        assert_eq!(std::fs::read(home.path().join("auth.json"))?, unrelated);
    }
    Ok(())
}
