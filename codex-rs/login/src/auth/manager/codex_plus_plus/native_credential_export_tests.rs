use base64::Engine;
use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AutomaticAccountSelection;
use codex_protocol::auth::AuthMode;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::super::super::AuthDotJson;
use super::super::super::AuthKeyringBackendKind;
use super::super::super::AuthManager;
use super::super::super::save_auth;
use super::NativeCredentialSource;
use crate::account::AccountStore;
use crate::account::account_id_for_auth;
use crate::test_support::transport_default_auth_route_config;
use crate::token_data::TokenData;
use crate::token_data::parse_chatgpt_jwt_claims;

fn jwt(payload: serde_json::Value) -> String {
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
    format!("e30.{payload}.sig")
}

fn auth(account_id: &str, access_label: &str) -> AuthDotJson {
    let id_token = jwt(json!({
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account_id,
            "chatgpt_plan_type": "plus"
        }
    }));
    AuthDotJson {
        auth_mode: Some(AuthMode::Chatgpt),
        openai_api_key: None,
        tokens: Some(TokenData {
            id_token: parse_chatgpt_jwt_claims(&id_token).unwrap(),
            access_token: jwt(json!({
                "exp": Utc::now().timestamp() + 3600,
                "label": access_label
            })),
            refresh_token: "refresh-secret".to_string(),
            account_id: Some(account_id.to_string()),
        }),
        last_refresh: Some(Utc::now()),
        agent_identity: None,
        personal_access_token: None,
        bedrock_api_key: None,
        bedrock_access_keys: None,
    }
}

async fn manager(home: &TempDir, selection: AutomaticAccountSelection) -> AuthManager {
    AuthManager::new_with_automatic_account_selection(
        home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        transport_default_auth_route_config(),
        selection,
    )
    .await
}

#[tokio::test]
async fn export_reads_rotated_disk_token_instead_of_cached_auth() {
    let home = TempDir::new().unwrap();
    let first = auth("upstream-a", "old");
    save_auth(
        home.path(),
        &first,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let manager = manager(&home, AutomaticAccountSelection::Disabled).await;
    let rotated = auth("upstream-a", "new");
    save_auth(
        home.path(),
        &rotated,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();

    let snapshot = manager.export_native_credentials().await.unwrap();
    assert_eq!(snapshot.credentials().len(), 1);
    assert_eq!(
        snapshot.selected_source(),
        Some(&NativeCredentialSource::Root(
            account_id_for_auth(&first).unwrap()
        ))
    );
    assert_eq!(
        snapshot.credentials()[0].access_token,
        rotated.tokens.unwrap().access_token
    );
    assert_eq!(snapshot.credentials()[0].upstream_account_id, "upstream-a");
    assert_eq!(snapshot.credentials()[0].plan_type.as_deref(), Some("plus"));
}

#[tokio::test]
async fn imported_marker_exports_once_and_manual_selection_survives() {
    let home = TempDir::new().unwrap();
    let root = auth("upstream-a", "imported");
    save_auth(
        home.path(),
        &root,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let store = AccountStore::new(home.path().to_path_buf());
    let profile = store
        .import_current(
            /*label*/ None,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .unwrap();
    store.set_automation_enabled(&profile.id, false).unwrap();
    let manager = manager(&home, AutomaticAccountSelection::Disabled).await;
    manager
        .activate_imported_account(&profile.id)
        .await
        .unwrap();
    let rotated = auth("upstream-a", "rotated-import");
    save_auth(
        &store.account_home(&profile.id),
        &rotated,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();

    let snapshot = manager.export_native_credentials().await.unwrap();
    assert_eq!(snapshot.credentials().len(), 1);
    assert_eq!(
        snapshot.selected_source(),
        Some(&NativeCredentialSource::Imported(profile.id.clone()))
    );
    assert_eq!(
        snapshot.credentials()[0].access_token,
        rotated.tokens.unwrap().access_token
    );
    assert!(manager.refresh_lock.try_acquire().is_err());
    drop(snapshot);
    assert!(manager.refresh_lock.try_acquire().is_ok());
    assert_eq!(manager.active_account_id(), Some(profile.id.clone()));

    manager.set_forced_chatgpt_workspace_id(Some(vec!["other-workspace".to_string()]));
    let snapshot = manager.export_native_credentials().await.unwrap();
    assert!(snapshot.credentials().is_empty());
    assert_eq!(snapshot.selected_source(), None);
    drop(snapshot);
    assert_eq!(manager.active_account_id(), Some(profile.id.clone()));
    manager.set_forced_chatgpt_workspace_id(None);

    store.record_login_required(&profile.id).unwrap();
    let snapshot = manager.export_native_credentials().await.unwrap();
    assert!(snapshot.credentials().is_empty());
    assert_eq!(snapshot.selected_source(), None);
    drop(snapshot);
    std::fs::remove_file(store.account_home(&profile.id).join("auth.json")).unwrap();
    let snapshot = manager.export_native_credentials().await.unwrap();
    assert!(snapshot.credentials().is_empty());
    assert_eq!(snapshot.selected_source(), None);
    assert_eq!(manager.active_account_id(), Some(profile.id));
}

#[tokio::test]
async fn root_workspace_policy_and_invalid_selected_credentials_are_respected() {
    let home = TempDir::new().unwrap();
    let mut root = auth("upstream-a", "root");
    save_auth(
        home.path(),
        &root,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
    let manager = manager(&home, AutomaticAccountSelection::Disabled).await;
    manager.set_forced_chatgpt_workspace_id(Some(vec!["other-workspace".to_string()]));
    let snapshot = manager.export_native_credentials().await.unwrap();
    assert!(snapshot.credentials().is_empty());
    assert_eq!(snapshot.selected_source(), None);
    drop(snapshot);
    manager.set_forced_chatgpt_workspace_id(None);

    for access_token in ["invalid".to_string(), jwt(json!({"exp": 1}))] {
        root.tokens.as_mut().unwrap().access_token = access_token;
        save_auth(
            home.path(),
            &root,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .unwrap();
        let snapshot = manager.export_native_credentials().await.unwrap();
        assert!(snapshot.credentials().is_empty());
        assert_eq!(snapshot.selected_source(), None);
    }
}

#[tokio::test]
async fn unreadable_index_fails_instead_of_publishing_empty_inventory() {
    let home = TempDir::new().unwrap();
    std::fs::create_dir_all(home.path().join("accounts")).unwrap();
    std::fs::write(home.path().join("accounts/index.json"), "invalid json").unwrap();
    let manager = manager(&home, AutomaticAccountSelection::Disabled).await;

    assert!(manager.export_native_credentials().await.is_err());
    std::fs::remove_file(home.path().join("accounts/index.json")).unwrap();
    std::fs::write(home.path().join("auth.json"), "invalid json").unwrap();
    assert!(manager.export_native_credentials().await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bound_native_recovery_rechecks_disk_before_mutation_and_after_candidate_load() {
    use crate::auth::ImportedAccountSwitchOutcome::NoCandidate;
    use crate::auth::ImportedAccountSwitchOutcome::ReadyToRetry;
    use crate::auth::ImportedAccountSwitchOutcome::RequestSourceChanged;
    use std::collections::HashSet;
    use std::sync::Arc;
    fn save(home: &std::path::Path, auth: &AuthDotJson) {
        save_auth(
            home,
            auth,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .unwrap();
    }
    for (case, operation) in [
        "switch",
        "manual",
        "stale",
        "rotate_during_load",
        "root",
        "same_owner_source",
        "selected_changed",
    ]
    .into_iter()
    .flat_map(|case| ["quota", "auth"].map(move |operation| (case, operation)))
    {
        let home = TempDir::new().unwrap();
        let store = AccountStore::new(home.path().to_path_buf());
        let mut profiles = Vec::new();
        for id in ["a", "b"] {
            save(home.path(), &auth(id, "initial"));
            profiles.push(
                store
                    .import_current(
                        /*label*/ None,
                        AuthCredentialsStoreMode::File,
                        AuthKeyringBackendKind::default(),
                    )
                    .unwrap(),
            );
        }
        let selection = if case == "manual" {
            AutomaticAccountSelection::Disabled
        } else {
            AutomaticAccountSelection::Enabled
        };
        let manager = Arc::new(manager(&home, selection).await);
        manager
            .activate_imported_account(&profiles[0].id)
            .await
            .unwrap();
        if case == "root" {
            save(home.path(), &auth("root", "initial"));
            manager.clear_active_imported_account();
            manager.reload().await;
        }
        if case == "same_owner_source" {
            let auth = serde_json::from_slice(
                &std::fs::read(store.account_home(&profiles[0].id).join("auth.json")).unwrap(),
            )
            .unwrap();
            save(home.path(), &auth);
            manager.clear_active_imported_account();
        }
        let expected = manager
            .export_native_credentials()
            .await
            .unwrap()
            .selected_expectation()
            .unwrap();
        if operation == "auth" {
            manager.record_permanent_refresh_failure_if_unchanged(
                &manager.auth_cached().unwrap(),
                &codex_protocol::auth::RefreshTokenFailedError::new(
                    codex_protocol::auth::RefreshTokenFailedReason::Expired,
                    "synthetic expired refresh",
                ),
            );
        }
        let original_revision = *manager.auth_change_receiver().borrow();
        if case == "same_owner_source" {
            manager
                .activate_imported_account(&profiles[0].id)
                .await
                .unwrap();
            assert_eq!(*manager.auth_change_receiver().borrow(), original_revision);
        }
        if case == "selected_changed" {
            manager
                .activate_imported_account(&profiles[1].id)
                .await
                .unwrap();
        }
        if case == "stale" {
            save(&store.account_home(&profiles[0].id), &auth("a", "rotated"));
        }
        let candidate_guard = (case == "rotate_during_load").then(|| {
            crate::account_lease::AuthRefreshGuard::acquire(&store.account_home(&profiles[1].id))
                .unwrap()
        });
        let resets_at = Utc::now().timestamp() + 3600;
        let task = tokio::spawn({
            let manager = manager.clone();
            async move {
                let mut attempted = HashSet::new();
                let outcome = if operation == "auth" {
                    let mut recovery = manager.unauthorized_recovery();
                    let result = recovery.next_for_native_request(&expected).await;
                    assert_eq!(result.is_ok(), case == "switch", "{case}");
                    assert!(!recovery.has_next());
                    if result.is_ok() {
                        ReadyToRetry
                    } else if matches!(case, "manual" | "root") {
                        NoCandidate
                    } else {
                        RequestSourceChanged
                    }
                } else {
                    manager
                        .switch_after_native_usage_limit(&expected, &mut attempted, Some(resets_at))
                        .await
                        .unwrap()
                };
                (outcome, attempted)
            }
        });
        if candidate_guard.is_some() {
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while {
                    let _index_guard = store.acquire_index_lock().unwrap();
                    let profile = &store.list().unwrap()[0];
                    if operation == "auth" {
                        !profile.login_required
                    } else {
                        profile.usage_limit_resets_at != Some(resets_at)
                    }
                } {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            // This succeeds only after A's file guard was released before waiting on B.
            save(&store.account_home(&profiles[0].id), &auth("a", "rotated"));
            drop(candidate_guard);
        }
        let (outcome, attempted) = task.await.unwrap();
        let switched = case == "switch" || (case == "root" && operation == "quota");
        assert_eq!(
            outcome,
            if switched {
                ReadyToRetry
            } else if case == "manual" || (case == "root" && operation == "auth") {
                NoCandidate
            } else {
                RequestSourceChanged
            },
            "{case}"
        );
        if case == "selected_changed" {
            assert_eq!(manager.active_account_id(), Some(profiles[1].id.clone()));
        } else if case == "root" && !switched {
            assert_eq!(manager.active_account_id(), None);
        } else if !switched {
            assert_eq!(*manager.auth_change_receiver().borrow(), original_revision);
            assert_eq!(manager.active_account_id(), Some(profiles[0].id.clone()));
        } else if case == "switch" {
            assert_eq!(manager.active_account_id(), Some(profiles[1].id.clone()));
        }
        let charged = !matches!(
            case,
            "stale" | "root" | "same_owner_source" | "selected_changed"
        );
        assert_eq!(
            attempted,
            if charged && operation == "quota" {
                HashSet::from([profiles[0].id.to_string()])
            } else {
                HashSet::new()
            }
        );
        assert_eq!(
            store
                .list()
                .unwrap()
                .iter()
                .map(|profile| profile.usage_limit_resets_at)
                .collect::<Vec<_>>(),
            vec![(charged && operation == "quota").then_some(resets_at), None],
            "{case}"
        );
        assert_eq!(
            store
                .list()
                .unwrap()
                .iter()
                .map(|profile| profile.login_required)
                .collect::<Vec<_>>(),
            vec![charged && operation == "auth", false],
            "{case}"
        );
    }
}
