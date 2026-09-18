use super::super::UnifiedExecProcess;
use super::super::UnifiedExecProcessManager;
use crate::context::ContextualUserFragment;
use crate::context::codex_plus_plus::BackgroundCompletion;
use crate::context::codex_plus_plus::BackgroundProcessExit;
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
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tokio::sync::Notify;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OnExit {
    Wake,
}

#[derive(Default)]
pub(crate) struct CompletionWake {
    // Entries are bounded by the existing background-process store. Removal disarms a wake.
    processes: Mutex<BTreeMap<i32, CompletionEntry>>,
    next_claim: AtomicU64,
    pub(crate) notify: Notify,
    idle_notify: Notify,
    idle_wait_interrupted: AtomicBool,
}

struct CompletionEntry {
    process: Weak<UnifiedExecProcess>,
    result: Option<BackgroundProcessExit>,
    claim: Option<u64>,
}

impl CompletionWake {
    pub(crate) fn cancel_for_abort(&self, reason: &codex_protocol::protocol::TurnAbortReason) {
        if *reason == codex_protocol::protocol::TurnAbortReason::Replaced {
            for entry in self
                .processes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values_mut()
            {
                entry.claim = None;
            }
            self.notify.notify_one();
        } else {
            self.clear();
        }
    }
    pub(crate) fn clear(&self) {
        self.idle_wait_interrupted.store(false, Ordering::Release);
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
            .any(|entry| entry.claim.is_none() && entry.result.is_some())
    }
    pub(crate) fn claim_input(&self, interrupted: bool) -> Option<(u64, Vec<TurnInput>)> {
        if interrupted {
            self.clear();
            return None;
        }
        let mut pending = self
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let claim = self.next_claim.fetch_add(1, Ordering::Relaxed);
        let ready: Vec<_> = pending
            .iter_mut()
            .filter_map(|(&id, entry)| {
                if entry.claim.is_some() {
                    return None;
                }
                entry.result.clone().map(|result| (id, result))
            })
            .take(8)
            .collect();
        for (id, _) in &ready {
            if let Some(entry) = pending.get_mut(id) {
                entry.claim = Some(claim);
            }
        }
        if ready.is_empty() {
            return None;
        }
        Some((
            claim,
            vec![TurnInput::ResponseItem(ResponseItemEnvelope::new(
                ContextualUserFragment::into(BackgroundCompletion(
                    ready.into_iter().map(|(_, result)| result).collect(),
                )),
            ))],
        ))
    }

    /// Interrupt only the owned idle waits, never unrelated active tools.
    pub(crate) async fn interrupt_idle_wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.has_ready() {
                self.idle_wait_interrupted.store(true, Ordering::Release);
                return;
            }
            notified.await;
        }
    }

    pub(crate) fn input_after_idle_wait(
        &self,
        interrupted: bool,
        claim: &mut Option<u64>,
    ) -> Vec<TurnInput> {
        if !self.idle_wait_interrupted.swap(false, Ordering::AcqRel) {
            return Vec::new();
        }
        let Some((new_claim, input)) = self.claim_input(interrupted) else {
            return Vec::new();
        };
        *claim = Some(new_claim);
        input
    }

    pub(crate) fn commit_claim(&self, claim: u64) {
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|_, entry| entry.claim != Some(claim));
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "active turn ownership and completion transfer must remain atomic"
    )]
    pub(crate) async fn wait_for_input(
        &self,
        session: &Session,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Option<u64> {
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
            return None;
        }
        loop {
            let active_turn = session.active_turn.lock().await;
            if !active_turn.as_ref().is_some_and(|active_turn| {
                turn_state
                    .as_ref()
                    .is_some_and(|turn_state| Arc::ptr_eq(&active_turn.turn_state, turn_state))
            }) {
                return None;
            }
            if let Some((claim, input)) = self.claim_input(session.is_interrupted()) {
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
                return Some(claim);
            }
            drop(active_turn);
            if self
                .processes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
            {
                return None;
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
                        return None;
                    }
                }
                _ = cancellation.cancelled() => return None,
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
    ) -> bool {
        let store = self.process_store.lock().await;
        let Some(entry) = store.processes.get(&id) else {
            return false;
        };
        let process = Arc::clone(&entry.process);
        let mut pending = self
            .completion_wake
            .processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cancellation.is_cancelled() || session.is_interrupted() {
            return false;
        }
        pending.insert(
            id,
            CompletionEntry {
                process: Arc::downgrade(&process),
                result: None,
                claim: None,
            },
        );
        drop(pending);
        let completion = process.wait_for_completion();
        let session = Arc::downgrade(session);
        let process = Arc::downgrade(&process);
        tokio::spawn(async move {
            completion.await;
            let Some(process) = process.upgrade() else {
                return;
            };
            let Some(exit_code) = process.completion() else {
                return;
            };
            let result = process.completion_output(id, exit_code).await;
            if let Some(session) = session.upgrade() {
                let completion_wake = &session.services.unified_exec_manager.completion_wake;
                if let Some(entry) = completion_wake
                    .processes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get_mut(&id)
                    && entry.process.ptr_eq(&Arc::downgrade(&process))
                {
                    entry.result = Some(result);
                }
                completion_wake.notify.notify_waiters();
                completion_wake.notify.notify_one();
                completion_wake.idle_notify.notify_one();
            }
        });
        true
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
            JsonSchema::string_enum(vec![serde_json::json!("wake")], Some("Set to 'wake' for a finite background command. If it outlives this call, completion resumes the thread automatically. Do independent work or end this turn. Do not sleep, wait, or poll for this session. Omit for servers and interactive commands.".into())));
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

#[cfg(test)]
#[path = "completion_wake_tests.rs"]
mod tests;
