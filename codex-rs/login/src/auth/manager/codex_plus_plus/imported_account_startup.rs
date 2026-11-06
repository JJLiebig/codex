//! Startup loading and workspace filtering for imported Codex++ accounts.

use std::path::Path;
use std::path::PathBuf;

use chrono::Utc;
use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AutomaticAccountSelection;

use super::super::CodexAuth;
use super::super::load_auth_from_storage;
use crate::account::AccountId;
use crate::account::AccountProfile;
use crate::account::AccountStore;
use crate::account::account_id_for_auth;
use crate::auth::storage::AuthKeyringBackendKind;
use crate::outbound_proxy::AuthRouteConfig;

pub(in crate::auth::manager) async fn load_initial_imported_account_auth(
    codex_home: &Path,
    root_auth: Option<&CodexAuth>,
    automatic_account_selection: AutomaticAccountSelection,
    forced_chatgpt_workspace_id: Option<&[String]>,
    chatgpt_base_url: Option<&str>,
    agent_identity_authapi_base_url: Option<&str>,
    auth_route_config: &AuthRouteConfig,
) -> Option<(AccountId, PathBuf, CodexAuth)> {
    let store = AccountStore::new(codex_home.to_path_buf());
    let accounts: Vec<_> = store
        .enabled_file_account_profiles()
        .unwrap_or_default()
        .into_iter()
        .map(|(account, account_home)| {
            let in_use = store.account_in_use(&account.id).unwrap_or(false);
            (account, account_home, in_use)
        })
        .collect();

    let now = Utc::now().timestamp();
    if let Some(root_account_id) = root_auth
        .and_then(CodexAuth::get_current_auth_json)
        .and_then(|auth| account_id_for_auth(&auth).ok())
    {
        for (account, account_home, in_use) in &accounts {
            if account.id != root_account_id
                || (automatic_account_selection == AutomaticAccountSelection::Enabled
                    && (*in_use || imported_account_blocked(account, now)))
            {
                continue;
            }
            if let Some(auth) = load_imported_account_auth(
                account_home,
                forced_chatgpt_workspace_id,
                chatgpt_base_url,
                agent_identity_authapi_base_url,
                auth_route_config,
            )
            .await
            {
                return Some((account.id.clone(), account_home.clone(), auth));
            }
        }
    }

    if automatic_account_selection == AutomaticAccountSelection::Disabled {
        return None;
    }
    for blocked in [false, true] {
        for in_use in [false, true] {
            for (account, account_home, account_in_use) in &accounts {
                if !account.automation_enabled
                    || imported_account_blocked(account, now) != blocked
                    || *account_in_use != in_use
                {
                    continue;
                }
                if let Some(auth) = load_imported_account_auth(
                    account_home,
                    forced_chatgpt_workspace_id,
                    chatgpt_base_url,
                    agent_identity_authapi_base_url,
                    auth_route_config,
                )
                .await
                {
                    return Some((account.id.clone(), account_home.clone(), auth));
                }
            }
        }
    }

    None
}

pub(in crate::auth::manager) fn imported_account_blocked(
    account: &AccountProfile,
    now: i64,
) -> bool {
    account
        .usage_limit_resets_at
        .is_some_and(|resets_at| resets_at > now)
}

pub(in crate::auth::manager) async fn load_imported_account_auth(
    account_home: &Path,
    forced_chatgpt_workspace_id: Option<&[String]>,
    chatgpt_base_url: Option<&str>,
    agent_identity_authapi_base_url: Option<&str>,
    auth_route_config: &AuthRouteConfig,
) -> Option<CodexAuth> {
    load_auth_from_storage(
        account_home,
        AuthCredentialsStoreMode::File,
        /*allowed_login_methods*/ None,
        forced_chatgpt_workspace_id,
        chatgpt_base_url,
        AuthKeyringBackendKind::default(),
        agent_identity_authapi_base_url,
        auth_route_config,
        /*cached_auth*/ None,
        Some(&[codex_protocol::auth::AuthMode::Chatgpt]),
    )
    .await
    .ok()
    .flatten()
    .filter(CodexAuth::is_chatgpt_auth)
    .filter(|auth| chatgpt_auth_workspace_allowed(auth, forced_chatgpt_workspace_id))
}

pub(in crate::auth::manager) fn chatgpt_auth_workspace_allowed(
    auth: &CodexAuth,
    expected_workspace_ids: Option<&[String]>,
) -> bool {
    if expected_workspace_ids.is_none() {
        return true;
    }
    if auth.is_external_chatgpt_tokens() {
        return auth.get_account_id().as_deref().is_some_and(|account_id| {
            crate::server::ensure_workspace_account_allowed(expected_workspace_ids, account_id)
                .is_ok()
        });
    }
    let Ok(token_data) = auth.get_token_data() else {
        return false;
    };
    crate::server::ensure_workspace_allowed(
        expected_workspace_ids,
        token_data.id_token.raw_jwt.as_str(),
    )
    .is_ok()
}
