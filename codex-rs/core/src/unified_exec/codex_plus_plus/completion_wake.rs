use super::super::UnifiedExecProcess;
use super::super::UnifiedExecProcessManager;
use crate::context::ContextualUserFragment;
use crate::context::codex_plus_plus::BackgroundCompletion;
use crate::session::TurnInput;
use crate::session::session::Session;
use codex_history::ResponseItemEnvelope;
use codex_tools::JsonSchema;
use codex_tools::ToolSpec;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use tokio::sync::Notify;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OnExit {
    Wake,
}

#[derive(Default)]
pub(crate) struct CompletionWake {
    // Entries are bounded by the existing background-process store. Removal disarms a wake.
    processes: Mutex<BTreeMap<i32, Weak<UnifiedExecProcess>>>,
    pub(crate) notify: Notify,
    idle_notify: Notify,
}

impl CompletionWake {
    pub(crate) fn cancel_for_abort(&self, reason: &codex_protocol::protocol::TurnAbortReason) {
        if *reason != codex_protocol::protocol::TurnAbortReason::Replaced {
            self.clear();
        }
    }
    pub(crate) fn clear(&self) {
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.notify.notify_one();
    }
    pub(crate) fn observed(&self, id: i32) {
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id);
        self.notify.notify_one();
    }
    pub(crate) fn has_ready(&self) -> bool {
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|p| p.upgrade().is_some_and(|p| p.completion().is_some()))
    }
    pub(crate) fn take_input(&self, interrupted: bool) -> Vec<TurnInput> {
        if interrupted {
            self.clear();
            return Vec::new();
        }
        let mut pending = self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let ready: Vec<_> = pending
            .iter()
            .filter_map(|(&id, process)| {
                let process = process.upgrade()?;
                process.completion().map(|code| (id, code))
            })
            .take(8)
            .collect();
        for (id, _) in &ready {
            pending.remove(id);
        }
        if ready.is_empty() {
            return Vec::new();
        }
        vec![TurnInput::ResponseItem(ResponseItemEnvelope::new(
            ContextualUserFragment::into(BackgroundCompletion(ready)),
        ))]
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn ownership and completion transfer must remain atomic"
    )]
    pub(crate) async fn wait_for_input(
        &self,
        session: &Session,
        cancellation: &tokio_util::sync::CancellationToken,
    ) {
        let turn_state = session
            .active_turn
            .lock()
            .await
            .as_ref()
            .map(|turn| Arc::clone(&turn.turn_state));
        let (mut activity, _) = session
            .input_queue
            .subscribe_activity(turn_state.as_deref())
            .await;
        if session
            .input_queue
            .has_pending_turn_input(turn_state.as_deref())
            .await
            || session.input_queue.has_trigger_turn_mailbox_items().await
        {
            return;
        }
        loop {
            let active_turn = session.active_turn.lock().await;
            if !active_turn.as_ref().is_some_and(|active_turn| {
                turn_state
                    .as_ref()
                    .is_some_and(|turn_state| Arc::ptr_eq(&active_turn.turn_state, turn_state))
            }) {
                return;
            }
            let input = self.take_input(session.is_interrupted());
            if !input.is_empty() {
                if let Some(turn_state) = turn_state.as_deref() {
                    turn_state
                        .lock()
                        .await
                        .accept_mailbox_delivery_for_current_turn();
                    session
                        .input_queue
                        .extend_pending_input_for_turn_state(turn_state, input)
                        .await;
                }
                return;
            }
            drop(active_turn);
            if self
                .processes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
            {
                return;
            }
            tokio::select! {
                _ = self.notify.notified() => {}
                result = activity.changed() => {
                    if result.is_err()
                        || session
                            .input_queue
                            .has_pending_turn_input(turn_state.as_deref())
                            .await
                        || session.input_queue.has_trigger_turn_mailbox_items().await
                    {
                        return;
                    }
                }
                _ = cancellation.cancelled() => return,
            }
        }
    }
}

impl UnifiedExecProcessManager {
    pub(crate) async fn wake_on_exit(
        &self,
        session: &Arc<Session>,
        id: i32,
        cancellation: &tokio_util::sync::CancellationToken,
    ) {
        let store = self.process_store.lock().await;
        let Some(entry) = store.processes.get(&id) else {
            return;
        };
        let process = Arc::clone(&entry.process);
        let mut pending = self
            .completion_wake
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cancellation.is_cancelled() || session.is_interrupted() {
            return;
        }
        pending.insert(id, Arc::downgrade(&process));
        drop(pending);
        let completion = process.wait_for_completion();
        let session = Arc::downgrade(session);
        tokio::spawn(async move {
            completion.await;
            if let Some(session) = session.upgrade() {
                let completion_wake = &session.services.unified_exec_manager.completion_wake;
                completion_wake.notify.notify_one();
                completion_wake.idle_notify.notify_one();
            }
        });
    }
}

pub(crate) fn supported_source(source: &codex_protocol::protocol::SessionSource) -> bool {
    // Exec shuts down on final; parents treat a subagent final as completed and may unload it.
    !matches!(source, codex_protocol::protocol::SessionSource::Exec) && !source.is_non_root_agent()
}

pub(crate) fn add_wake_option(mut spec: ToolSpec, enabled: bool) -> ToolSpec {
    // The one-turn exec host exits on the first final response.
    if !enabled {
        return spec;
    }
    if let ToolSpec::Function(spec) = &mut spec {
        spec.parameters.properties.get_or_insert_default().insert("on_exit".into(),
            JsonSchema::string_enum(vec![serde_json::json!("wake")], Some("Set to 'wake' for a finite background command. If it outlives this call, completion resumes the thread automatically. Do independent work or finish the turn; do not poll. Omit for servers and interactive commands.".into())));
    }
    spec
}

/// Preserve completion delivery when another task temporarily replaces the owning turn.
pub(crate) async fn next_submission(
    session: &Arc<Session>,
    submissions: &async_channel::Receiver<codex_protocol::protocol::Submission>,
) -> Option<codex_protocol::protocol::Submission> {
    loop {
        tokio::select! {
            biased;
            sub = submissions.recv() => return sub.ok(),
            _ = session.services.unified_exec_manager.completion_wake.idle_notify.notified() => {
                session.maybe_start_turn_for_pending_work().await;
            }
        }
    }
}
