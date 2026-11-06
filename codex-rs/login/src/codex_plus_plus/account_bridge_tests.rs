use super::super::tests::import_test_account;
use super::*;
use crate::account_lease::AccountLease;
use crate::load_auth_dot_json;
use crate::save_auth;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::time::Duration;
use tempfile::tempdir;

const FILE: AuthCredentialsStoreMode = AuthCredentialsStoreMode::File;

fn load_auth(auth_home: &Path) -> Option<AuthDotJson> {
    load_auth_dot_json(auth_home, FILE, Default::default()).expect("load auth")
}

fn save_file_auth(auth_home: &Path, auth: &AuthDotJson) {
    save_auth(auth_home, auth, FILE, Default::default()).expect("save auth");
}

fn export(store: &AccountStore) -> AccountHandoffOutcome {
    store
        .export_selected_account_to_root_auth(FILE, Default::default())
        .expect("export selected account")
}

fn reconcile(store: &AccountStore) -> AccountHandoffOutcome {
    store
        .reconcile_root_auth_to_matching_account(FILE, Default::default())
        .expect("reconcile root auth")
}

type AuthStates = Vec<Option<AuthDotJson>>;
type Snapshot = (Vec<AccountProfile>, AuthStates, Option<AuthDotJson>);

fn snapshot(store: &AccountStore) -> Snapshot {
    let profiles = store.list().expect("list accounts");
    let account_auth = profiles
        .iter()
        .map(|profile| load_auth(&store.account_home(&profile.id)))
        .collect();
    (profiles, account_auth, load_auth(&store.codex_home))
}

fn assert_no_handoff(store: &AccountStore, operation: fn(&AccountStore) -> AccountHandoffOutcome) {
    let before = snapshot(store);
    assert_eq!(operation(store), AccountHandoffOutcome::NoHandoff);
    assert_eq!(snapshot(store), before);
}

fn update_profile(
    store: &AccountStore,
    account_id: &AccountId,
    update: impl FnOnce(&mut AccountProfile),
) -> AccountProfile {
    let _guard = store.acquire_index_lock().expect("lock account index");
    let mut index = store.load_index().expect("load account index");
    let profile = index
        .accounts
        .iter_mut()
        .find(|profile| &profile.id == account_id)
        .unwrap();
    update(profile);
    let profile = profile.clone();
    store.save_index(&index).expect("save account index");
    profile
}

fn auth_version(mut auth: AuthDotJson, name: &str, refresh_offset: Option<i64>) -> AuthDotJson {
    let tokens = auth.tokens.as_mut().expect("ChatGPT tokens");
    tokens.access_token = format!("{name}-access");
    tokens.refresh_token = format!("{name}-refresh");
    auth.last_refresh = refresh_offset.and_then(|seconds| {
        auth.last_refresh
            .map(|time| time + chrono::Duration::seconds(seconds))
    });
    auth
}

fn root_marker(mut auth: AuthDotJson) -> AuthDotJson {
    auth.tokens.as_mut().unwrap().refresh_token.clear();
    auth
}

#[test]
fn selected_profile_round_trips_without_losing_metadata() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    let first = update_profile(&store, &first.id, |profile| {
        profile.label = "preserved".to_string();
        profile.automation_enabled = false;
        profile.usage_limit_resets_at = Some(1234);
    });
    store
        .apply_imported_account_to_root_auth(&first.id, FILE, Default::default())
        .expect("select first account");
    let first_auth = load_auth(&store.account_home(&first.id)).expect("first auth");
    let before = snapshot(&store);
    assert_eq!(
        export(&store),
        AccountHandoffOutcome::Completed(first.clone())
    );
    let exported = snapshot(&store);
    assert_eq!(exported.0, before.0);
    assert_eq!(exported.2, Some(first_auth.clone()));
    let refreshed = auth_version(first_auth, "refreshed", Some(1));
    save_file_auth(codex_home.path(), &refreshed);
    let mut expected = update_profile(&store, &first.id, |profile| profile.login_required = true);
    expected.login_required = false;
    assert_eq!(
        reconcile(&store),
        AccountHandoffOutcome::Completed(expected.clone())
    );
    assert_eq!(snapshot(&store).0[0], expected);
    assert_no_handoff(&store, reconcile);
    let preserved = snapshot(&store);
    let second_auth = load_auth(&store.account_home(&second.id)).expect("second auth");
    store
        .apply_imported_account_to_root_auth(&second.id, FILE, Default::default())
        .expect("select second account");
    assert_eq!(export(&store), AccountHandoffOutcome::Completed(second));
    assert_eq!(
        snapshot(&store),
        (preserved.0, preserved.1, Some(second_auth))
    );
}

#[test]
fn reconciliation_uses_the_freshest_matching_auth() {
    for (root_refresh_offset, preserve_profile) in [(Some(3), false), (Some(1), true), (None, true)]
    {
        let codex_home = tempdir().expect("tempdir");
        let store = AccountStore::new(codex_home.path().to_path_buf());
        let mut expected_profile =
            import_test_account(&store, codex_home.path(), "first", "account-a");
        let initial_auth = load_auth(&store.account_home(&expected_profile.id)).expect("auth");
        let profile_auth = auth_version(initial_auth.clone(), "profile", Some(2));
        let root_auth = auth_version(initial_auth, "root", root_refresh_offset);
        save_file_auth(&store.account_home(&expected_profile.id), &profile_auth);
        save_file_auth(codex_home.path(), &root_auth);
        update_profile(&store, &expected_profile.id, |profile| {
            profile.login_required = true
        });
        expected_profile.login_required = false;
        let reconciled_auth = if preserve_profile {
            profile_auth
        } else {
            root_auth
        };
        let outcome = reconcile(&store);
        assert!(!matches!(outcome, AccountHandoffOutcome::NoHandoff));
        let preserved = matches!(outcome, AccountHandoffOutcome::PreservedNewerProfile(_));
        assert_eq!(preserved, preserve_profile);
        assert_eq!(
            snapshot(&store),
            (
                vec![expected_profile],
                vec![Some(reconciled_auth.clone())],
                Some(root_marker(reconciled_auth)),
            )
        );
    }
}

#[test]
fn handoff_refuses_disabled_unusable_and_unrelated_auth() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    let second_home = store.account_home(&second.id);
    let second_auth = load_auth(&second_home).expect("second auth");
    let before = snapshot(&store);
    assert_eq!(
        store
            .export_selected_account_to_root_auth(
                AuthCredentialsStoreMode::Ephemeral,
                Default::default(),
            )
            .expect("ephemeral export"),
        AccountHandoffOutcome::UnavailableForEphemeralStore
    );
    assert_eq!(
        store
            .reconcile_root_auth_to_matching_account(
                AuthCredentialsStoreMode::Ephemeral,
                Default::default(),
            )
            .expect("ephemeral reconciliation"),
        AccountHandoffOutcome::UnavailableForEphemeralStore
    );
    assert_eq!(snapshot(&store), before);
    update_profile(&store, &second.id, |profile| profile.enabled = false);
    assert_no_handoff(&store, export);
    update_profile(&store, &second.id, |profile| profile.enabled = true);
    save_file_auth(&second_home, &root_marker(second_auth.clone()));
    assert_no_handoff(&store, export);
    save_file_auth(&second_home, &second_auth);
    let mut unrelated = load_auth(&store.account_home(&first.id)).expect("first auth");
    unrelated.tokens.as_mut().unwrap().account_id = Some("unrelated".into());
    save_file_auth(codex_home.path(), &unrelated);
    assert_no_handoff(&store, reconcile);
    save_file_auth(codex_home.path(), &second_auth);
    update_profile(&store, &second.id, |profile| profile.enabled = false);
    assert_no_handoff(&store, reconcile);
}

#[test]
fn reconcile_rolls_back_when_the_root_marker_write_fails() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let imported = import_test_account(&store, codex_home.path(), "first", "account-a");
    update_profile(&store, &imported.id, |profile| {
        profile.login_required = true
    });
    let account_home = store.account_home(&imported.id);
    let previous_auth = load_auth(&account_home).expect("account auth");
    let refreshed = auth_version(previous_auth, "refreshed", Some(1));
    save_file_auth(codex_home.path(), &refreshed);
    let before = snapshot(&store);
    let index_guard = store.acquire_index_lock().expect("hold account index");
    let worker_store = store.clone();
    let worker = std::thread::spawn(move || {
        worker_store.reconcile_root_auth_to_matching_account(FILE, Default::default())
    });
    wait_until_locked(&account_home.join(".auth-refresh.lock"));
    let drift = auth_version(refreshed, "drift", Some(2));
    let bytes = serde_json::to_vec_pretty(&drift).expect("serialize drifted root auth");
    std::fs::write(codex_home.path().join("auth.json"), bytes).expect("drift root auth");
    drop(index_guard);
    assert!(worker.join().expect("reconciliation thread").is_err());
    let mut expected = before;
    expected.2 = Some(drift);
    assert_eq!(snapshot(&store), expected);
}

#[test]
fn export_refuses_root_drift_without_touching_account_state() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let imported = import_test_account(&store, codex_home.path(), "first", "account-a");
    store
        .apply_imported_account_to_root_auth(&imported.id, FILE, Default::default())
        .expect("select account");
    let before = snapshot(&store);
    let index_guard = store.acquire_index_lock().expect("hold account index");
    let worker_store = store.clone();
    let worker = std::thread::spawn(move || {
        worker_store.export_selected_account_to_root_auth(FILE, Default::default())
    });
    wait_until_locked(&store.account_home(&imported.id).join(".auth-refresh.lock"));
    let marker = load_auth(codex_home.path()).expect("root marker");
    let drift = auth_version(marker, "drift", Some(1));
    let bytes = serde_json::to_vec_pretty(&drift).expect("serialize drifted root auth");
    std::fs::write(codex_home.path().join("auth.json"), bytes).expect("drift root auth");
    drop(index_guard);
    let err = worker
        .join()
        .expect("export thread")
        .expect_err("export should reject root drift");
    assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    let mut expected = before;
    expected.2 = Some(drift);
    assert_eq!(snapshot(&store), expected);
}

fn wait_until_locked(lock_path: &Path) {
    for _ in 0..5000 {
        if AccountLease::try_acquire(lock_path)
            .expect("inspect auth guard")
            .is_none()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("reconciliation did not acquire the account auth guard");
}
