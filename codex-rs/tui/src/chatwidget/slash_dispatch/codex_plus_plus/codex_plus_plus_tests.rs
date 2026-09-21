use assert_matches::assert_matches;
use crossterm::event::KeyEvent;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use tokio::sync::mpsc::unbounded_channel;

use super::*;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::codex_plus_plus::destructive_command_guard::DcgUnsupportedReason;
use crate::codex_plus_plus::destructive_command_guard::RepairReason;
use crate::keymap::RuntimeKeymap;
use crate::test_backend::VT100Backend;

fn settings_view(
    automatic: AutomaticAccountSelection,
    weekly: WeeklyUsageWindowAutoStart,
    capacity: ModelCapacityRetryMode,
) -> (
    ListSelectionView,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) {
    let (tx, rx) = unbounded_channel();
    let keymap = settings_list_keymap(RuntimeKeymap::defaults().list);
    let weekly_supported = automatic == AutomaticAccountSelection::Enabled;
    (
        ListSelectionView::new(
            codex_plus_plus_settings_params(
                automatic,
                weekly,
                /*current_auto_redeem*/ None,
                capacity,
                ToolActivityPresentation::Full,
                /*current_quiet_updates*/ true,
                weekly_supported,
                None,
                &keymap,
            ),
            AppEventSender::new(tx),
            keymap,
        ),
        rx,
    )
}

fn render_settings(
    automatic: AutomaticAccountSelection,
    weekly: WeeklyUsageWindowAutoStart,
    capacity: ModelCapacityRetryMode,
) -> String {
    let (view, _rx) = settings_view(automatic, weekly, capacity);
    let mut terminal =
        Terminal::new(VT100Backend::new(/*width*/ 84, /*height*/ 24)).expect("terminal");
    terminal
        .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
        .expect("render Codex++ settings");
    terminal.backend().to_string()
}

#[test]
fn settings_enabled_snapshot() {
    insta::assert_snapshot!(
        "codex_plus_plus_settings_enabled",
        render_settings(
            AutomaticAccountSelection::Enabled,
            WeeklyUsageWindowAutoStart::Enabled,
            ModelCapacityRetryMode::Bounded,
        )
    );
}

#[test]
fn settings_unsupported_snapshot() {
    insta::assert_snapshot!(
        "codex_plus_plus_settings_unsupported",
        render_settings(
            AutomaticAccountSelection::Disabled,
            WeeklyUsageWindowAutoStart::Disabled,
            ModelCapacityRetryMode::Indefinite,
        )
    );
}

#[test]
fn unsupported_settings_save_only_the_visible_settings() {
    let (mut view, mut rx) = settings_view(
        AutomaticAccountSelection::Disabled,
        WeeklyUsageWindowAutoStart::Enabled,
        ModelCapacityRetryMode::Bounded,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::PersistCodexPlusPlusSettings {
            automatic_account_selection: AutomaticAccountSelection::Enabled,
            weekly_usage_window_auto_start: None,
            auto_redeem_resets: None,
            model_capacity_retry_mode: ModelCapacityRetryMode::Bounded,
            tool_activity: ToolActivityPresentation::Full,
            disable_unnecessary_updates: true,
        })
    );
}

#[test]
fn redemption_toggles_save_independently() {
    for (row, expected) in [
        (
            2,
            AutoRedeemResets {
                weekly_exhausted_min_wait_hours: None,
                ..AutoRedeemResets::default()
            },
        ),
        (
            3,
            AutoRedeemResets {
                before_expiry_minutes: None,
                ..AutoRedeemResets::default()
            },
        ),
    ] {
        let (mut view, mut rx) = settings_view(AutomaticOn, WeeklyOn, CapacityBounded);
        for _ in 0..row {
            view.handle_key_event(KeyEvent::from(KeyCode::Down));
        }
        view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
        view.handle_key_event(KeyEvent::from(KeyCode::Enter));
        let AppEvent::PersistCodexPlusPlusSettings {
            auto_redeem_resets, ..
        } = rx.try_recv().unwrap()
        else {
            panic!("expected settings save")
        };
        assert_eq!(auto_redeem_resets, Some(expected));
    }
}

#[test]
fn redemption_rows_describe_configured_thresholds() {
    let configured = AutoRedeemResets {
        before_expiry_minutes: std::num::NonZeroU64::new(15),
        weekly_exhausted_min_wait_hours: std::num::NonZeroU64::new(48),
    };
    let params = codex_plus_plus_settings_params(
        AutomaticOn,
        WeeklyOn,
        Some(configured),
        CapacityBounded,
        ActivityFull,
        /*current_quiet_updates*/ true,
        /*weekly_supported*/ true,
        /*dcg_status*/ None,
        &settings_list_keymap(RuntimeKeymap::defaults().list),
    );
    assert_eq!(
        [
            params.items[2].description.as_deref(),
            params.items[3].description.as_deref()
        ],
        [
            Some("Within 15m of expiry."),
            Some("0% weekly quota; reset ≥48h.")
        ]
    );
}

#[test]
fn weekly_setting_saves_full_selection() {
    let (mut view, mut rx) = settings_view(
        AutomaticAccountSelection::Enabled,
        WeeklyUsageWindowAutoStart::Enabled,
        ModelCapacityRetryMode::Bounded,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::PersistCodexPlusPlusSettings {
            automatic_account_selection: AutomaticAccountSelection::Enabled,
            weekly_usage_window_auto_start: Some(WeeklyUsageWindowAutoStart::Disabled),
            auto_redeem_resets: Some(AutoRedeemResets {
                before_expiry_minutes: None,
                weekly_exhausted_min_wait_hours: None
            }),
            model_capacity_retry_mode: ModelCapacityRetryMode::Bounded,
            tool_activity: ToolActivityPresentation::Full,
            disable_unnecessary_updates: true,
        })
    );
}

#[test]
fn capacity_setting_saves_indefinite_mode() {
    let (mut view, mut rx) = settings_view(
        AutomaticAccountSelection::Enabled,
        WeeklyUsageWindowAutoStart::Enabled,
        ModelCapacityRetryMode::Bounded,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Down));
    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::PersistCodexPlusPlusSettings {
            automatic_account_selection: AutomaticAccountSelection::Enabled,
            weekly_usage_window_auto_start: Some(WeeklyUsageWindowAutoStart::Enabled),
            auto_redeem_resets: Some(AutoRedeemResets {
                before_expiry_minutes: None,
                weekly_exhausted_min_wait_hours: None
            }),
            model_capacity_retry_mode: ModelCapacityRetryMode::Indefinite,
            tool_activity: ToolActivityPresentation::Full,
            disable_unnecessary_updates: true,
        })
    );
}

#[test]
fn compact_tool_activity_saves_full_selection() {
    let (mut view, mut rx) = settings_view(
        AutomaticAccountSelection::Enabled,
        WeeklyUsageWindowAutoStart::Enabled,
        ModelCapacityRetryMode::Bounded,
    );

    for _ in 0..5 {
        view.handle_key_event(KeyEvent::from(KeyCode::Down));
    }
    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::PersistCodexPlusPlusSettings {
            tool_activity: ToolActivityPresentation::Compact,
            ..
        })
    );
}

#[test]
fn escape_cancels_without_writing() {
    let (mut view, mut rx) = settings_view(
        AutomaticAccountSelection::Enabled,
        WeeklyUsageWindowAutoStart::Enabled,
        ModelCapacityRetryMode::Bounded,
    );

    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Esc));

    assert_matches!(rx.try_recv(), Err(_));
}

#[test]
fn settings_hint_uses_list_keymap() {
    let mut keymap = RuntimeKeymap::defaults().list;
    keymap.accept = vec![key_hint::plain(KeyCode::Char(' '))];
    keymap.cancel = vec![key_hint::plain(KeyCode::Char('q'))];
    let keymap = settings_list_keymap(keymap);

    let text = settings_hint_line(&keymap)
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();

    assert_eq!(
        text,
        "Press space to toggle; enter to save or manage; q to cancel"
    );
}

#[test]
fn dcg_views_snapshot() {
    let i = dcg::settings_item(
        DcgStatus::NotInstalled,
        Default::default(),
        /*save_weekly*/ false,
    );
    let (tx, mut rx) = unbounded_channel();
    (i.actions[0])(&AppEventSender::new(tx));
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::PersistCodexPlusPlusSettings { .. })
    );
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenDcgInstallConfirmation));
    let states = [
        DcgStatus::NotInstalled,
        DcgStatus::Enabled("0.6.8-codexpp.1".to_string()),
        DcgStatus::Disabled("0.6.8-codexpp.1".to_string()),
        DcgStatus::UpdateAvailable {
            installed_version: Some("0.6.7".to_string()),
            target_version: "0.6.8".to_string(),
        },
        DcgStatus::ExternalInstallation("0.6.8".to_string()),
        DcgStatus::NeedsRepair(RepairReason::HookUntrusted),
        DcgStatus::NeedsRepair(RepairReason::StatusUnavailable),
        DcgStatus::Unsupported(DcgUnsupportedReason::Platform),
    ];
    let flow = [
        dcg::confirmation_params(),
        dcg::progress_params(crate::codex_plus_plus::DcgAction::InstallAndEnable),
        dcg::failure_params(),
    ]
    .map(summarize_params)
    .join("\n");
    insta::assert_snapshot!(
        "codex_plus_plus_dcg_settings_states",
        format!("{}\n{flow}", states.map(render_dcg_item).join(" || "))
    );
}

fn render_dcg_item(status: DcgStatus) -> String {
    let item = dcg::settings_item(
        status,
        SettingsSelection::default(),
        /*save_weekly*/ false,
    );
    format!("{} | {} action(s)", item.name, item.actions.len())
}

fn summarize_params(params: SelectionViewParams) -> String {
    let items = params
        .items
        .into_iter()
        .map(|item| format!("{}|{}", item.name, item.description.unwrap_or_default()))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{}\n{items}", params.title.unwrap_or_default())
}

#[test]
fn persistence_messages_snapshot() {
    insta::assert_snapshot!(
        "codex_plus_plus_persistence_messages",
        [
            persistence_success_message(),
            persistence_error_message("permission denied".to_string()),
            persistence_overridden_message(),
            persistence_verification_failed_message("connection closed".to_string()),
        ]
        .join("\n")
    );
}

#[test]
fn quiet_updates_toggle_is_saved() {
    let (mut view, mut rx) = settings_view(AutomaticOn, WeeklyOn, CapacityBounded);
    view.handle_key_event(KeyEvent::from(KeyCode::End));
    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));
    assert_matches!(
        rx.try_recv(),
        Ok(AppEvent::PersistCodexPlusPlusSettings {
            disable_unnecessary_updates: false,
            ..
        })
    );
}
