use super::*;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::SelectionAction;
use crate::codex_plus_plus::DcgAction;
use crate::codex_plus_plus::destructive_command_guard::DcgChange;
use crate::codex_plus_plus::destructive_command_guard::DcgStatus;
use crate::codex_plus_plus::destructive_command_guard::DcgUnsupportedReason;
use crate::codex_plus_plus::destructive_command_guard::RepairReason;

const DESCRIPTION: &str = "Review destructive shell commands with Guardian.";
const PROGRESS_VIEW_ID: &str = "dcg-operation-progress";
const STATUS_VIEW_ID: &str = "dcg-status-progress";

pub(super) fn settings_item(
    status: DcgStatus,
    selection: SettingsSelection,
    save_weekly: bool,
) -> SelectionItem {
    let (state, action) = match status {
        DcgStatus::NotInstalled => (
            "Not installed".to_string(),
            Some(DcgAction::InstallAndEnable),
        ),
        DcgStatus::Enabled(version) => (format!("Enabled · {version}"), Some(DcgAction::Disable)),
        DcgStatus::Disabled(version) => (format!("Disabled · {version}"), Some(DcgAction::Enable)),
        DcgStatus::UpdateAvailable { target_version, .. } => (
            format!("Update available · {target_version}"),
            Some(DcgAction::Update),
        ),
        DcgStatus::ExternalInstallation(_) => ("External installation".to_string(), None),
        DcgStatus::NeedsRepair(reason) => (
            "Needs repair".to_string(),
            (!matches!(
                reason,
                RepairReason::MarketplaceConfigMalformed
                    | RepairReason::MarketplacePinMismatch
                    | RepairReason::HookDisabled
                    | RepairReason::StatusUnavailable
            ))
            .then_some(DcgAction::Repair(reason)),
        ),
        DcgStatus::Unsupported(DcgUnsupportedReason::Platform) => {
            ("Unsupported on this platform".to_string(), None)
        }
        DcgStatus::Unsupported(DcgUnsupportedReason::RemoteHookHost) => {
            ("Manage on the shell host".to_string(), None)
        }
    };
    let actions = action
        .map(|action| {
            vec![Box::new(move |tx: &AppEventSender| {
                tx.send(AppEvent::PersistCodexPlusPlusSettings {
                    disable_unnecessary_updates: selection.quiet_updates.load(Ordering::Relaxed),
                    automatic_account_selection: selected(
                        &selection.automatic,
                        AutomaticOn,
                        AutomaticOff,
                    ),
                    weekly_usage_window_auto_start: save_weekly
                        .then(|| selected(&selection.weekly, WeeklyOn, WeeklyOff)),
                    auto_redeem_resets: save_weekly.then(|| selection.selected_auto_redeem()),
                    model_capacity_retry_mode: selected(
                        &selection.capacity_indefinite,
                        CapacityIndefinite,
                        CapacityBounded,
                    ),
                    user_message_inbox: selected(&selection.user_message_inbox, InboxOn, InboxOff),
                    tool_activity: selected(
                        &selection.compact_tool_activity,
                        ActivityCompact,
                        ActivityFull,
                    ),
                });
                tx.send(match action {
                    DcgAction::InstallAndEnable => AppEvent::OpenDcgInstallConfirmation,
                    _ => AppEvent::ManageDcg(action),
                });
            }) as SelectionAction]
        })
        .unwrap_or_default();
    SelectionItem {
        name: format!("Destructive Command Guard · {state}"),
        description: Some(DESCRIPTION.to_string()),
        is_disabled: action.is_none(),
        actions,
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn selected<T: Copy>(value: &AtomicBool, on: T, off: T) -> T {
    [off, on][usize::from(value.load(Ordering::Relaxed))]
}

pub(super) fn confirmation_params() -> SelectionViewParams {
    SelectionViewParams {
        title: Some("Install Destructive Command Guard?".to_string()),
        subtitle: Some("Codex++ will enable protection for new sessions.".to_string()),
        items: vec![
            SelectionItem {
                name: "Install and enable".to_string(),
                description: Some(DESCRIPTION.to_string()),
                actions: vec![Box::new(|tx| {
                    tx.send(AppEvent::ManageDcg(DcgAction::InstallAndEnable));
                })],
                dismiss_on_select: true,
                ..Default::default()
            },
            SelectionItem {
                name: "Cancel".to_string(),
                dismiss_on_select: true,
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

pub(super) fn progress_params(action: DcgAction) -> SelectionViewParams {
    let mut params = message_params(format!("{}…", action_verb(action)), "Working…");
    params.view_id = Some(PROGRESS_VIEW_ID);
    params.allow_cancel = false;
    params
}

pub(super) fn failure_params() -> SelectionViewParams {
    message_params(
        "Operation failed".to_string(),
        "The operation did not finish. Try again.",
    )
}

fn message_params(name: String, description: &str) -> SelectionViewParams {
    SelectionViewParams {
        title: Some("Destructive Command Guard".to_string()),
        items: vec![SelectionItem {
            name,
            description: Some(description.to_string()),
            is_disabled: true,
            ..Default::default()
        }],
        ..Default::default()
    }
}

fn action_verb(action: DcgAction) -> &'static str {
    match action {
        DcgAction::InstallAndEnable => "Installing and enabling",
        DcgAction::Enable => "Enabling",
        DcgAction::Disable => "Disabling",
        DcgAction::Update => "Updating",
        DcgAction::Repair(_) => "Repairing",
    }
}

pub(super) fn action_succeeded(action: DcgAction, status: &DcgStatus) -> bool {
    match action {
        DcgAction::Disable => matches!(status, DcgStatus::Disabled(_)),
        DcgAction::Update => matches!(status, DcgStatus::Enabled(_) | DcgStatus::Disabled(_)),
        _ => matches!(status, DcgStatus::Enabled(_)),
    }
}

impl ChatWidget {
    pub(crate) fn show_dcg_status_progress(&mut self) {
        let mut params = message_params("Checking command guard…".to_string(), "Working…");
        params.view_id = Some(STATUS_VIEW_ID);
        self.show_dcg_view(params);
    }

    pub(crate) fn finish_dcg_status_detection(&mut self, status: DcgStatus) {
        if self.bottom_pane.dismiss_active_view_if_id(STATUS_VIEW_ID) {
            self.open_codex_plus_plus_popup(status);
        }
    }

    pub(crate) fn open_dcg_install_confirmation(&mut self) {
        self.show_dcg_view(confirmation_params());
    }

    pub(crate) fn show_dcg_progress(&mut self, action: DcgAction) {
        self.show_dcg_view(progress_params(action));
    }

    pub(crate) fn finish_dcg_action(&mut self, action: DcgAction, change: Option<DcgChange>) {
        if !self.bottom_pane.dismiss_active_view_if_id(PROGRESS_VIEW_ID) {
            self.bottom_pane.dismiss_view_by_id(PROGRESS_VIEW_ID);
            return;
        }
        match change {
            Some(change) if action_succeeded(action, &change.status) => {
                debug_assert!(!change.takes_effect_in_current_session);
                self.add_info_message(
                    "Destructive Command Guard settings updated. Changes apply to new sessions."
                        .to_string(),
                    /*hint*/ None,
                );
                self.open_codex_plus_plus_popup(change.status);
            }
            _ => self.show_dcg_view(failure_params()),
        }
    }

    fn show_dcg_view(&mut self, params: SelectionViewParams) {
        let keymap = self.bottom_pane.list_keymap();
        let view = ListSelectionView::new(params, self.app_event_tx.clone(), keymap);
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }
}
