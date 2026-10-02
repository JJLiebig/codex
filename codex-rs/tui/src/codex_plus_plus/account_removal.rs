//! Interactive removal using the same account rows as startup.

use codex_login::AccountProfile;
use codex_login::AccountStore;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::StreamExt;

use super::startup_accounts::account_picker_candidate;
use super::startup_accounts::sort_candidates_alphabetically;
use crate::TerminalRestoreGuard;
use crate::account_picker;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::BottomPaneView;
use crate::bottom_pane::ListSelectionView;
use crate::bottom_pane::SelectionItem;
use crate::bottom_pane::SelectionViewParams;
use crate::keymap::RuntimeKeymap;
use crate::legacy_core::config::Config;
use crate::render::renderable::Renderable;
use crate::tui;
use crate::tui::Tui;
use crate::tui::TuiEvent;

/// Pick any stored account and require explicit confirmation before returning it.
pub async fn choose_account_to_remove(
    config: &Config,
) -> color_eyre::Result<Option<AccountProfile>> {
    let store = AccountStore::new(config.codex_home.to_path_buf());
    let profiles = store.list()?;
    if profiles.is_empty() {
        return Err(color_eyre::eyre::eyre!("No accounts imported."));
    }
    let current_id = store
        .current_root_account_id(
            config.cli_auth_credentials_store_mode,
            config.auth_keyring_backend_kind(),
        )
        .ok()
        .flatten();
    let mut candidates = store.candidates()?;
    sort_candidates_alphabetically(&mut candidates);
    let picker_candidates = candidates
        .iter()
        .map(|candidate| {
            account_picker_candidate(
                candidate,
                /*usage*/ None,
                store.account_in_use(&candidate.id).unwrap_or(false),
                current_id.as_ref() == Some(&candidate.id),
            )
        })
        .collect();
    let (initialized, mut restore_guard) = tokio::task::spawn_blocking(|| {
        tui::init().map(|terminal| (terminal, TerminalRestoreGuard::new()))
    })
    .await??;
    let mut tui = Tui::new(
        initialized.terminal,
        initialized.enhanced_keys_supported,
        initialized.stderr_guard,
    );
    tui.terminal_app_over_ssh = initialized.terminal_app_over_ssh;
    tui.enter_alt_screen()?;
    let result = async {
        let Some(selection) = account_picker::run_startup_account_picker(
            &mut tui,
            picker_candidates,
            account_picker::StartupAccountPickerMode::Manual,
        )
        .await?
        else {
            return Ok(None);
        };
        let account_picker::StartupAccountPickerSelection::User(id) = selection else {
            return Ok(None);
        };
        let Some(profile) = profiles
            .into_iter()
            .find(|profile| profile.id.as_str() == id)
        else {
            return Ok(None);
        };
        let events = tui.event_stream();
        if confirm_removal(&mut tui, &profile.label, events).await? {
            Ok(Some(profile))
        } else {
            Ok(None)
        }
    }
    .await;
    restore_guard.restore()?;
    result
}

fn confirmation_view(label: &str) -> ListSelectionView {
    let (tx, _rx) = mpsc::unbounded_channel::<AppEvent>();
    ListSelectionView::new(
        SelectionViewParams {
            title: Some(format!("Remove {label}?")),
            subtitle: Some("You will need to sign in again to add it back.".to_string()),
            items: vec!["Cancel", "Remove account"]
                .into_iter()
                .map(|name| SelectionItem {
                    name: name.to_string(),
                    dismiss_on_select: true,
                    ..Default::default()
                })
                .collect(),
            initial_selected_idx: Some(0),
            ..Default::default()
        },
        AppEventSender::new(tx),
        RuntimeKeymap::defaults().list,
    )
}

async fn confirm_removal(
    tui: &mut Tui,
    label: &str,
    mut events: impl Stream<Item = TuiEvent> + Unpin,
) -> color_eyre::Result<bool> {
    let mut view = confirmation_view(label);
    loop {
        tui.draw(u16::MAX, |frame| {
            view.render(frame.area(), frame.buffer_mut())
        })?;
        let Some(event) = events.next().await else {
            return Ok(false);
        };
        if let TuiEvent::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d'))
            {
                return Ok(false);
            }
            view.handle_key_event(key);
            if view.is_complete() {
                return Ok(view.take_last_selected_index() == Some(1));
            }
        }
    }
}

#[cfg(test)]
#[path = "account_removal_tests.rs"]
mod tests;
