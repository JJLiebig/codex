use codex_core::config::ConfigBuilder;
use codex_install_context::StandalonePlatform;
use codex_tui::UpdateAction;
use std::process::ExitStatus;

pub(crate) fn pending_message(action: UpdateAction, status: ExitStatus) -> Option<&'static str> {
    if matches!(
        action,
        UpdateAction::CodexPlusPlusStandalone(StandalonePlatform::Windows)
    ) && status.code() == Some(75)
    {
        return Some(
            "Upgrade pending. Close existing Codex sessions, then launch Codex to finish.",
        );
    }
    None
}

#[cfg(all(test, windows))]
#[path = "history_upgrade_tests.rs"]
mod tests;

/// Resolve the installation's global history target without opening history or auth.
pub(crate) async fn print_target() -> anyhow::Result<()> {
    let config = ConfigBuilder::default()
        .without_project_context()
        .build()
        .await?;
    println!("{}", config.sqlite.thread_history_db_path().display());
    Ok(())
}
