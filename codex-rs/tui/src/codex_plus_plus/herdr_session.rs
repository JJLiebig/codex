//! Private OSC 7501 extension carrying the app-owned primary conversation identity.

use std::fmt;
use std::sync::OnceLock;

use codex_protocol::ThreadId;

use super::CachedStatus;
use super::SESSION_STATUS_STATE;
use super::SessionStatusState;
use crate::session_state::ThreadSessionState;

pub(crate) fn set_primary_session(session: Option<&ThreadSessionState>) {
    static IN_HERDR: OnceLock<bool> = OnceLock::new();
    if *IN_HERDR.get_or_init(|| {
        ["HC_TERMINAL_ID", "HC_TERMINAL_SOCKET"]
            .iter()
            .all(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
    }) {
        // A native rollout destination marks a persistable thread, not proof it has been written.
        SESSION_STATUS_STATE.set_herdr_session(
            session.and_then(|session| session.rollout_path.as_ref().map(|_| session.thread_id)),
        );
    }
    crate::codex_plus_plus::herdr_recovery::set_primary(session);
}

// Registration is asynchronous: the first OSC report can reach core before recovery is armed.
// Repeat an already reported status after the durable registration acknowledgement.
pub(crate) fn refresh_recovery_binding() {
    if cfg!(test) {
        return;
    }
    let _ = SESSION_STATUS_STATE.reemit_recovery_binding(|status, emitted| {
        crossterm::execute!(
            std::io::stdout(),
            super::SetTerminalProgramStatus {
                status,
                iterm_detail: &emitted.detail,
                herdr_session: emitted.herdr_session,
                emit_iterm_status: super::current_terminal_program_status_support().osc_21337,
            }
        )
    });
}

pub(super) fn record_status(emitted: &mut super::EmittedSessionStatus, status: CachedStatus) {
    match status {
        CachedStatus::Reported(status) => emitted.herdr_reported_status = Some(status),
        CachedStatus::Cleared => emitted.herdr_reported_status = None,
        CachedStatus::Invalidated => {}
    }
}

impl SessionStatusState {
    fn reemit_recovery_binding(
        &self,
        emit: impl FnOnce(super::ProgramStatus, &super::EmittedSessionStatus) -> std::io::Result<()>,
    ) -> std::io::Result<()> {
        let emitted = self
            .last_emitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let status = match emitted.status {
            CachedStatus::Reported(status) => Some(status),
            CachedStatus::Invalidated => emitted.herdr_reported_status,
            CachedStatus::Cleared => None,
        };
        if let Some(status) = status {
            emit(status, &emitted)?;
        }
        Ok(())
    }

    fn set_herdr_session(&self, session: Option<ThreadId>) {
        let mut emitted = self
            .last_emitted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if emitted.herdr_session != session {
            emitted.herdr_session = session;
            emitted.status = CachedStatus::Invalidated;
        }
    }
}

pub(super) fn write_session(f: &mut impl fmt::Write, session: Option<ThreadId>) -> fmt::Result {
    if let Some(session) = session {
        write!(f, ":session={session}")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "herdr_session_tests.rs"]
mod tests;
