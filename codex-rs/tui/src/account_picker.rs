use std::cmp::Reverse;
use std::time::Duration;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ColumnWidthMode;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionRowDisplay;
use crate::bottom_pane::SelectionViewParams;
use crate::keymap::RuntimeKeymap;
use crate::render::renderable::Renderable;
use crate::tui::Tui;
use crate::tui::TuiEvent;
use color_eyre::Result;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use ratatui::style::Style;
use ratatui::style::Stylize as _;
use ratatui::text::Line;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_stream::Stream;
use tokio_stream::StreamExt;

const STARTUP_AUTO_PICK_AFTER: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AccountPickerCandidate {
    pub(crate) id: String,
    pub(crate) email: String,
    pub(crate) available_resets: Option<i64>,
    pub(crate) primary_window_label: String,
    pub(crate) five_hour_reset: Option<String>,
    pub(crate) five_hour_usage_left_percent: Option<u8>,
    pub(crate) five_hour_exhausted: bool,
    pub(crate) weekly_reset: Option<String>,
    pub(crate) weekly_usage_left_percent: Option<u8>,
    pub(crate) weekly_exhausted: bool,
    pub(crate) blocked_until: Option<String>,
    pub(crate) blocked: bool,
    pub(crate) in_use: bool,
    pub(crate) is_current: bool,
    pub(crate) is_default: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StartupAccountPickerMode {
    Timed,
    Manual,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum StartupAccountPickerSelection {
    Automatic(String),
    User(String),
}
pub(crate) async fn run_startup_account_picker(
    tui: &mut Tui,
    candidates: Vec<AccountPickerCandidate>,
    mode: StartupAccountPickerMode,
) -> Result<Option<StartupAccountPickerSelection>> {
    let events = tui.event_stream();
    run_startup_account_picker_with_events(tui, candidates, mode, events).await
}

async fn run_startup_account_picker_with_events(
    tui: &mut Tui,
    candidates: Vec<AccountPickerCandidate>,
    mode: StartupAccountPickerMode,
    mut events: impl Stream<Item = TuiEvent> + Unpin,
) -> Result<Option<StartupAccountPickerSelection>> {
    if candidates.len() <= 1 && mode == StartupAccountPickerMode::Timed {
        return Ok(default_candidate(&candidates)
            .map(|candidate| StartupAccountPickerSelection::Automatic(candidate.id.clone())));
    }

    let default_idx = default_candidate_index(&candidates);
    let deadline = Instant::now() + STARTUP_AUTO_PICK_AFTER;
    let mut auto_pick = mode == StartupAccountPickerMode::Timed;
    let mut view = new_view(
        &candidates,
        default_idx,
        auto_pick.then_some(STARTUP_AUTO_PICK_AFTER.as_secs()),
    );
    draw_view(tui, &view)?;

    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            biased;
            event = events.next() => {
                let Some(event) = event else {
                    return Ok(auto_pick.then(|| {
                        StartupAccountPickerSelection::Automatic(
                            candidates[default_idx].id.clone(),
                        )
                    }));
                };
                match event {
                    TuiEvent::Key(key) => {
                        if key.kind == KeyEventKind::Release {
                            continue;
                        }
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
                        {
                            return Ok(None);
                        }
                        view.handle_key_event(key);
                        if view.is_complete() {
                            return Ok(view
                                .take_last_selected_index()
                                .and_then(|idx| candidates.get(idx))
                                .map(|candidate| {
                                    StartupAccountPickerSelection::User(candidate.id.clone())
                                }));
                        }
                        if auto_pick {
                            auto_pick = false;
                            let selected_idx = view.selected_index().unwrap_or(default_idx);
                            view = new_view(&candidates, selected_idx, None);
                        }
                        draw_view(tui, &view)?;
                    }
                    TuiEvent::Paste(_) => {
                        if auto_pick {
                            auto_pick = false;
                            let selected_idx = view.selected_index().unwrap_or(default_idx);
                            view = new_view(&candidates, selected_idx, None);
                            draw_view(tui, &view)?;
                        }
                    }
                    TuiEvent::Draw
                    | TuiEvent::Resume
                    | TuiEvent::Resize(_)
                    | TuiEvent::FocusGained => {
                        draw_view(tui, &view)?
                    }
                    TuiEvent::FocusLost | TuiEvent::Mouse(_) => {}
                }
            }
            _ = tokio::time::sleep_until(deadline), if auto_pick => {
                return Ok(Some(StartupAccountPickerSelection::Automatic(
                    candidates[default_idx].id.clone(),
                )));
            }
            _ = tick.tick(), if auto_pick => {
                let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
                let selected_idx = view.selected_index().unwrap_or(default_idx);
                view = new_view(&candidates, selected_idx, Some(remaining));
                draw_view(tui, &view)?;
            }
        }
    }
}

pub(crate) fn default_candidate(
    candidates: &[AccountPickerCandidate],
) -> Option<&AccountPickerCandidate> {
    candidates
        .get(default_candidate_index(candidates))
        .or_else(|| candidates.first())
}

pub(crate) fn recommended_candidate_index(candidates: &[AccountPickerCandidate]) -> usize {
    candidates
        .iter()
        .enumerate()
        .max_by_key(|(index, candidate)| {
            let usage_left_percent = [
                candidate.five_hour_usage_left_percent,
                candidate.weekly_usage_left_percent,
            ]
            .into_iter()
            .flatten()
            .min();
            let has_usage_remaining = !candidate.five_hour_exhausted && !candidate.weekly_exhausted;
            (
                !candidate.blocked && has_usage_remaining,
                !candidate.in_use,
                usage_left_percent.is_some(),
                !candidate.is_current,
                usage_left_percent.unwrap_or_default(),
                Reverse(*index),
            )
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

fn default_candidate_index(candidates: &[AccountPickerCandidate]) -> usize {
    candidates
        .iter()
        .position(|candidate| candidate.is_default)
        .unwrap_or(0)
}

fn new_view(
    candidates: &[AccountPickerCandidate],
    selected_idx: usize,
    seconds_remaining: Option<u64>,
) -> ListSelectionView {
    let (tx, _rx) = mpsc::unbounded_channel::<AppEvent>();
    ListSelectionView::new(
        selection_params(candidates, selected_idx, seconds_remaining),
        AppEventSender::new(tx),
        RuntimeKeymap::defaults().list,
    )
}

fn selection_params(
    candidates: &[AccountPickerCandidate],
    selected_idx: usize,
    seconds_remaining: Option<u64>,
) -> SelectionViewParams {
    SelectionViewParams {
        title: Some("Choose account".to_string()),
        footer_note: seconds_remaining.map(|seconds_remaining| {
            Line::from(vec![
                "Auto-selects in ".dim(),
                seconds_remaining.to_string().bold(),
                "s".dim(),
            ])
        }),
        items: candidates.iter().map(selection_item).collect(),
        initial_selected_idx: Some(selected_idx.min(candidates.len().saturating_sub(1))),
        is_searchable: false,
        col_width_mode: ColumnWidthMode::AutoAllRows,
        row_display: SelectionRowDisplay::SingleLine,
        ..Default::default()
    }
}

fn selection_item(candidate: &AccountPickerCandidate) -> SelectionItem {
    let resets = candidate
        .available_resets
        .map_or_else(|| "-".to_string(), |count| count.to_string());
    let in_use = if candidate.in_use { "    In use" } else { "" };
    let blocked_until = candidate
        .blocked_until
        .as_ref()
        .map_or_else(String::new, |reset| {
            format!("    Unavailable until {reset}")
        });
    let mut usage = Vec::with_capacity(2);
    if let Some(percent) = candidate.five_hour_usage_left_percent {
        usage.push(format!(
            "{} {}",
            candidate.primary_window_label,
            usage_window(percent, candidate.five_hour_reset.as_deref()),
        ));
    }
    if let Some(percent) = candidate.weekly_usage_left_percent {
        usage.push(format!(
            "Weekly {}",
            usage_window(percent, candidate.weekly_reset.as_deref()),
        ));
    }
    let usage = if usage.is_empty() {
        "Usage unknown".to_string()
    } else {
        usage.join("  ")
    };
    SelectionItem {
        name: candidate.email.clone(),
        description: Some(format!("Resets: {resets}  {usage}{blocked_until}{in_use}")),
        description_style: Some(Style::default()),
        is_default: candidate.is_default,
        dismiss_on_select: true,
        search_value: Some(candidate.email.clone()),
        ..Default::default()
    }
}

fn usage_window(percent: u8, reset: Option<&str>) -> String {
    match reset {
        Some(reset) => format!("{percent:>3}% ({reset})"),
        None => format!("{percent:>3}%"),
    }
}

fn draw_view(tui: &mut Tui, view: &ListSelectionView) -> Result<()> {
    tui.draw(u16::MAX, |frame| {
        view.render(frame.area(), frame.buffer_mut());
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "account_picker_tests.rs"]
mod tests;
