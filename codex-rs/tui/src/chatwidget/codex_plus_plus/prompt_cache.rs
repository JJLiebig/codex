use super::ChatWidget;
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
        // Historical replay must neither contribute usage nor re-emit a miss notice.
        if replay_kind.is_none() {
            self.prompt_cache.observe_completion(&completed);
        }
        let miss = self.prompt_cache.take_miss(&completed.response_id);
        if miss && replay_kind.is_none() {
            self.add_info_message("Prompt cache miss.".into(), /*hint*/ None);
        }
    }
}
