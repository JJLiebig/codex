use super::RuntimeFacts;
use super::ThreadWatchManager;
use codex_app_server_protocol::ThreadActiveFlag;

impl ThreadWatchManager {
    pub(crate) async fn on_background_completion_wait(
        &self,
        thread_id: &str,
        turn_id: &str,
        state: &tokio::sync::Mutex<crate::thread_state::ThreadState>,
        waiting: bool,
    ) {
        if state
            .lock()
            .await
            .active_turn_snapshot()
            .is_some_and(|turn| turn.id == turn_id)
        {
            self.note_background_completion_waiting(thread_id, waiting)
                .await;
        }
    }

    async fn note_background_completion_waiting(&self, thread_id: &str, waiting: bool) {
        self.mutate_and_publish(|state| {
            // A late wait update must not reload a stopped thread.
            if !state.runtime_by_thread_id.get(thread_id)?.running {
                return None;
            }
            state.update_runtime(thread_id, |runtime| {
                runtime.background_completion_waiting = waiting;
            })
        })
        .await;
    }
}

pub(super) fn add_active_flag(runtime: &RuntimeFacts, flags: &mut Vec<ThreadActiveFlag>) {
    if runtime.running && runtime.background_completion_waiting {
        flags.push(ThreadActiveFlag::WaitingOnBackgroundCompletion);
    }
}

#[cfg(test)]
#[path = "completion_wait_tests.rs"]
mod tests;
