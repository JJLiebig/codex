use super::*;
use crate::history_cell::HistoryCell;
use insta::assert_snapshot;
use std::time::UNIX_EPOCH;

fn status(
    installed_fork_version: &str,
    latest_fork_version: Option<&str>,
    latest_stable_upstream_version: Option<&str>,
) -> ForkReleaseStatus {
    ForkReleaseStatus::new(
        installed_fork_version.to_string(),
        latest_fork_version.map(str::to_string),
        latest_stable_upstream_version.map(str::to_string),
        UNIX_EPOCH,
    )
}

fn render_case(label: &str, status: ForkReleaseStatus, codex_home: &Path) -> String {
    let rendered = lag_warning(&status, codex_home)
        .map(|cell| {
            cell.transcript_lines(80)
                .into_iter()
                .map(|line| {
                    line.spans
                        .into_iter()
                        .fold(String::new(), |mut text, span| {
                            text.push_str(span.content.as_ref());
                            text
                        })
                })
                .collect::<Vec<_>>()
                .join("\n")
                .replace('\\', "/")
        })
        .unwrap_or_else(|| "<none>".to_string());
    format!("{label}:\n{rendered}")
}

#[test]
fn renders_cached_lag_warning_states() {
    let cases = [
        render_case(
            "below threshold",
            status("0.144.4-fork.2", Some("0.144.4-fork.2"), Some("0.146.9")),
            Path::new("/home/alice/.codex"),
        ),
        render_case(
            "at threshold",
            status("0.144.4-fork.2", Some("0.144.4-fork.2"), Some("0.147.1")),
            Path::new("/home/alice/.codex"),
        ),
        render_case(
            "fork update takes precedence",
            status("0.144.4-fork.1", Some("0.144.4-fork.2"), Some("0.147.1")),
            Path::new("/home/alice/.codex"),
        ),
        render_case(
            "custom codex home",
            status("0.144.4-fork.2", Some("0.144.4-fork.2"), Some("0.148.0")),
            Path::new("/srv/codex-home"),
        ),
        render_case(
            "unavailable",
            ForkReleaseStatus::unavailable("0.144.4-fork.2".to_string()),
            Path::new("/home/alice/.codex"),
        ),
    ]
    .join("\n\n");

    insta::with_settings!({ snapshot_path => "../snapshots" }, {
        assert_snapshot!(cases);
    });
}
