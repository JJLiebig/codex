use super::App;
use codex_protocol::ThreadId;

impl App {
    pub(super) async fn prompt_cache_for_thread(
        &self,
        thread_id: ThreadId,
    ) -> crate::codex_plus_plus::prompt_cache::PromptCache {
        match self.thread_event_channels.get(&thread_id) {
            Some(channel) => channel.store.lock().await.prompt_cache.clone(),
            None => Default::default(),
        }
    }

    pub(super) async fn attach_prompt_cache(&mut self, thread_id: ThreadId) {
        self.chat_widget.prompt_cache = self.prompt_cache_for_thread(thread_id).await;
    }
}

#[cfg(test)]
#[path = "prompt_cache_tests.rs"]
mod tests;
