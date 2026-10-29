//! Cloud configuration loading for an explicitly selected imported account.

use crate::bundle_loader::managed_config_bundle_loader;
use codex_config::CloudConfigBundleLoader;
use codex_config::ConfigRequirementsToml;
use codex_login::AccountId;
use codex_login::AuthManager;
use codex_login::AuthManagerConfig;

/// Builds a cloud configuration loader using the explicitly selected imported account.
pub async fn cloud_config_bundle_loader_for_selected_account(
    config: &impl AuthManagerConfig,
    selected_account_id: Option<&AccountId>,
    requirements: &ConfigRequirementsToml,
) -> std::io::Result<CloudConfigBundleLoader> {
    let auth_route_config = config.auth_route_config();
    let http_client_factory = auth_route_config.http_client_factory().clone();
    let auth_manager =
        AuthManager::shared_from_config(config, /*enable_codex_api_key_env*/ false).await?;
    if let Some(selected_account_id) = selected_account_id {
        auth_manager
            .activate_imported_account(selected_account_id)
            .await?;
    }
    Ok(managed_config_bundle_loader(
        auth_manager,
        config.chatgpt_base_url(),
        config.codex_home(),
        http_client_factory,
        requirements,
    ))
}
