//! Codex++ TUI capabilities kept separate from upstream-owned orchestration.

mod account_identity_freshness;
mod account_policy;
mod auto_redeem_resets;
pub(crate) mod destructive_command_guard;
#[cfg(any(not(debug_assertions), test))]
mod lag_warning;
mod model_capacity_retry;
mod release_status;
pub(crate) mod runtime_version;
mod startup_accounts;
mod weekly_window_scheduler;
mod welcome;

pub(crate) use account_identity_freshness::AccountIdentityFreshness;
pub(crate) use account_identity_freshness::MAY_BE_STALE_NOTE as ACCOUNT_IDENTITY_MAY_BE_STALE_NOTE;
pub(crate) use account_policy::persist_settings;
use anyhow::Result;
pub(crate) use auto_redeem_resets::settings as auto_redeem_resets_settings;
use destructive_command_guard::DcgChange;
use destructive_command_guard::DcgManager;
use destructive_command_guard::RepairReason;
#[cfg(not(debug_assertions))]
pub(crate) use lag_warning::lag_warning;
pub(crate) use model_capacity_retry::status_details as model_capacity_retry_status_details;
#[cfg(any(not(debug_assertions), test))]
pub(crate) use release_status::dismiss_version;
#[cfg(any(not(debug_assertions), test))]
pub(crate) use release_status::read_release_status;
#[cfg(not(debug_assertions))]
pub(crate) use release_status::refresh_release_status;
#[cfg(any(not(debug_assertions), test))]
pub(crate) use release_status::release_status_filepath;
pub(super) use startup_accounts::StartupAccountSelection;
pub(super) use startup_accounts::may_run_startup_account_picker;
pub(super) use startup_accounts::run_startup_account_picker;
pub(crate) use weekly_window_scheduler::WeeklyWindowScheduler;
pub(crate) use weekly_window_scheduler::WeeklyWindowStatus;
pub(crate) use welcome::DCG_UPDATE_TIP;
pub(crate) use welcome::WELCOME_TIP;
pub(crate) use welcome::dcg_update_available;
#[cfg(not(test))]
pub(crate) use welcome::mark_dcg_nux_pending;
pub(crate) use welcome::replace_upstream_app_promo;
pub(crate) use welcome::take_dcg_nux_help_lines;
pub(crate) use welcome::take_dcg_nux_render_pending;
pub(crate) use welcome::welcome_help_lines;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DcgAction {
    InstallAndEnable,
    Enable,
    Disable,
    Update,
    Repair(RepairReason),
}

pub(crate) async fn apply_dcg_action(manager: DcgManager, action: DcgAction) -> Result<DcgChange> {
    match action {
        DcgAction::InstallAndEnable => manager.install_and_enable().await,
        DcgAction::Enable => manager.enable().await,
        DcgAction::Disable => manager.disable().await,
        DcgAction::Update => manager.update().await,
        DcgAction::Repair(reason) => manager.repair(reason).await,
    }
}
#[cfg(not(test))]
pub(crate) async fn start_dcg_update_detection(
    app_server: &crate::app_server_session::AppServerSession,
    config: &crate::legacy_core::config::Config,
) {
    if let Ok(manager) = DcgManager::new(app_server, config) {
        manager.restore_cached_update_available();
        tokio::spawn(manager.reconcile_and_detect());
    }
}
