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
use std::sync::Weak;
use tokio::sync::Mutex;
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
}

impl CompletionWake {
    pub(crate) async fn cancel_for_abort(
        &self,
        reason: &codex_protocol::protocol::TurnAbortReason,
    ) {
        if *reason != codex_protocol::protocol::TurnAbortReason::Replaced {
            self.clear().await;
        }
    }
    pub(crate) async fn clear(&self) {
        self.processes.lock().await.clear();
    }
    pub(crate) async fn observed(&self, id: i32) {
        self.processes.lock().await.remove(&id);
    }
    pub(crate) async fn has_ready(&self) -> bool {
        self.processes.lock().await.values().any(|p| {
            p.upgrade()
                .is_some_and(|p| p.cancellation_token().is_cancelled())
        })
    }
    pub(crate) async fn take_input(&self) -> Vec<TurnInput> {
        let mut pending = self.processes.lock().await;
        let ready: Vec<_> = pending
            .iter()
            .filter_map(|(&id, process)| {
                let process = process.upgrade()?;
                process
                    .cancellation_token()
                    .is_cancelled()
                    .then_some((id, process.exit_code()))
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
        let mut pending = self.completion_wake.processes.lock().await;
        if cancellation.is_cancelled() {
            return;
        }
        pending.insert(id, Arc::downgrade(&process));
        drop(pending);
        let session = Arc::downgrade(session);
        tokio::spawn(async move {
            process.cancellation_token().cancelled().await;
            if let Some(session) = session.upgrade() {
                session
                    .services
                    .unified_exec_manager
                    .completion_wake
                    .notify
                    .notify_one();
            }
        });
    }
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

/// Serialize completion-triggered starts with incoming user operations. User input wins ties.
pub(crate) async fn next_submission(
    session: &Arc<Session>,
    submissions: &async_channel::Receiver<codex_protocol::protocol::Submission>,
) -> Option<codex_protocol::protocol::Submission> {
    loop {
        tokio::select! {
            biased;
            sub = submissions.recv() => return sub.ok(),
            _ = session.services.unified_exec_manager.completion_wake.notify.notified() => {
                session.maybe_start_turn_for_pending_work().await;
            }
        }
    }
}
