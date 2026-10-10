use super::*;
use crate::test_backend::VT100Backend;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use ratatui::Terminal;
use tokio_stream::wrappers::UnboundedReceiverStream;

fn candidates() -> Vec<AccountPickerCandidate> {
    vec![
        AccountPickerCandidate {
            id: "acct_a".to_string(),
            email: "first@example.com".to_string(),
            plan_type: Some(codex_protocol::account::PlanType::Plus),
            available_resets: Some(0),
            primary_window_label: "5h".to_string(),
            five_hour_reset: Some("Jul 10 17:00".to_string()),
            five_hour_usage_left_percent: Some(0),
            five_hour_exhausted: true,
            weekly_reset: Some("Jul 14".to_string()),
            weekly_usage_left_percent: Some(12),
            weekly_exhausted: false,
            blocked_until: None,
            blocked: false,
            in_use: true,
            is_current: true,
            is_default: false,
        },
        AccountPickerCandidate {
            id: "acct_b".to_string(),
            email: "best@example.com".to_string(),
            plan_type: Some(codex_protocol::account::PlanType::Pro),
            available_resets: Some(2),
            primary_window_label: "5h".to_string(),
            five_hour_reset: Some("Jul 10 18:00".to_string()),
            five_hour_usage_left_percent: Some(72),
            five_hour_exhausted: false,
            weekly_reset: None,
            weekly_usage_left_percent: Some(84),
            weekly_exhausted: false,
            blocked_until: None,
            blocked: false,
            in_use: false,
            is_current: false,
            is_default: true,
        },
        AccountPickerCandidate {
            id: "acct_c".to_string(),
            email: "unknown@example.com".to_string(),
            plan_type: None,
            available_resets: None,
            primary_window_label: "5h".to_string(),
            five_hour_reset: None,
            five_hour_usage_left_percent: None,
            five_hour_exhausted: false,
            weekly_reset: None,
            weekly_usage_left_percent: None,
            weekly_exhausted: false,
            blocked_until: None,
            blocked: false,
            in_use: false,
            is_current: false,
            is_default: false,
        },
    ]
}

#[test]
fn account_picker_snapshot() {
    let mut rows = candidates();
    rows[1].weekly_usage_left_percent = None;
    let mut weekly_only = rows[1].clone();
    weekly_only.id = "acct_weekly".to_string();
    weekly_only.email = "weekly@example.com".to_string();
    weekly_only.plan_type = Some(codex_protocol::account::PlanType::ProLite);
    weekly_only.five_hour_reset = None;
    weekly_only.five_hour_usage_left_percent = None;
    weekly_only.weekly_reset = Some("Jul 15 12:00".to_string());
    weekly_only.weekly_usage_left_percent = Some(84);
    weekly_only.is_default = false;
    rows.push(weekly_only);
    let mut pro_max = rows[1].clone();
    pro_max.email = "max@example.com".to_string();
    pro_max.plan_type = Some(codex_protocol::account::PlanType::ProMax);
    pro_max.is_default = false;
    rows.push(pro_max);
    let view = new_view(
        &rows,
        /*selected_idx*/ 1,
        /*seconds_remaining*/ Some(15),
    );
    let mut terminal =
        Terminal::new(VT100Backend::new(/*width*/ 120, /*height*/ 11)).expect("terminal");
    terminal
        .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
        .expect("render account picker");

    let usage = terminal
        .backend()
        .vt100()
        .screen()
        .cell(/*row*/ 3, /*col*/ 39)
        .expect("first usage description");
    assert_eq!(usage.contents(), "R");
    assert!(!usage.dim());
    insta::assert_snapshot!("account_picker_startup", terminal.backend());

    let view = new_view(
        &rows, /*selected_idx*/ 1, /*seconds_remaining*/ None,
    );
    terminal
        .draw(|frame| view.render(frame.area(), frame.buffer_mut()))
        .expect("render account picker after input");

    insta::assert_snapshot!("account_picker_manual", terminal.backend());
}

#[test]
fn default_candidate_prefers_backend_marked_default() {
    assert_eq!(
        default_candidate(&candidates()).map(|candidate| candidate.id.as_str()),
        Some("acct_b")
    );
}

#[test]
fn recommendation_avoids_accounts_used_by_other_sessions() {
    assert_eq!(recommended_candidate_index(&candidates()), 1);
}

#[test]
fn recommendation_avoids_exhausted_five_hour_window() {
    let mut candidates = candidates();
    candidates[0].in_use = false;
    candidates[0].is_current = false;
    candidates[1].in_use = true;
    candidates[2].five_hour_usage_left_percent = Some(0);
    candidates[2].five_hour_exhausted = true;

    assert_eq!(recommended_candidate_index(&candidates), 1);
}

#[test]
fn recommendation_does_not_treat_rounded_zero_as_exhausted() {
    let mut candidates = candidates();
    candidates[0].five_hour_exhausted = false;
    candidates[0].in_use = false;
    candidates[0].is_current = false;
    candidates[1].in_use = true;
    candidates[2].five_hour_exhausted = true;

    assert_eq!(recommended_candidate_index(&candidates), 0);
}

#[test]
fn enter_selects_highlighted_candidate() {
    let rows = candidates();
    let mut view = new_view(
        &rows,
        /*selected_idx*/ 1,
        /*seconds_remaining*/ Some(15),
    );

    view.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

    assert!(view.is_complete());
    assert_eq!(view.take_last_selected_index(), Some(1));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn input_cancels_auto_selection() -> Result<()> {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let picker = run_startup_account_picker_with_events(
        &mut tui,
        candidates(),
        StartupAccountPickerMode::Timed,
        UnboundedReceiverStream::new(event_rx),
    );
    tokio::pin!(picker);

    event_tx.send(TuiEvent::Key(KeyEvent::new(
        KeyCode::Down,
        KeyModifiers::NONE,
    )))?;
    tokio::select! {
        biased;
        result = &mut picker => panic!("picker completed after input: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }

    tokio::time::advance(STARTUP_AUTO_PICK_AFTER + Duration::from_secs(1)).await;
    tokio::select! {
        biased;
        result = &mut picker => panic!("picker auto-selected after input: {result:?}"),
        _ = tokio::task::yield_now() => {}
    }

    event_tx.send(TuiEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))?;
    assert_eq!(
        picker.await?,
        Some(StartupAccountPickerSelection::User("acct_c".to_string()))
    );
    Ok(())
}
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn manual_mode_waits_for_selection() {
    let (_event_tx, event_rx) = mpsc::unbounded_channel();
    let mut tui = crate::tui::test_support::make_test_tui().expect("test TUI");
    let picker = run_startup_account_picker_with_events(
        &mut tui,
        vec![candidates()[0].clone()],
        StartupAccountPickerMode::Manual,
        UnboundedReceiverStream::new(event_rx),
    );
    tokio::pin!(picker);
    tokio::time::advance(STARTUP_AUTO_PICK_AFTER + Duration::from_secs(1)).await;
    let result = tokio::time::timeout(Duration::ZERO, &mut picker).await;
    assert!(result.is_err());
}

#[test]
fn row_description_uses_one_unknown_for_missing_usage_data() {
    for plan_type in [None, Some(codex_protocol::account::PlanType::Unknown)] {
        let mut candidate = candidates()[2].clone();
        candidate.plan_type = plan_type;
        let item = selection_item(&candidate);
        assert_eq!(
            item.description.as_deref(),
            Some("Resets: -  Usage unknown")
        );
    }
}

#[test]
fn row_description_uses_window_label_and_generic_blocked_reset() {
    let mut candidate = candidates()[2].clone();
    candidate.primary_window_label = "daily".to_string();
    candidate.five_hour_usage_left_percent = Some(42);
    candidate.blocked_until = Some("Jul 14 08:00".to_string());
    let item = selection_item(&candidate);

    assert_eq!(
        item.description.as_deref(),
        Some("Resets: -  daily  42%    Unavailable until Jul 14 08:00")
    );
}
