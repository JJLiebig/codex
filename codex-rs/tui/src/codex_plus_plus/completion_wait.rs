use super::ThreadSessionState;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::ThreadActiveFlag;
use codex_app_server_protocol::ThreadStatus;

impl ThreadSessionState {
    pub(crate) fn with_completion_wait_status(mut self, status: &ThreadStatus) -> Self {
        self.background_completion_waiting = matches!(status, ThreadStatus::Active { active_flags }
            if active_flags.contains(&ThreadActiveFlag::WaitingOnBackgroundCompletion));
        self
    }

    pub(crate) fn observe_completion_wait(&mut self, notification: &ServerNotification) {
        match notification {
            ServerNotification::ThreadStatusChanged(status) => {
                self.background_completion_waiting = matches!(&status.status, ThreadStatus::Active { active_flags }
                    if active_flags.contains(&ThreadActiveFlag::WaitingOnBackgroundCompletion));
            }
            ServerNotification::TurnStarted(_)
            | ServerNotification::TurnCompleted(_)
            | ServerNotification::ThreadClosed(_) => {
                self.background_completion_waiting = false;
            }
            _ => {}
        }
    }
}
