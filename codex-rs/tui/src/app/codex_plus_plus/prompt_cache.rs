use super::App;
use codex_protocol::ThreadId;

impl App {
    pub(super) async fn attach_prompt_cache(&mut self, thread_id: ThreadId) {
        if let Some(channel) = self.thread_event_channels.get(&thread_id) {
            self.chat_widget.prompt_cache = channel.store.lock().await.prompt_cache.clone();
        }
    }
}

#[cfg(test)]
#[path = "prompt_cache_tests.rs"]
mod tests;
