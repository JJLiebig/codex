use super::ChatWidget;
use codex_app_server_protocol::ThreadActiveFlag;
use codex_app_server_protocol::ThreadStatus;

impl ChatWidget {
    pub(in crate::chatwidget) fn on_background_completion_status(&mut self, status: &ThreadStatus) {
        self.restore_background_completion_wait(
            matches!(status, ThreadStatus::Active { active_flags }
            if active_flags.contains(&ThreadActiveFlag::WaitingOnBackgroundCompletion)),
        );
    }

    pub(crate) fn restore_background_completion_wait(&mut self, waiting: bool) {
        if waiting && self.bottom_pane.is_task_running() {
            self.set_status_header("Working".to_string());
            self.status_state.pending_status_indicator_restore = true;
            self.maybe_restore_status_indicator_after_stream_idle();
            self.request_redraw();
        }
    }
}
