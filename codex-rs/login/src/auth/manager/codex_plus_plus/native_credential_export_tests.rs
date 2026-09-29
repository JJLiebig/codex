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

async fn manager(home: &TempDir) -> AuthManager {
    AuthManager::new_with_automatic_account_selection(
        home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        transport_default_auth_route_config(),
        AutomaticAccountSelection::Disabled,
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
    let manager = manager(&home).await;
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
    let manager = manager(&home).await;
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
    let manager = manager(&home).await;
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
    let manager = manager(&home).await;

    assert!(manager.export_native_credentials().await.is_err());
    std::fs::remove_file(home.path().join("accounts/index.json")).unwrap();
    std::fs::write(home.path().join("auth.json"), "invalid json").unwrap();
    assert!(manager.export_native_credentials().await.is_err());
}
