use super::*;
use crate::AuthConfig;
use crate::AuthManager;
use crate::CodexAuth;
use crate::auth::ImportedAccountSwitchOutcome;
use crate::save_auth;
use crate::token_data::IdTokenInfo;
use crate::token_data::TokenData;
use base64::Engine;
use chrono::Utc;
use codex_config::ManagedAuthPolicy;
use codex_config::types::AutomaticAccountSelection;
use codex_protocol::auth::AuthMode;
use codex_protocol::config_types::ForcedLoginMethod;
use pretty_assertions::assert_eq;
use serde::Serialize;
use sha2::Digest;
use sha2::Sha256;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Barrier;
use tempfile::tempdir;

#[test]
fn import_current_keeps_refresh_token_only_in_account_home() {
    let codex_home = tempdir().expect("tempdir");
    let root_auth = test_auth("account-a", "user-a", "a@example.com");
    save_auth(
        codex_home.path(),
        &root_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save root auth");

    let store = AccountStore::new(codex_home.path().to_path_buf());
    let profile = store
        .import_current(
            Some("work".to_string()),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("import account");

    assert_eq!(store.list().expect("list accounts"), vec![profile.clone()]);
    let imported_auth = load_auth_dot_json(
        &store.account_home(&profile.id),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("load account auth");
    assert_eq!(imported_auth, Some(root_auth));
    let root_auth = load_auth_dot_json(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("load root auth")
    .expect("root account marker");
    assert_eq!(
        root_auth.tokens.expect("root tokens").refresh_token,
        String::new()
    );
}

#[test]
fn import_current_uses_email_label_when_label_is_omitted() {
    let codex_home = tempdir().expect("tempdir");
    save_auth(
        codex_home.path(),
        &test_auth("account-a", "user-a", "a@example.com"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save root auth");

    let store = AccountStore::new(codex_home.path().to_path_buf());
    let profile = store
        .import_current(
            None,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("import account");

    assert_eq!(profile.label, "a@example.com");
}

#[test]
fn import_current_reenables_existing_account_profile() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    {
        let _index_guard = store.acquire_index_lock().expect("lock account index");
        assert!(store.disable_all_unlocked().expect("disable accounts"));
    }

    let profile = store
        .import_current(
            Some("first again".to_string()),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("re-import account");

    assert_eq!(
        store.candidates().expect("candidates"),
        vec![AccountCandidate {
            id: profile.id.clone(),
            display_label: "first again".to_string(),
            priority: first.priority,
            enabled: true,
            automation_enabled: true,
            usage_limit_resets_at: None,
            blocked: false,
        }]
    );
    assert!(
        load_auth_dot_json(
            &store.account_home(&profile.id),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("load reimported auth")
        .is_some()
    );
}

#[test]
fn import_current_preserves_login_required_for_root_marker() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let imported = import_test_account(&store, codex_home.path(), "first", "account-a");
    assert!(
        store
            .record_login_required(&imported.id)
            .expect("record login required")
    );

    let profile = store
        .import_current(
            Some("still stale".to_string()),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("re-import root marker");

    assert!(profile.login_required);
    assert!(
        store
            .enabled_file_account_profiles()
            .expect("eligible accounts")
            .is_empty()
    );
}

#[test]
fn concurrent_login_required_updates_are_preserved() {
    let codex_home = tempdir().expect("tempdir");
    let profiles: Vec<_> = (0..16)
        .map(|index| {
            import_test_account(
                &AccountStore::new(codex_home.path().to_path_buf()),
                codex_home.path(),
                &format!("account {index}"),
                &format!("account-{index}"),
            )
        })
        .collect();
    let barrier = Arc::new(Barrier::new(profiles.len()));
    let handles: Vec<_> = profiles
        .iter()
        .map(|profile| {
            let account_id = profile.id.clone();
            let codex_home = codex_home.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                AccountStore::new(codex_home).record_login_required(&account_id)
            })
        })
        .collect();

    for handle in handles {
        assert!(handle.join().expect("update thread").expect("update"));
    }
    assert!(
        AccountStore::new(codex_home.path().to_path_buf())
            .list()
            .expect("accounts")
            .iter()
            .all(|profile| profile.login_required)
    );
}

#[test]
fn login_requirement_is_not_recorded_after_account_auth_is_replaced() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let profile = import_test_account(&store, codex_home.path(), "first", "account-a");
    let account_home = store.account_home(&profile.id);
    let attempted_auth = load_auth_dot_json(
        &account_home,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("load attempted auth")
    .expect("attempted auth");
    let replacement_auth = test_auth("account-a", "replacement-user", "replacement@example.com");
    save_auth(
        &account_home,
        &replacement_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("replace account auth");
    let pending = stage_pending_auth_save(&account_home, &replacement_auth);

    assert!(
        !store
            .record_login_required_if_auth_matches(&profile.id, &attempted_auth)
            .expect("compare account auth")
    );
    assert!(
        store
            .list()
            .expect("accounts")
            .iter()
            .find(|account| account.id == profile.id)
            .is_some_and(|account| !account.login_required)
    );
    assert!(!pending.exists());
    assert_eq!(
        load_auth_dot_json(
            &account_home,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("load replacement auth"),
        Some(replacement_auth)
    );
}

#[test]
fn candidates_include_usage_limit_blocked_state() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");

    assert!(
        store
            .record_usage_limit_resets_at(&first.id, /*resets_at*/ 2_000)
            .expect("record reset")
    );

    assert_eq!(
        store.candidates_at(/*now*/ 1_000).expect("candidates"),
        vec![
            AccountCandidate {
                id: first.id,
                display_label: "first".to_string(),
                priority: first.priority,
                enabled: true,
                automation_enabled: true,
                usage_limit_resets_at: Some(2_000),
                blocked: true,
            },
            AccountCandidate {
                id: second.id,
                display_label: "second".to_string(),
                priority: second.priority,
                enabled: true,
                automation_enabled: true,
                usage_limit_resets_at: None,
                blocked: false,
            },
        ]
    );
}

#[test]
fn account_lease_marks_account_in_use_until_released() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let account = import_test_account(&store, codex_home.path(), "first", "account-a");

    let lease = store
        .try_acquire_lease(&account.id)
        .expect("acquire lease")
        .expect("account should be free");
    assert!(store.account_in_use(&account.id).expect("inspect lease"));

    drop(lease);
    assert!(!store.account_in_use(&account.id).expect("inspect lease"));
}

#[test]
fn apply_imported_account_to_root_auth_switches_root_auth() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let _second = import_test_account(&store, codex_home.path(), "second", "account-b");
    let first_home = store.account_home(&first.id);
    let first_auth = load_auth_dot_json(
        &first_home,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("load first account auth")
    .expect("first account auth");
    let pending = stage_pending_auth_save(&first_home, &first_auth);

    let selected = store
        .apply_imported_account_to_root_auth(
            &first.id,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("select imported account");
    assert!(!pending.exists());

    let root_auth = load_auth_dot_json(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("load root auth")
    .expect("root auth");
    assert_eq!(selected, first);
    let tokens = root_auth.tokens.expect("root tokens");
    assert_eq!(tokens.account_id, Some("account-a".to_string()));
    assert_eq!(tokens.refresh_token, String::new());
}

#[tokio::test]
async fn startup_prefers_imported_account_matching_root_chatgpt_auth() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let _first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");

    let manager = test_auth_manager(codex_home.path()).await;

    assert_eq!(manager.active_account_id(), Some(second.id));
}

#[tokio::test]
async fn startup_does_not_use_root_account_marker_when_index_is_malformed() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let account = import_test_account(&store, codex_home.path(), "first", "account-a");
    std::fs::write(codex_home.path().join("accounts/index.json"), "{")
        .expect("write malformed account index");

    let manager = test_auth_manager(codex_home.path()).await;

    assert_eq!(manager.active_account_id(), None);
    assert_eq!(manager.auth_cached(), None);
    assert!(manager.logout().await.expect("logout"));
    assert!(!codex_home.path().join("auth.json").exists());
    assert!(!store.account_home(&account.id).join("auth.json").exists());
}

#[tokio::test]
async fn live_manager_adopts_imported_account_when_root_becomes_a_marker() {
    let codex_home = tempdir().expect("tempdir");
    save_root_test_auth(codex_home.path(), "account-a");
    let manager = test_auth_manager(codex_home.path()).await;
    assert_eq!(manager.active_account_id(), None);

    let store = AccountStore::new(codex_home.path().to_path_buf());
    let imported = store
        .import_current(
            Some("first".to_string()),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("import account");

    assert!(manager.reload().await);
    assert_eq!(manager.active_account_id(), Some(imported.id));
    assert_eq!(
        manager.auth_cached().and_then(|auth| auth.get_account_id()),
        Some("account-a".to_string())
    );
}

#[tokio::test]
async fn guarded_reload_does_not_commit_a_different_imported_account_source() {
    let codex_home = tempdir().expect("tempdir");
    save_root_test_auth(codex_home.path(), "account-a");
    let live_manager = test_auth_manager(codex_home.path()).await;
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let account_a = import_test_account(&store, codex_home.path(), "first", "account-a");
    let _account_b = import_test_account(&store, codex_home.path(), "second", "account-b");
    store
        .apply_imported_account_to_root_auth(
            &account_a.id,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("select first account");
    let lease_owner = test_auth_manager(codex_home.path()).await;
    assert_eq!(lease_owner.active_account_id(), Some(account_a.id));

    live_manager
        .refresh_token()
        .await
        .expect_err("alternate imported account should fail guarded reload");

    assert_eq!(live_manager.active_account_id(), None);
    assert_eq!(
        live_manager
            .auth_cached()
            .and_then(|auth| auth.get_account_id()),
        Some("account-a".to_string())
    );
}

#[tokio::test]
async fn startup_avoids_account_leased_by_another_manager() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");

    let owner = test_auth_manager(codex_home.path()).await;
    assert_eq!(owner.active_account_id(), Some(second.id));

    let other = test_auth_manager(codex_home.path()).await;
    assert_eq!(other.active_account_id(), Some(first.id));
}

#[tokio::test]
async fn startup_prefers_unblocked_account_over_blocked_root_match() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    store
        .record_usage_limit_resets_at(&second.id, Utc::now().timestamp() + 60)
        .expect("record reset");

    let manager = test_auth_manager(codex_home.path()).await;

    assert_eq!(manager.active_account_id(), Some(first.id));
}

#[tokio::test]
async fn startup_skips_imported_accounts_outside_forced_workspace() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let _first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    save_root_test_auth(codex_home.path(), "account-a");

    let manager =
        test_auth_manager_with_forced_workspace(codex_home.path(), vec!["account-b"]).await;

    assert_eq!(manager.active_account_id(), Some(second.id));
}

#[tokio::test]
async fn startup_keeps_root_api_key_auth_over_imported_accounts() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    import_test_account(&store, codex_home.path(), "first", "account-a");
    save_auth(
        codex_home.path(),
        &api_key_auth(),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save api key auth");

    let manager = test_auth_manager(codex_home.path()).await;

    assert_eq!(manager.active_account_id(), None);
    assert_eq!(manager.auth_mode(), Some(AuthMode::ApiKey));
}

#[tokio::test]
async fn startup_keeps_fresh_root_login_when_imported_profile_requires_login() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let imported = import_test_account(&store, codex_home.path(), "first", "account-a");
    store
        .record_login_required(&imported.id)
        .expect("record login requirement");
    let fresh_auth = test_auth("account-a", "user-account-a", "account-a@example.com");
    save_auth(
        codex_home.path(),
        &fresh_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save fresh root auth");

    let manager = test_auth_manager(codex_home.path()).await;

    assert_eq!(manager.active_account_id(), None);
    assert_eq!(
        manager
            .auth_cached()
            .expect("fresh root auth")
            .get_token_data()
            .expect("fresh root tokens"),
        fresh_auth.tokens.expect("expected root tokens")
    );
}

#[tokio::test]
async fn startup_keeps_fresh_root_login_when_another_imported_account_is_available() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let _healthy = import_test_account(&store, codex_home.path(), "first", "account-a");
    let reauthenticated = import_test_account(&store, codex_home.path(), "second", "account-b");
    store
        .record_login_required(&reauthenticated.id)
        .expect("record login requirement");
    let fresh_auth = test_auth("account-b", "fresh-user-b", "fresh-b@example.com");
    save_auth(
        codex_home.path(),
        &fresh_auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save fresh root auth");

    let manager = test_auth_manager(codex_home.path()).await;

    assert_eq!(manager.active_account_id(), None);
    assert_eq!(
        manager
            .auth_cached()
            .expect("fresh root auth")
            .get_token_data()
            .expect("fresh root tokens"),
        fresh_auth.tokens.expect("expected root tokens")
    );
}

#[tokio::test]
async fn startup_honors_forced_workspace_over_fresh_root_reauthentication() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let forced = import_test_account(&store, codex_home.path(), "first", "account-a");
    let reauthenticated = import_test_account(&store, codex_home.path(), "second", "account-b");
    store
        .record_login_required(&reauthenticated.id)
        .expect("record login requirement");
    save_auth(
        codex_home.path(),
        &test_auth("account-b", "fresh-user-b", "fresh-b@example.com"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save fresh root auth");

    let manager =
        test_auth_manager_with_forced_workspace(codex_home.path(), vec!["account-a"]).await;

    assert_eq!(manager.active_account_id(), Some(forced.id));
    assert_eq!(
        manager
            .auth_cached()
            .expect("forced imported auth")
            .get_account_id(),
        Some("account-a".to_string())
    );
}

#[tokio::test]
async fn startup_rejects_disallowed_root_reauthentication_without_forced_account() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let reauthenticated = import_test_account(&store, codex_home.path(), "first", "account-a");
    store
        .record_login_required(&reauthenticated.id)
        .expect("record login requirement");
    save_auth(
        codex_home.path(),
        &test_auth("account-a", "fresh-user-a", "fresh-a@example.com"),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save fresh root auth");

    let manager =
        test_auth_manager_with_forced_workspace(codex_home.path(), vec!["account-b"]).await;

    assert_eq!(manager.active_account_id(), None);
    assert_eq!(manager.auth_cached(), None);
}

#[tokio::test]
async fn direct_marker_auth_resolution_honors_forced_workspace() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let _second = import_test_account(&store, codex_home.path(), "second", "account-b");
    store
        .apply_imported_account_to_root_auth(
            &first.id,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("select first account");
    let forced_workspaces = vec!["account-b".to_string()];

    let auth = CodexAuth::from_auth_storage(
        codex_home.path(),
        AuthCredentialsStoreMode::File,
        Some(&forced_workspaces),
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        &crate::test_support::transport_default_auth_route_config(),
    )
    .await
    .expect("load marker auth")
    .expect("matching imported auth");

    assert_eq!(auth.get_account_id(), Some("account-b".to_string()));
}

#[tokio::test]
async fn switch_to_next_imported_account_skips_attempted_local_account_ids() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    save_root_test_auth(codex_home.path(), "account-a");

    let manager = test_auth_manager(codex_home.path()).await;
    assert_eq!(manager.active_account_id(), Some(first.id.clone()));

    let attempted = HashSet::from([first.id.to_string()]);
    assert_eq!(
        manager.switch_to_next_imported_account(&attempted).await,
        ImportedAccountSwitchOutcome::ReadyToRetry
    );
    assert_eq!(manager.active_account_id(), Some(second.id.clone()));

    let attempted = HashSet::from([first.id.to_string(), second.id.to_string()]);
    assert_eq!(
        manager.switch_to_next_imported_account(&attempted).await,
        ImportedAccountSwitchOutcome::NoCandidate
    );
    assert_eq!(manager.active_account_id(), Some(second.id));
}

#[tokio::test]
async fn managed_api_only_policy_blocks_imported_account_switches() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    save_root_test_auth(codex_home.path(), "account-a");
    let managed_auth_policy = ManagedAuthPolicy {
        allowed_login_methods: Some(vec![ForcedLoginMethod::Api]),
        ..Default::default()
    };
    let manager = AuthManager::shared_from_auth_config(
        AuthConfig {
            codex_home: codex_home.path().to_path_buf(),
            auth_credentials_store_mode: AuthCredentialsStoreMode::File,
            keyring_backend_kind: AuthKeyringBackendKind::default(),
            automatic_account_selection: AutomaticAccountSelection::Enabled,
            forced_login_method: None,
            chatgpt_base_url: None,
            forced_chatgpt_workspace_id: None,
            managed_auth_policy,
            auth_route_config: crate::test_support::transport_default_auth_route_config(),
        },
        /*enable_codex_api_key_env*/ false,
    )
    .await
    .expect("auth manager should initialize");

    assert_eq!(manager.active_account_id(), None);
    assert_eq!(
        manager
            .activate_imported_account(&second.id)
            .await
            .expect_err("manual activation must honor managed auth policy")
            .kind(),
        std::io::ErrorKind::PermissionDenied
    );
    assert_eq!(
        manager
            .switch_to_next_imported_account(&HashSet::from([first.id.to_string()]))
            .await,
        ImportedAccountSwitchOutcome::NoCandidate
    );
}

#[tokio::test]
async fn disabled_automatic_selection_keeps_login_required_account() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    store
        .record_login_required(&second.id)
        .expect("record login requirement");
    let manager = AuthManager::new_with_automatic_account_selection(
        codex_home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
        AutomaticAccountSelection::Disabled,
    )
    .await;
    save_root_test_auth(codex_home.path(), "account-b");
    assert!(manager.reload().await);
    assert_eq!(manager.active_account_id(), Some(second.id.clone()));
}
#[tokio::test]
async fn switch_to_next_imported_account_prefers_unblocked_accounts() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    let third = import_test_account(&store, codex_home.path(), "third", "account-c");
    save_root_test_auth(codex_home.path(), "account-a");
    store
        .record_usage_limit_resets_at(&second.id, Utc::now().timestamp() + 60)
        .expect("record reset");

    let manager = test_auth_manager(codex_home.path()).await;

    let attempted = HashSet::from([first.id.to_string()]);
    assert_eq!(
        manager.switch_to_next_imported_account(&attempted).await,
        ImportedAccountSwitchOutcome::ReadyToRetry
    );
    assert_eq!(manager.active_account_id(), Some(third.id));
}

#[tokio::test]
async fn switch_to_next_imported_account_skips_automation_disabled_accounts() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    let third = import_test_account(&store, codex_home.path(), "third", "account-c");
    store
        .set_automation_enabled(&second.id, /*automation_enabled*/ false)
        .expect("disable second automation");
    save_root_test_auth(codex_home.path(), "account-a");
    let manager = test_auth_manager(codex_home.path()).await;
    assert_eq!(manager.active_account_id(), Some(first.id));
    manager
        .activate_imported_account(&second.id)
        .await
        .expect("activate second manually");

    assert_eq!(
        manager
            .switch_to_next_imported_account(&HashSet::from([second.id.to_string()]))
            .await,
        ImportedAccountSwitchOutcome::ReadyToRetry
    );
    assert_eq!(manager.active_account_id(), Some(third.id));
}

#[tokio::test]
async fn switch_to_next_imported_account_selects_earliest_known_reset() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    let third = import_test_account(&store, codex_home.path(), "third", "account-c");
    save_root_test_auth(codex_home.path(), "account-a");
    let now = Utc::now().timestamp();
    store
        .record_usage_limit_resets_at(&second.id, now + 120)
        .expect("record second reset");
    store
        .record_usage_limit_resets_at(&third.id, now + 60)
        .expect("record third reset");
    let manager = test_auth_manager(codex_home.path()).await;

    let attempted = HashSet::from([first.id.to_string()]);
    assert_eq!(
        manager.switch_to_next_imported_account(&attempted).await,
        ImportedAccountSwitchOutcome::SelectedBlockedUntil {
            resets_at: now + 60
        }
    );
    assert_eq!(manager.active_account_id(), Some(third.id));
}

#[tokio::test]
async fn switch_to_next_imported_account_preserves_order_for_reset_ties() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    let third = import_test_account(&store, codex_home.path(), "third", "account-c");
    save_root_test_auth(codex_home.path(), "account-a");
    let resets_at = Utc::now().timestamp() + 60;
    store
        .record_usage_limit_resets_at(&second.id, resets_at)
        .expect("record second reset");
    store
        .record_usage_limit_resets_at(&third.id, resets_at)
        .expect("record third reset");
    let manager = test_auth_manager(codex_home.path()).await;

    let attempted = HashSet::from([first.id.to_string()]);
    assert_eq!(
        manager.switch_to_next_imported_account(&attempted).await,
        ImportedAccountSwitchOutcome::SelectedBlockedUntil { resets_at }
    );
    assert_eq!(manager.active_account_id(), Some(second.id));
}

#[tokio::test]
async fn switch_to_next_imported_account_prefers_free_blocked_account() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    let third = import_test_account(&store, codex_home.path(), "third", "account-c");
    save_root_test_auth(codex_home.path(), "account-a");
    let now = Utc::now().timestamp();
    store
        .record_usage_limit_resets_at(&second.id, now + 60)
        .expect("record second reset");
    store
        .record_usage_limit_resets_at(&third.id, now + 120)
        .expect("record third reset");
    let _second_lease = store
        .try_acquire_lease(&second.id)
        .expect("acquire second lease")
        .expect("second account should initially be free");
    let manager = test_auth_manager(codex_home.path()).await;

    let attempted = HashSet::from([first.id.to_string()]);
    assert_eq!(
        manager.switch_to_next_imported_account(&attempted).await,
        ImportedAccountSwitchOutcome::SelectedBlockedUntil {
            resets_at: now + 120
        }
    );
    assert_eq!(manager.active_account_id(), Some(third.id));
}

#[tokio::test]
async fn activate_imported_account_respects_requested_blocked_in_use_account() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    store
        .record_usage_limit_resets_at(&second.id, Utc::now().timestamp() + 60)
        .expect("record reset");
    let _lease = store
        .try_acquire_lease(&second.id)
        .expect("acquire lease")
        .expect("selected account should initially be free");
    save_root_test_auth(codex_home.path(), "account-a");
    let manager = test_auth_manager(codex_home.path()).await;
    assert_eq!(manager.active_account_id(), Some(first.id));

    manager
        .activate_imported_account(&second.id)
        .await
        .expect("activate account");

    assert_eq!(manager.active_account_id(), Some(second.id));
    assert_eq!(
        manager.auth_cached().and_then(|auth| auth.get_account_id()),
        Some("account-b".to_string())
    );
}

#[tokio::test]
async fn reactivating_current_imported_account_preserves_its_lease() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let account = import_test_account(&store, codex_home.path(), "first", "account-a");
    save_root_test_auth(codex_home.path(), "account-a");
    let manager = test_auth_manager(codex_home.path()).await;
    assert_eq!(manager.active_account_id(), Some(account.id.clone()));
    assert!(store.account_in_use(&account.id).expect("check lease"));

    manager
        .activate_imported_account(&account.id)
        .await
        .expect("reactivate account");

    assert_eq!(manager.active_account_id(), Some(account.id.clone()));
    assert!(store.account_in_use(&account.id).expect("check lease"));
}

#[tokio::test]
async fn logout_clears_imported_accounts_that_startup_would_select() {
    let codex_home = tempdir().expect("tempdir");
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let first = import_test_account(&store, codex_home.path(), "first", "account-a");
    let second = import_test_account(&store, codex_home.path(), "second", "account-b");
    store
        .record_login_required(&second.id)
        .expect("record login requirement");
    save_root_test_auth(codex_home.path(), "account-a");
    let manager = test_auth_manager(codex_home.path()).await;
    assert_eq!(manager.active_account_id(), Some(first.id.clone()));
    let reset_lease = store
        .acquire_reset_mutation_lease(&second.id)
        .expect("acquire reset lease");
    let logout_manager = Arc::clone(&manager);
    let logout = tokio::spawn(async move { logout_manager.logout().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while store.list().expect("list accounts")[0].enabled {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("account disabled");
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(
        store
            .try_acquire_reset_mutation_lease(&first.id)
            .expect("inspect reset lease")
            .is_none()
    );
    assert!(store.account_home(&first.id).join("auth.json").is_file());
    drop(reset_lease);
    tokio::time::timeout(std::time::Duration::from_secs(5), logout)
        .await
        .expect("logout completed")
        .expect("logout task")
        .expect("logout");

    assert_eq!(manager.active_account_id(), None);
    assert!(manager.auth_cached().is_none());
    assert_eq!(
        load_auth_dot_json(
            codex_home.path(),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("load root auth"),
        None
    );
    assert_eq!(
        load_auth_dot_json(
            &store.account_home(&first.id),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("load first account auth"),
        None
    );
    assert_eq!(
        load_auth_dot_json(
            &store.account_home(&second.id),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("load second account auth"),
        None
    );
    assert_eq!(
        store.candidates().expect("candidates"),
        vec![
            AccountCandidate {
                id: first.id.clone(),
                display_label: "first".to_string(),
                priority: first.priority,
                enabled: false,
                automation_enabled: true,
                usage_limit_resets_at: None,
                blocked: false,
            },
            AccountCandidate {
                id: second.id.clone(),
                display_label: "second".to_string(),
                priority: second.priority,
                enabled: false,
                automation_enabled: true,
                usage_limit_resets_at: None,
                blocked: false,
            },
        ]
    );
    let restarted = test_auth_manager(codex_home.path()).await;
    assert_eq!(restarted.active_account_id(), None);
    assert_eq!(restarted.auth_mode(), None);
}

pub(super) async fn test_auth_manager(codex_home: &std::path::Path) -> std::sync::Arc<AuthManager> {
    test_auth_manager_with_forced_workspace(codex_home, Vec::new()).await
}

async fn test_auth_manager_with_forced_workspace(
    codex_home: &std::path::Path,
    forced_workspace_ids: Vec<&str>,
) -> std::sync::Arc<AuthManager> {
    let forced_chatgpt_workspace_id = if forced_workspace_ids.is_empty() {
        None
    } else {
        Some(
            forced_workspace_ids
                .into_iter()
                .map(str::to_string)
                .collect(),
        )
    };
    AuthManager::shared(
        codex_home.to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        forced_chatgpt_workspace_id,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        crate::test_support::transport_default_auth_route_config(),
    )
    .await
}

pub(super) fn import_test_account(
    store: &AccountStore,
    codex_home: &std::path::Path,
    label: &str,
    account_id: &str,
) -> AccountProfile {
    save_root_test_auth(codex_home, account_id);
    store
        .import_current(
            Some(label.to_string()),
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .expect("import account")
}

fn save_root_test_auth(codex_home: &std::path::Path, account_id: &str) {
    save_auth(
        codex_home,
        &test_auth(
            account_id,
            &format!("user-{account_id}"),
            &format!("{account_id}@example.com"),
        ),
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save root auth");
}

fn stage_pending_auth_save(auth_home: &std::path::Path, auth: &AuthDotJson) -> std::path::PathBuf {
    let bytes = serde_json::to_vec(auth).expect("serialize auth fingerprint");
    let fingerprint = format!("{:x}", Sha256::digest(bytes));
    let short = &fingerprint[..16];
    let pending = auth_home.join(format!(".codex-auth-{short}-{short}"));
    std::fs::write(
        &pending,
        serde_json::to_vec_pretty(auth).expect("serialize pending auth"),
    )
    .expect("write pending auth");
    pending
}

fn test_auth(account_id: &str, user_id: &str, email: &str) -> AuthDotJson {
    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: IdTokenInfo {
                email: Some(email.to_string()),
                chatgpt_user_id: Some(user_id.to_string()),
                chatgpt_account_id: Some(account_id.to_string()),
                raw_jwt: fake_jwt(account_id, user_id, email),
                ..IdTokenInfo::default()
            },
            access_token: format!("access-{account_id}"),
            refresh_token: format!("refresh-{account_id}"),
            oauth_provenance: None,
            login_owner_id: None,
            account_id: Some(account_id.to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

fn api_key_auth() -> AuthDotJson {
    AuthDotJson {
        auth_mode: Some(AuthMode::ApiKey),
        openai_api_key: Some("test-api-key".to_string()),
        tokens: None,
        last_refresh: None,
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

fn fake_jwt(account_id: &str, user_id: &str, email: &str) -> String {
    #[derive(Serialize)]
    struct Header {
        alg: &'static str,
        typ: &'static str,
    }

    let header = Header {
        alg: "none",
        typ: "JWT",
    };
    let payload = serde_json::json!({
        "email": email,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_user_id": user_id
        }
    });
    let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&header).expect("serialize header"));
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(serde_json::to_vec(&payload).expect("serialize payload"));
    let signature = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b"sig");
    format!("{header}.{payload}.{signature}")
}
