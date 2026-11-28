//! Authenticate recovery commands for the app-owned primary conversation.

use std::io::IsTerminal;
use std::io::Write;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use codex_app_server_protocol::ServerNotification;
use codex_protocol::SessionId;
use codex_protocol::ThreadId;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadSource;
use sha2::Digest;
use sha2::Sha256;

use crate::AppServerTarget;
use crate::Cli;
use crate::session_state::ThreadSessionState;

#[path = "terminal_resume_launch.rs"]
mod launch;

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
        .filter(|(path, _)| reporter.launch.owns_rollout(path))
        .filter(|_| {
            !target.uses_remote_workspace()
                && !session.read_only_history
                && session
                    .primary_environment_cwd
                    .as_ref()
                    .is_none_or(|cwd| cwd.is_server_local)
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
    let Some(path) = codex_rollout::existing_rollout_path(path).await else {
        clear();
        return;
    };
    let Ok(meta) = codex_rollout::read_session_meta_line(&path).await else {
        clear();
        return;
    };
    if meta.meta.id != id
        || meta.meta.session_id != SessionId::from(id)
        || meta.meta.attached_thread
        || meta.meta.parent_thread_id.is_some()
        || matches!(
            meta.meta.source,
            SessionSource::SubAgent(_) | SessionSource::Internal(_)
        )
        || !matches!(meta.meta.thread_source, None | Some(ThreadSource::User))
    {
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
