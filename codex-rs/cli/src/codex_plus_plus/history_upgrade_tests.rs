use super::pending_message;
use codex_install_context::StandalonePlatform;
use codex_tui::UpdateAction;
use std::os::windows::process::ExitStatusExt;
use std::process::ExitStatus;

#[test]
fn only_managed_windows_pending_exit_defers_completion() {
    let outcomes = [
        (
            UpdateAction::CodexPlusPlusStandalone(StandalonePlatform::Windows),
            75,
        ),
        (
            UpdateAction::CodexPlusPlusStandalone(StandalonePlatform::Windows),
            0,
        ),
        (
            UpdateAction::CodexPlusPlusStandalone(StandalonePlatform::Unix),
            75,
        ),
        (UpdateAction::NpmGlobalLatest, 75),
    ]
    .map(|(action, status)| pending_message(action, ExitStatus::from_raw(status)));
    insta::assert_debug_snapshot!(outcomes, @r#"
    [
        Some(
            "Upgrade pending. Close existing Codex sessions, then launch Codex to finish.",
        ),
        None,
        None,
        None,
    ]
    "#);
}
