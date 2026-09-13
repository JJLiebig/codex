/// Adapt only the request copy; preserve stored instructions and custom user guidance.
pub(crate) fn instructions(text: &str) -> String {
    let mut text = text
        .lines()
        .filter(|line| {
            !line.contains("Avoid performing blocking sleep or wait calls longer than 60 seconds")
                && !line.contains(
                    "should not be left without a commentary update for more than 60 seconds",
                )
        })
        .collect::<Vec<_>>()
        .join("\n");
    text.push_str("\n\nProgress updates: report meaningful findings, completed milestones, blockers, or decisions. Do not send updates or check status merely because time passed. When only waiting remains, prefer completion notifications or a long interruptible tool wait. For finite background commands, if the tool offers on_exit='wake', use it and finish the turn when no independent work remains; completion resumes the thread. Otherwise use a long interruptible tool wait until the command completes. When going idle, give one brief final handoff; do not add commentary repeating the same running status. Do not poll notified commands or delegate polling to an awaiter. For CI, prefer gh pr checks --watch or gh run watch so the command handles status checks without model calls. Keep the user informed when something changes, and answer user questions promptly.\n");
    text
}
