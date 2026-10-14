use super::ChatWidget;
use crate::codex_plus_plus::prompt_cache::CacheObservation;
use codex_app_server_protocol::RawResponseCompletedNotification;

impl ChatWidget {
    pub(in crate::chatwidget) fn on_prompt_cache_completion(
        &mut self,
        completed: RawResponseCompletedNotification,
        replay_kind: Option<crate::chatwidget::ReplayKind>,
    ) {
        if self.thread_id.map(|id| id.to_string()).as_deref() != Some(&completed.thread_id) {
            return;
        }
        // Historical replay must neither contribute usage nor re-emit a miss indication.
        self.prompt_cache.observe_completion(
            &completed,
            if replay_kind.is_none() {
                CacheObservation::Active
            } else {
                CacheObservation::Replay
            },
        );
        if replay_kind.is_none() {
            self.refresh_prompt_cache_status_line();
            self.request_redraw();
        }
    }

    pub(in crate::chatwidget) fn refresh_prompt_cache_status_line(&mut self) {
        if self
            .local_settings
            .tui
            .status_line
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| item == "prompt-cache"))
        {
            self.refresh_status_line();
            if let Some(delay) = self.prompt_cache.status_line().1 {
                self.frame_requester.schedule_frame_in(delay);
            }
        }
    }
}

#[cfg(test)]
#[path = "prompt_cache_tests.rs"]
mod tests;
