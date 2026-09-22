use crate::history_cell::WarningHistoryCell;
use codex_install_context::codex_plus_plus::ForkReleaseStatus;
use std::path::Path;

pub(crate) fn lag_warning(
    status: &ForkReleaseStatus,
    codex_home: &Path,
) -> Option<WarningHistoryCell> {
    let lag = status.stable_minor_lag?;
    status
        .warning_state_key
        .as_deref()
        .filter(|key| key.starts_with("upstream-lag/"))?;

    Some(crate::history_cell::new_warning_event(format!(
        "Codex++ is {lag} releases behind Codex. Run `codex update upstream` to switch to upstream Codex. Your accounts stay saved in {} and will be ready if you switch back later.",
        codex_home.join("accounts").display()
    )))
}

#[cfg(test)]
#[path = "lag_warning_tests.rs"]
mod tests;
