//! Client-owned appearance controls in the Codex++ settings menu.
//!
//! Space only stages values. An ordinary save (including save-before-DCG-management) first writes
//! changed appearance keys to the local client's config, then runs the existing server-settings
//! actions in their original order. A local write failure submits none of those actions. The two
//! scopes are intentionally separate: a remote app server must not receive client TUI preferences.

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::SelectionAction;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionToggle;
use crate::bottom_pane::SelectionViewParams;
use crate::history_cell;
use crate::legacy_core::config::edit::ConfigEdit;
use crate::legacy_core::config::edit::ConfigEditsBuilder;
use crate::local_settings::LocalSettings;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DisplaySettings {
    show_footer_hints: bool,
    show_tooltips: bool,
}

#[derive(Clone)]
struct DisplaySelection {
    initial: DisplaySettings,
    footer: Arc<AtomicBool>,
    tips: Arc<AtomicBool>,
}

impl DisplaySelection {
    fn new(initial: DisplaySettings) -> Self {
        Self {
            initial,
            footer: Arc::new(AtomicBool::new(initial.show_footer_hints)),
            tips: Arc::new(AtomicBool::new(initial.show_tooltips)),
        }
    }

    fn selected(&self) -> DisplaySettings {
        DisplaySettings {
            show_footer_hints: self.footer.load(Ordering::Relaxed),
            show_tooltips: self.tips.load(Ordering::Relaxed),
        }
    }
}

pub(super) fn add_to_menu(params: &mut SelectionViewParams, settings: &LocalSettings) {
    let selection = DisplaySelection::new(DisplaySettings {
        show_footer_hints: settings.tui.show_footer_hints.unwrap_or(true),
        show_tooltips: settings.tui.show_tooltips,
    });
    let path = settings.user_config_path.to_path_buf();
    let mut ordinary_save = None;
    for item in &mut params.items {
        if item.actions.is_empty() {
            continue;
        }
        let actions = Arc::new(std::mem::take(&mut item.actions));
        // Ordinary toggle rows all save the same staged server settings. Reuse that action rather
        // than duplicating its payload or accidentally inheriting a DCG management action.
        if item.toggle.is_some() && ordinary_save.is_none() {
            ordinary_save = Some(Arc::clone(&actions));
        }
        item.actions = vec![save_then(path.clone(), selection.clone(), actions)];
    }
    let Some(ordinary_save) = ordinary_save else {
        return;
    };
    for (name, description, toggle) in [
        (
            "Show footer hints",
            "Show the extra shortcuts/warning row below status; keep warnings available in /warnings.",
            Arc::clone(&selection.footer),
        ),
        (
            "Show tips",
            "Show startup and working/completion tips (tui.show_tooltips).",
            Arc::clone(&selection.tips),
        ),
    ] {
        params.items.push(SelectionItem {
            name: name.to_string(),
            description: Some(description.to_string()),
            toggle: Some(SelectionToggle {
                is_on: toggle.load(Ordering::Relaxed),
                action: Box::new(move |is_on, _tx| toggle.store(is_on, Ordering::Relaxed)),
            }),
            actions: vec![save_then(
                path.clone(),
                selection.clone(),
                Arc::clone(&ordinary_save),
            )],
            dismiss_on_select: true,
            ..Default::default()
        });
    }
}

fn save_then(
    path: PathBuf,
    selection: DisplaySelection,
    actions: Arc<Vec<SelectionAction>>,
) -> SelectionAction {
    Box::new(move |tx| {
        let selected = selection.selected();
        if selected == selection.initial {
            for action in actions.iter() {
                action(tx);
            }
            return;
        }
        let path = path.clone();
        let initial = selection.initial;
        let actions = Arc::clone(&actions);
        let tx = tx.clone();
        tokio::spawn(async move {
            match save(&path, initial, selected).await {
                Ok(()) => {
                    notify(
                        &tx,
                        "Display preferences saved locally. Restart Codex to apply; launch overrides still apply."
                            .to_string(),
                    );
                    for action in actions.iter() {
                        action(&tx);
                    }
                }
                Err(error) => notify(
                    &tx,
                    format!(
                        "Failed to save display preferences; other menu changes were not submitted: {error}"
                    ),
                ),
            }
        });
    })
}

fn notify(tx: &AppEventSender, message: String) {
    tx.send(AppEvent::InsertHistoryCell(Box::new(history_cell::new_info_event(
        message,
        /*hint*/ None,
    ))));
}

async fn save(path: &Path, initial: DisplaySettings, selected: DisplaySettings) -> anyhow::Result<()> {
    let edits = [
        ("show_footer_hints", initial.show_footer_hints, selected.show_footer_hints),
        ("show_tooltips", initial.show_tooltips, selected.show_tooltips),
    ]
    .into_iter()
    .filter(|(_, before, after)| before != after)
    .map(|(key, _, value)| ConfigEdit::SetPath {
        segments: vec!["tui".into(), key.into()],
        value: toml_edit::value(value),
    })
    .collect::<Vec<_>>();
    if edits.is_empty() {
        return Ok(());
    }
    ConfigEditsBuilder::for_config_path(path)
        .with_edits(edits)
        .apply()
        .await?;
    Ok(())
}

#[cfg(test)]
#[path = "display_settings_tests.rs"]
mod tests;
