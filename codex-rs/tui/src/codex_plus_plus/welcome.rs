//! Codex++ onboarding guidance.

use ratatui::style::Stylize;
use ratatui::text::Line;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

pub(crate) const WELCOME_TIP: &str = "Welcome to **Codex++**. Use **/codexplusplus** for settings and **/accounts** to enable or disable accounts.";
pub(crate) const DCG_NUX_TIP: &str = "Tip: Enable Destructive Command Guard in /codexplusplus to send risky commands to Guardian for review even in full --yolo mode.";
pub(crate) const DCG_UPDATE_TIP: &str =
    "Tip: Destructive Command Guard was updated. Use /codexplusplus to upgrade when convenient.";

static DCG_NUX_PENDING: AtomicBool = AtomicBool::new(false);
static DCG_NUX_RENDER_PENDING: AtomicBool = AtomicBool::new(false);
pub(super) static DCG_UPDATE_AVAILABLE: AtomicBool = AtomicBool::new(false);

#[cfg(not(test))]
pub(crate) fn mark_dcg_nux_pending() {
    DCG_NUX_PENDING.store(true, Ordering::Relaxed);
}

pub(crate) fn replace_upstream_app_promo(tip: &'static str) -> &'static str {
    if tip.contains("codex app") || tip.contains("Desktop app") || tip.contains("desktop app") {
        WELCOME_TIP
    } else {
        tip
    }
}

pub(crate) fn dcg_update_available() -> bool {
    DCG_UPDATE_AVAILABLE.load(Ordering::Relaxed)
}

pub(crate) fn welcome_help_lines() -> Vec<Line<'static>> {
    wrapped_tip(&WELCOME_TIP.replace("**", ""))
}

pub(crate) fn take_dcg_nux_help_lines() -> Option<Vec<Line<'static>>> {
    DCG_NUX_PENDING
        .swap(false, Ordering::Relaxed)
        .then(|| wrapped_tip(DCG_NUX_TIP))
        .inspect(|_| DCG_NUX_RENDER_PENDING.store(true, Ordering::Relaxed))
}

pub(crate) fn take_dcg_nux_render_pending() -> bool {
    DCG_NUX_RENDER_PENDING.swap(false, Ordering::Relaxed)
}

#[cfg(test)]
fn welcome_help_lines_for(show_dcg_nux: bool) -> Vec<Line<'static>> {
    let mut lines = welcome_help_lines();
    if show_dcg_nux {
        lines.extend(wrapped_tip(DCG_NUX_TIP));
    }
    lines
}

fn wrapped_tip(tip: &str) -> Vec<Line<'static>> {
    textwrap::wrap(
        tip,
        textwrap::Options::new(/*width*/ 78)
            .initial_indent("  ")
            .subsequent_indent("  "),
    )
    .into_iter()
    .map(|line| line.into_owned().dim().into())
    .collect()
}

#[cfg(test)]
#[path = "welcome_tests.rs"]
mod tests;
