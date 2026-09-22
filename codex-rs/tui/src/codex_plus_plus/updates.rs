#![cfg(not(debug_assertions))]

use crate::legacy_core::config::Config;
use crate::update_action::UpdateAction;
use crate::update_versions::is_newer;
use crate::update_versions::is_source_build_version;
use crate::updates_cache::read_version_info;
use crate::updates_cache::version_filepath;
use codex_install_context::InstallContext;
use codex_install_context::codex_plus_plus::FORK_RELEASE_STATUS_MAX_AGE;
use codex_install_context::codex_plus_plus::UpdateChannel;
use codex_install_context::codex_plus_plus::UpdatePlan;
use std::time::SystemTime;

use crate::version::codex_cli_version;

pub(crate) use crate::updates_cache::dismiss_version;

pub fn get_upgrade_version(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup || is_source_build_version(codex_cli_version()) {
        return None;
    }

    let plan = UpdateAction::current_plan();
    let upstream_plan =
        UpdatePlan::for_install_context(InstallContext::current(), UpdateChannel::Upstream);
    let version_file = version_filepath(config);
    let info = read_version_info(&version_file).ok();

    if match &info {
        None => true,
        Some(info) => info.is_stale(SystemTime::now(), FORK_RELEASE_STATUS_MAX_AGE),
    } {
        // Refresh the cached latest version in the background so TUI startup
        // isn’t blocked by a network call. The UI reads the previously cached
        // value (if any) for this run; the next run shows the banner if needed.
        tokio::spawn(async move {
            crate::codex_plus_plus::refresh_release_status(
                &version_file,
                codex_cli_version(),
                plan,
                upstream_plan,
            )
            .await
            .inspect_err(|e| tracing::error!("Failed to update version: {e}"))
        });
    }

    info.and_then(|info| {
        let latest = info.latest_fork_version?;
        if is_newer(&latest, codex_cli_version()).unwrap_or(false) {
            Some(latest)
        } else {
            None
        }
    })
}

/// Returns the latest version to show in a popup, if it should be shown.
/// This respects the user's dismissal choice for the current latest version.
pub fn get_upgrade_version_for_popup(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup || is_source_build_version(codex_cli_version()) {
        return None;
    }

    let version_file = version_filepath(config);
    let latest = get_upgrade_version(config)?;
    // If the user dismissed this exact version previously, do not show the popup.
    if let Ok(info) = read_version_info(&version_file)
        && info.dismissed_version.as_deref() == Some(latest.as_str())
    {
        return None;
    }
    Some(latest)
}
