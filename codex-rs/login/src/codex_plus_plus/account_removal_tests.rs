use super::*;
use crate::account::tests::import_test_account;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[test]
fn remove_only_selected_account_and_matching_root_auth() {
    for remove_current in [false, true] {
        let home = tempdir().unwrap();
        let store = AccountStore::new(home.path().to_path_buf());
        let first = import_test_account(&store, home.path(), "first", "account-a");
        let second = import_test_account(&store, home.path(), "second", "account-b");
        let (removed, kept) = if remove_current {
            (second, first)
        } else {
            (first, second)
        };
        let kept_auth = std::fs::read(store.account_home(&kept.id).join("auth.json")).unwrap();
        let root_auth = std::fs::read(home.path().join("auth.json")).unwrap();
        assert!(
            store
                .remove(
                    &removed.id,
                    AuthCredentialsStoreMode::File,
                    AuthKeyringBackendKind::default()
                )
                .unwrap()
        );
        assert_eq!(store.list().unwrap(), vec![kept.clone()]);
        assert!(!store.account_home(&removed.id).join("auth.json").exists());
        assert_eq!(
            std::fs::read(store.account_home(&kept.id).join("auth.json")).unwrap(),
            kept_auth
        );
        if remove_current {
            assert!(!home.path().join("auth.json").exists());
        } else {
            assert_eq!(
                std::fs::read(home.path().join("auth.json")).unwrap(),
                root_auth
            );
        }
        assert!(
            !store
                .remove(
                    &removed.id,
                    AuthCredentialsStoreMode::File,
                    AuthKeyringBackendKind::default()
                )
                .unwrap()
        );
    }
}

#[test]
fn remove_disabled_account_without_credentials_preserves_api_root() {
    let home = tempdir().unwrap();
    let store = AccountStore::new(home.path().to_path_buf());
    let profile = import_test_account(&store, home.path(), "first", "account-a");
    store.disable_all_unlocked().unwrap();
    std::fs::remove_file(store.account_home(&profile.id).join("auth.json")).unwrap();
    crate::login_with_api_key(
        home.path(),
        "test-key",
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let root_auth = std::fs::read(home.path().join("auth.json")).unwrap();
    assert!(
        store
            .remove(
                &profile.id,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default()
            )
            .unwrap()
    );
    assert!(store.list().unwrap().is_empty());
    assert_eq!(
        std::fs::read(home.path().join("auth.json")).unwrap(),
        root_auth
    );
}

#[test]
fn remove_busy_account_preserves_profile_and_credentials() {
    let home = tempdir().unwrap();
    let store = AccountStore::new(home.path().to_path_buf());
    let profile = import_test_account(&store, home.path(), "first", "account-a");
    let account_auth = std::fs::read(store.account_home(&profile.id).join("auth.json")).unwrap();
    let root_auth = std::fs::read(home.path().join("auth.json")).unwrap();
    let lease = store.try_acquire_lease(&profile.id).unwrap().unwrap();
    assert_eq!(
        store
            .remove(
                &profile.id,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default()
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(lease);
    let _reset_lease = store.acquire_reset_mutation_lease(&profile.id).unwrap();
    assert_eq!(
        store
            .remove(
                &profile.id,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default()
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    assert_eq!(store.list().unwrap(), vec![profile.clone()]);
    assert_eq!(
        std::fs::read(store.account_home(&profile.id).join("auth.json")).unwrap(),
        account_auth
    );
    assert_eq!(
        std::fs::read(home.path().join("auth.json")).unwrap(),
        root_auth
    );
}

#[test]
fn failed_index_save_restores_account_and_root_credentials() {
    let home = tempdir().unwrap();
    let store = AccountStore::new(home.path().to_path_buf());
    let profile = import_test_account(&store, home.path(), "first", "account-a");
    let account_auth = std::fs::read(store.account_home(&profile.id).join("auth.json")).unwrap();
    let root_auth = std::fs::read(home.path().join("auth.json")).unwrap();
    std::fs::create_dir(home.path().join("accounts/index.json.tmp")).unwrap();
    assert!(
        store
            .remove(
                &profile.id,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default()
            )
            .is_err()
    );
    assert_eq!(store.list().unwrap(), vec![profile.clone()]);
    assert_eq!(
        std::fs::read(store.account_home(&profile.id).join("auth.json")).unwrap(),
        account_auth
    );
    assert_eq!(
        std::fs::read(home.path().join("auth.json")).unwrap(),
        root_auth
    );
}
