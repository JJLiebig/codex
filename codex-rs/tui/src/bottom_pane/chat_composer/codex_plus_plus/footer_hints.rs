//! Codex++ visibility policy for the extra footer below the configured status line.
//!
//! Native scrollback combines status and hints, so it is deliberately left unchanged. Explicit
//! help, navigation, searches, pending chords, quit reminders, and transient feedback retain space.
//! Returning zero from the shared height calculation removes both painting and the reserved row.

use super::super::ActivePopup;
use super::ComposerRenderOptions;
use crate::bottom_pane::ChatComposer;
use crate::bottom_pane::footer::FooterMode;

pub(super) fn hide_passive_footer(
    composer: &ChatComposer,
    options: ComposerRenderOptions<'_>,
) -> bool {
    options.hide_footer_hints
        && options.separate_status_line
        && options.footer.is_none()
        && matches!(composer.popups.active, ActivePopup::None)
        && matches!(
            composer.footer_mode(),
            FooterMode::ComposerEmpty | FooterMode::ComposerHasDraft
        )
        && composer.footer.hint_override.is_none()
        && !composer.footer.flash_visible()
        && composer.history_search.is_none()
        && composer.draft.textarea.vim_query().is_none()
        && !composer.quit_shortcut_hint_visible()
}

#[cfg(test)]
#[path = "footer_hints_tests.rs"]
mod tests;
