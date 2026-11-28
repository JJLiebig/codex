//! Authenticate recovery commands for the app-owned primary conversation.

use std::io::IsTerminal;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use codex_app_server_protocol::ServerNotification;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::models::ActivePermissionProfile;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::FileSystemPath;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadSource;
use codex_rollout::ReverseJsonlScanner;
use codex_rollout::RolloutItem;
use codex_rollout::ScanOutcome;
use sha2::Digest;
use sha2::Sha256;

use crate::AppServerTarget;
use crate::Cli;
use crate::session_state::ThreadSessionState;

#[path = "terminal_resume_launch.rs"]
mod launch;

pub use launch::apply_home;
pub(crate) use launch::startup_model_settings;

static REPORTER: OnceLock<Reporter> = OnceLock::new();

struct Reporter {
    launch: launch::Launch,
    reported: Mutex<Option<(ThreadId, String)>>,
    token: &'static str,
}

pub(crate) fn initialize(cli: &Cli) {
    if let Some(token) = codex_arg0::program_resume_token::get()
        && std::io::stdout().is_terminal()
        && let Some(launch) = launch::Launch::new(cli)
    {
        REPORTER.get_or_init(|| Reporter {
            launch,
            reported: Mutex::default(),
            token,
        });
    }
}

pub(crate) async fn update(target: &AppServerTarget, session: &ThreadSessionState) {
    let Some(reporter) = REPORTER.get() else {
        return;
    };
    let cwd = session.cwd.to_abs_path().ok();
    let Some((path, cwd)) = session
        .rollout_path
        .as_deref()
        .zip(cwd.as_deref().and_then(std::path::Path::to_str))
        .filter(|(path, _)| path.starts_with(&reporter.launch.sessions))
        .filter(|_| {
            !target.uses_remote_workspace()
                && !session.read_only_history
                && session
                    .primary_environment_cwd
                    .as_ref()
                    .is_none_or(|cwd| cwd.is_server_local)
                && permission_profile_is_resumable(
                    &session.permission_profile,
                    session.active_permission_profile.as_ref(),
                )
        })
    else {
        clear();
        return;
    };
    let id = session.thread_id;
    if reporter
        .reported
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|(prev, dir)| *prev == id && dir == cwd)
    {
        return;
    }
    if !saved_root_has_settings(path, id).await {
        clear();
        return;
    }
    let mut argv = reporter.launch.argv.clone();
    argv.push(id.to_string());
    argv.extend(reporter.launch.options.iter().cloned());
    if argv.len() > 64
        || argv
            .iter()
            .map(String::as_str)
            .chain([cwd])
            .any(|arg| arg.contains(reporter.token) || arg.chars().any(char::is_control))
    {
        clear();
        return;
    }
    let argv = argv
        .iter()
        .map(|arg| STANDARD.encode(arg))
        .collect::<Vec<_>>()
        .join(",");
    let cwd_encoded = STANDARD.encode(cwd);
    let mut reported = reporter.reported.lock().unwrap();
    match reporter.send(&format!("need=cwd:argv={argv}:cwd={cwd_encoded}")) {
        Some(true) => *reported = Some((id, cwd.to_owned())),
        Some(false) => {}
        None => {
            drop(reported);
            clear();
        }
    }
}

async fn saved_root_has_settings(path: &Path, id: ThreadId) -> bool {
    let Some(path) = codex_rollout::existing_rollout_path(path).await else {
        return false;
    };
    tokio::task::spawn_blocking(move || {
        // Naming can save just the metadata. Require the owned settings checkpoint, which
        // new rollouts write ahead of the first input, without traversing a long history.
        const CHECKPOINT_PREFIX_BYTES: usize = 256 * 1024;
        let Ok(Some(prefix)) = codex_rollout::read_rollout_prefix(&path, CHECKPOINT_PREFIX_BYTES) else {
            return false;
        };
        let owns_settings = |item| matches!(item, RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event))
            if event.thread_id == Some(id));
        let mut items = prefix
            .split_inclusive(|byte| *byte == b'\n')
            .filter(|line| line.ends_with(b"\n"))
            .filter_map(|line| codex_rollout::parse_rollout_line_bytes(line).ok().map(|line| line.item));
        if !matches!(items.next(), Some(RolloutItem::SessionMeta(meta))
            if meta.meta.id == id
                && meta.meta.session_id == SessionId::from(id)
                && !meta.meta.attached_thread
                && meta.meta.parent_thread_id.is_none()
                && !matches!(meta.meta.source, SessionSource::SubAgent(_) | SessionSource::Internal(_))
                && matches!(meta.meta.thread_source, None | Some(ThreadSource::User)))
        {
            return false;
        }
        if items.any(owns_settings) {
            return true;
        }
        // Copied forks and older histories can have their first owned checkpoint at the end.
        if prefix.len() < CHECKPOINT_PREFIX_BYTES || !path.extension().is_some_and(|ext| ext == "jsonl") {
            return false;
        }
        let Ok(scanner) = std::fs::File::open(&path).and_then(ReverseJsonlScanner::new) else {
            return false;
        };
        let mut scanner = scanner.with_max_record_bytes(CHECKPOINT_PREFIX_BYTES);
        (0..32).any(|_| matches!(scanner.scan_next_rollout_line(),
            Ok(Some(ScanOutcome::Parsed(line))) if owns_settings(line.item)))
    })
    .await
    .unwrap_or(false)
}

fn permission_profile_is_resumable(
    profile: &PermissionProfile,
    active: Option<&ActivePermissionProfile>,
) -> bool {
    !matches!(profile, PermissionProfile::External { .. })
        && (active.is_some()
            // Normal resume rejects unnamed profiles with materialized read/write grants.
            // Arming in that case could restart under a broader configured default.
            || !profile.file_system_sandbox_policy().entries.iter().any(|entry| {
                entry.access.can_read()
                    && matches!(entry.path, FileSystemPath::Path { .. } | FileSystemPath::GlobPattern { .. })
            }))
}

#[cfg(test)]
#[path = "terminal_resume_tests.rs"]
mod tests;

pub(crate) async fn observe(
    target: &AppServerTarget,
    session: &ThreadSessionState,
    notification: &ServerNotification,
) {
    match notification {
        ServerNotification::ThreadArchived(_) | ServerNotification::ThreadDeleted(_) => clear(),
        ServerNotification::ThreadNameUpdated(_)
        | ServerNotification::ThreadSettingsUpdated(_)
        | ServerNotification::ThreadGoalUpdated(_)
        | ServerNotification::TurnStarted(_)
        | ServerNotification::ItemCompleted(_)
        | ServerNotification::TurnCompleted(_) => update(target, session).await,
        _ => {}
    }
}

pub(crate) fn clear() {
    if let Some(reporter) = REPORTER.get() {
        let mut reported = reporter.reported.lock().unwrap();
        if reported.is_some() && reporter.send("clear=1") == Some(true) {
            *reported = None;
        }
    }
}

impl Reporter {
    fn send(&self, fields: &str) -> Option<bool> {
        let pid = std::process::id();
        let time = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
        let body = format!("pid={pid}:time={time}:{fields}");
        let mut mac = Sha256::new();
        mac.update(body.as_bytes());
        mac.update(b":");
        mac.update(self.token.as_bytes());
        let digest = mac.finalize();
        let frame = format!("\x1b]7577;{body}:mac={digest:x}\x1b\\");
        // A truncated report is ignored by the terminal and would leave the old record in place.
        if frame.len() > 2048 {
            return None;
        }
        let mut terminal = std::io::stdout().lock();
        Some(
            terminal
                .write_all(frame.as_bytes())
                .and_then(|()| terminal.flush())
                .is_ok(),
        )
    }
}
