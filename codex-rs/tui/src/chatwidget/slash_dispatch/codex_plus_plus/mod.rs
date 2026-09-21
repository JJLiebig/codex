//! Codex++ settings exposed through fork slash commands.

mod accounts;
mod dcg;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use AutomaticAccountSelection::Disabled as AutomaticOff;
use AutomaticAccountSelection::Enabled as AutomaticOn;
use ModelCapacityRetryMode::Bounded as CapacityBounded;
use ModelCapacityRetryMode::Indefinite as CapacityIndefinite;
use ToolActivityPresentation::Compact as ActivityCompact;
use ToolActivityPresentation::Full as ActivityFull;
use WeeklyUsageWindowAutoStart::Disabled as WeeklyOff;
use WeeklyUsageWindowAutoStart::Enabled as WeeklyOn;
use codex_config::AutoRedeemResets;
use codex_config::ModelCapacityRetryMode;
use codex_config::ToolActivityPresentation;
use codex_config::WeeklyUsageWindowAutoStart;
use codex_config::types::AutomaticAccountSelection;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use ratatui::text::Line;

use super::super::*;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionToggle;
use crate::codex_plus_plus::destructive_command_guard::DcgStatus;
use crate::key_hint;
use crate::keymap::ListKeymap;
use crate::keymap::primary_binding;

impl ChatWidget {
    pub(crate) fn open_codex_plus_plus_popup(&mut self, dcg_status: DcgStatus) {
        let list_keymap = settings_list_keymap(self.bottom_pane.list_keymap());
        let params = codex_plus_plus_settings_params(
            self.config.automatic_account_selection,
            self.config.weekly_usage_window_auto_start,
            crate::codex_plus_plus::auto_redeem_resets_settings(&self.config.config_layer_stack),
            self.config.model_capacity_retry_mode,
            self.config.codex_plus_plus_tool_activity,
            codex_config::disable_unnecessary_updates(&self.config.config_layer_stack),
            self.weekly_start_supported,
            Some(dcg_status),
            &list_keymap,
        );
        let view = ListSelectionView::new(params, self.app_event_tx.clone(), list_keymap);
        self.bottom_pane.show_view(Box::new(view));
        self.request_redraw();
    }

    pub(crate) fn codex_plus_plus_settings_persisted(
        &mut self,
        automatic: AutomaticAccountSelection,
        weekly: WeeklyUsageWindowAutoStart,
        capacity: ModelCapacityRetryMode,
        tool_activity: ToolActivityPresentation,
    ) {
        self.config.automatic_account_selection = automatic;
        self.config.weekly_usage_window_auto_start = weekly;
        self.config.model_capacity_retry_mode = capacity;
        self.config.codex_plus_plus_tool_activity = tool_activity;
        self.add_info_message(persistence_success_message(), /*hint*/ None);
    }

    pub(crate) fn sync_codex_plus_plus_settings_config(
        &mut self,
        config: &crate::legacy_core::config::Config,
    ) {
        self.config.automatic_account_selection = config.automatic_account_selection;
        self.config.weekly_usage_window_auto_start = config.weekly_usage_window_auto_start;
        self.config.model_capacity_retry_mode = config.model_capacity_retry_mode;
        self.config.codex_plus_plus_tool_activity = config.codex_plus_plus_tool_activity;
        self.config.config_layer_stack = config.config_layer_stack.clone();
    }

    pub(crate) fn codex_plus_plus_settings_persistence_failed(&mut self, err: String) {
        self.add_error_message(persistence_error_message(err));
    }

    pub(crate) fn codex_plus_plus_settings_persistence_overridden(
        &mut self,
        automatic: AutomaticAccountSelection,
        weekly: WeeklyUsageWindowAutoStart,
        capacity: ModelCapacityRetryMode,
        tool_activity: ToolActivityPresentation,
    ) {
        self.config.automatic_account_selection = automatic;
        self.config.weekly_usage_window_auto_start = weekly;
        self.config.model_capacity_retry_mode = capacity;
        self.config.codex_plus_plus_tool_activity = tool_activity;
        self.add_error_message(persistence_overridden_message());
    }

    pub(crate) fn codex_plus_plus_settings_verification_failed(&mut self, err: String) {
        self.add_error_message(persistence_verification_failed_message(err));
    }
}

#[derive(Clone, Default)]
struct SettingsSelection {
    automatic: Arc<AtomicBool>,
    weekly: Arc<AtomicBool>,
    auto_redeem_expiring: Arc<AtomicBool>,
    auto_redeem_exhausted: Arc<AtomicBool>,
    auto_redeem_thresholds: AutoRedeemResets,
    capacity_indefinite: Arc<AtomicBool>,
    compact_tool_activity: Arc<AtomicBool>,
    quiet_updates: Arc<AtomicBool>,
}

impl SettingsSelection {
    fn selected_auto_redeem(&self) -> AutoRedeemResets {
        AutoRedeemResets {
            before_expiry_minutes: self
                .auto_redeem_expiring
                .load(Ordering::Relaxed)
                .then_some(self.auto_redeem_thresholds.before_expiry_minutes)
                .flatten(),
            weekly_exhausted_min_wait_hours: self
                .auto_redeem_exhausted
                .load(Ordering::Relaxed)
                .then_some(self.auto_redeem_thresholds.weekly_exhausted_min_wait_hours)
                .flatten(),
        }
    }
}

fn settings_list_keymap(mut keymap: ListKeymap) -> ListKeymap {
    keymap
        .accept
        .retain(|binding| !binding.is_press(KeyEvent::from(KeyCode::Char(' '))));
    if keymap.accept.is_empty() {
        keymap.accept.push(key_hint::plain(KeyCode::Enter));
    }
    keymap
}

fn codex_plus_plus_settings_params(
    current_automatic: AutomaticAccountSelection,
    current_weekly: WeeklyUsageWindowAutoStart,
    current_auto_redeem: Option<AutoRedeemResets>,
    current_capacity: ModelCapacityRetryMode,
    current_tool_activity: ToolActivityPresentation,
    current_quiet_updates: bool,
    weekly_supported: bool,
    dcg_status: Option<DcgStatus>,
    list_keymap: &ListKeymap,
) -> SelectionViewParams {
    let defaults = AutoRedeemResets::default();
    let thresholds = current_auto_redeem.unwrap_or(defaults);
    let selection = SettingsSelection {
        automatic: Arc::new(AtomicBool::new(current_automatic == AutomaticOn)),
        weekly: Arc::new(AtomicBool::new(current_weekly == WeeklyOn)),
        auto_redeem_expiring: Arc::new(AtomicBool::new(
            current_auto_redeem.is_some_and(|settings| settings.before_expiry_minutes.is_some()),
        )),
        auto_redeem_exhausted: Arc::new(AtomicBool::new(
            current_auto_redeem
                .is_some_and(|settings| settings.weekly_exhausted_min_wait_hours.is_some()),
        )),
        auto_redeem_thresholds: AutoRedeemResets {
            before_expiry_minutes: thresholds
                .before_expiry_minutes
                .or(defaults.before_expiry_minutes),
            weekly_exhausted_min_wait_hours: thresholds
                .weekly_exhausted_min_wait_hours
                .or(defaults.weekly_exhausted_min_wait_hours),
        },
        capacity_indefinite: Arc::new(AtomicBool::new(current_capacity == CapacityIndefinite)),
        compact_tool_activity: Arc::new(AtomicBool::new(current_tool_activity == ActivityCompact)),
        quiet_updates: Arc::new(AtomicBool::new(current_quiet_updates)),
    };
    let mut items = vec![settings_item(
        "Automatic account selection",
        "Choose and switch accounts when needed.",
        Arc::clone(&selection.automatic),
        selection.clone(),
        weekly_supported,
    )];
    if let Some(status) = dcg_status {
        items.insert(
            0,
            dcg::settings_item(status, selection.clone(), weekly_supported),
        );
    }
    if weekly_supported {
        items.push(settings_item(
            "Start unused weekly windows",
            "Keep imported accounts ready to use.",
            Arc::clone(&selection.weekly),
            selection.clone(),
            true,
        ));
        items.push(settings_item(
            "Auto-redeem expiring resets (Experimental)",
            &format!(
                "Within {}m of expiry.",
                selection
                    .auto_redeem_thresholds
                    .before_expiry_minutes
                    .unwrap()
            ),
            Arc::clone(&selection.auto_redeem_expiring),
            selection.clone(),
            true,
        ));
        items.push(settings_item(
            "Auto-redeem at exhaustion (Experimental)",
            &format!(
                "0% weekly quota; reset ≥{}h.",
                selection
                    .auto_redeem_thresholds
                    .weekly_exhausted_min_wait_hours
                    .unwrap()
            ),
            Arc::clone(&selection.auto_redeem_exhausted),
            selection.clone(),
            true,
        ));
    }
    items.push(settings_item(
        "Keep retrying at capacity",
        "After 1, 2, 5, and 15 minutes, retry every 15 minutes.",
        Arc::clone(&selection.capacity_indefinite),
        selection.clone(),
        weekly_supported,
    ));
    items.push(settings_item(
        "Compact tool activity",
        "Keep routine tool details in Ctrl+T instead of the main conversation.",
        Arc::clone(&selection.compact_tool_activity),
        selection.clone(),
        weekly_supported,
    ));
    items.push(settings_item(
        "Disable unnecessary updates to save tokens",
        "Report meaningful progress instead of timed status updates.",
        Arc::clone(&selection.quiet_updates),
        selection,
        weekly_supported,
    ));
    SelectionViewParams {
        title: Some("Codex++ Settings".to_string()),
        subtitle: Some("Select the settings to enable.".to_string()),
        footer_hint: Some(settings_hint_line(list_keymap)),
        items,
        ..Default::default()
    }
}

fn settings_item(
    name: &str,
    description: &str,
    toggle: Arc<AtomicBool>,
    selection: SettingsSelection,
    save_weekly: bool,
) -> SelectionItem {
    let toggle_action = Arc::clone(&toggle);
    SelectionItem {
        name: name.to_string(),
        description: Some(description.to_string()),
        toggle: Some(SelectionToggle {
            is_on: toggle.load(Ordering::Relaxed),
            action: Box::new(move |is_on, _tx| toggle_action.store(is_on, Ordering::Relaxed)),
        }),
        actions: vec![Box::new(move |tx| {
            let automatic = if selection.automatic.load(Ordering::Relaxed) {
                AutomaticOn
            } else {
                AutomaticOff
            };
            tx.send(AppEvent::PersistCodexPlusPlusSettings {
                disable_unnecessary_updates: selection.quiet_updates.load(Ordering::Relaxed),
                automatic_account_selection: automatic,
                weekly_usage_window_auto_start: save_weekly.then(|| {
                    if selection.weekly.load(Ordering::Relaxed) {
                        WeeklyOn
                    } else {
                        WeeklyOff
                    }
                }),
                auto_redeem_resets: save_weekly.then(|| selection.selected_auto_redeem()),
                model_capacity_retry_mode: if selection.capacity_indefinite.load(Ordering::Relaxed)
                {
                    CapacityIndefinite
                } else {
                    CapacityBounded
                },
                tool_activity: if selection.compact_tool_activity.load(Ordering::Relaxed) {
                    ActivityCompact
                } else {
                    ActivityFull
                },
            });
        })],
        dismiss_on_select: true,
        ..Default::default()
    }
}

fn settings_hint_line(list_keymap: &ListKeymap) -> Line<'static> {
    let mut spans = vec![
        "Press ".into(),
        key_hint::plain(KeyCode::Char(' ')).into(),
        " to toggle".into(),
    ];
    if let Some(accept) = primary_binding(&list_keymap.accept) {
        spans.extend(["; ".into(), accept.into(), " to save or manage".into()]);
    }
    if let Some(cancel) = primary_binding(&list_keymap.cancel) {
        spans.extend(["; ".into(), cancel.into(), " to cancel".into()]);
    }
    spans.into()
}

fn persistence_success_message() -> String {
    "Codex++ settings updated. Changes apply after restart.".to_string()
}

fn persistence_error_message(err: String) -> String {
    format!("Failed to update Codex++ settings: {err}")
}

fn persistence_overridden_message() -> String {
    "Some Codex++ settings are controlled elsewhere and were not changed.".to_string()
}

fn persistence_verification_failed_message(err: String) -> String {
    format!("Codex++ settings were saved, but Codex could not verify them for this project: {err}")
}

#[cfg(test)]
#[path = "codex_plus_plus_tests.rs"]
mod tests;
