//! Report the displayed session independently of title and notification settings.

use super::super::ChatWidget;
use crate::codex_plus_plus::program_status::Kind;
use crate::codex_plus_plus::program_status::State;
use crate::codex_plus_plus::program_status::Status;
use codex_app_server_protocol::TurnStatus;

impl ChatWidget {
    pub(in crate::chatwidget) fn record_program_status_completion(
        &mut self,
        status: &TurnStatus,
        from_replay: bool,
    ) {
        if !from_replay {
            self.program_status.resting = match status {
                TurnStatus::Completed => State::Done,
                TurnStatus::Interrupted => State::Idle,
                TurnStatus::Failed => State::Error,
                TurnStatus::InProgress => return,
            };
        }
    }

    pub(in crate::chatwidget) fn current_program_status(&self) -> Status {
        let kind = self
            .bottom_pane
            .program_status_kind()
            .or_else(|| self.plugin_install_auth_flow.as_ref().map(|_| Kind::Auth));
        let state = if kind.is_some() {
            State::Blocked
        } else if self.bottom_pane.is_task_running() || self.status_state.compaction.is_some() {
            State::Working
        } else {
            self.program_status.resting
        };
        let message = match (state, kind) {
            (State::Blocked, Some(Kind::Permission)) => Some("Approval required".to_string()),
            (State::Blocked, Some(Kind::Question)) => Some("Input required".to_string()),
            (State::Blocked, Some(Kind::Auth)) => Some("Authentication required".to_string()),
            (State::Error, _) => Some("Run failed".to_string()),
            (State::Working, _) if self.status_state.compaction.is_some() => {
                Some("Compacting context".to_string())
            }
            (State::Working | State::Done, _) => self.thread_name.clone(),
            (State::Idle | State::Clear | State::Blocked, _) => None,
        };
        Status {
            state,
            kind,
            message,
        }
    }

    pub(crate) fn refresh_program_status(&mut self) {
        self.program_status.publish(self.current_program_status());
    }
}

#[cfg(test)]
#[path = "program_status_tests.rs"]
mod tests;
