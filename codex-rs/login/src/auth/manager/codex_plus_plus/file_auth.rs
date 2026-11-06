//! File-only auth loading for isolated Codex++ account work.

use std::sync::Arc;
use std::sync::RwLock;

use codex_config::types::AuthCredentialsStoreMode;
use codex_config::types::AutomaticAccountSelection;

use super::super::AuthConfig;
use super::super::AuthKeyringBackendKind;
use super::super::AuthManager;
use super::super::CodexAuth;
use super::super::agent_identity_authapi_base_url;
use super::super::load_auth_from_storage;
use super::super::load_auth_from_storage_with_guard;
use super::super::validate_auth_restrictions;
use crate::account_lease::AuthRefreshGuard;

pub(in crate::auth::manager) async fn new_manager(
    auth_config: AuthConfig,
) -> std::io::Result<Option<AuthManager>> {
    let allowed_login_methods = auth_config.allowed_login_methods();
    if !allowed_login_methods.contains(&codex_protocol::config_types::ForcedLoginMethod::Chatgpt) {
        return Ok(None);
    }
    let effective_chatgpt_workspaces = auth_config.effective_chatgpt_workspaces();
    let codex_home = auth_config.codex_home;
    let agent_identity_authapi_base_url =
        agent_identity_authapi_base_url(auth_config.chatgpt_base_url.as_deref()).ok();
    let Some(auth) = load_auth_from_storage(
        &codex_home,
        AuthCredentialsStoreMode::File,
        Some(&allowed_login_methods),
        effective_chatgpt_workspaces.as_deref(),
        auth_config.chatgpt_base_url.as_deref(),
        AuthKeyringBackendKind::default(),
        agent_identity_authapi_base_url.as_deref(),
        &auth_config.auth_route_config,
        /*cached_auth*/ None,
        /*allowed_auth_modes*/ None,
    )
    .await?
    .filter(|auth| {
        validate_auth_restrictions(
            Some(&allowed_login_methods),
            effective_chatgpt_workspaces.as_deref(),
            auth,
        )
        .is_ok()
    }) else {
        return Ok(None);
    };
    let mut manager = Arc::into_inner(AuthManager::from_auth_with_home(auth, codex_home))
        .ok_or_else(|| std::io::Error::other("file auth manager is unexpectedly shared"))?;
    manager.auth_storage_only = true;
    manager.automatic_account_selection = AutomaticAccountSelection::Disabled;
    manager.forced_login_method = auth_config.forced_login_method;
    manager.forced_chatgpt_workspace_id = RwLock::new(auth_config.forced_chatgpt_workspace_id);
    manager.managed_auth_policy = auth_config.managed_auth_policy;
    manager.chatgpt_base_url = auth_config.chatgpt_base_url;
    manager.agent_identity_authapi_base_url = agent_identity_authapi_base_url;
    manager.auth_route_config = auth_config.auth_route_config;
    Ok(Some(manager))
}

pub(in crate::auth::manager) async fn load(
    manager: &AuthManager,
    cached_auth: Option<&CodexAuth>,
    guard: Option<&AuthRefreshGuard>,
) -> std::io::Result<Option<CodexAuth>> {
    let allowed_login_methods = manager.allowed_login_methods();
    let effective_chatgpt_workspaces = manager.effective_chatgpt_workspaces();
    let allowed_auth_modes = manager
        .allowed_auth_modes
        .read()
        .map_err(|_| std::io::Error::other("credential policy is unavailable"))?
        .clone();
    Ok(load_auth_from_storage_with_guard(
        &manager.active_auth_home(),
        manager.active_auth_credentials_store_mode(),
        Some(&allowed_login_methods),
        effective_chatgpt_workspaces.as_deref(),
        manager.chatgpt_base_url.as_deref(),
        manager.active_keyring_backend_kind(),
        manager.agent_identity_authapi_base_url.as_deref(),
        &manager.auth_route_config,
        cached_auth,
        allowed_auth_modes.as_deref(),
        guard,
    )
    .await?
    .filter(|auth| {
        validate_auth_restrictions(
            Some(&allowed_login_methods),
            effective_chatgpt_workspaces.as_deref(),
            auth,
        )
        .is_ok()
    }))
}
