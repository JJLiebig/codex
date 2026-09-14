use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::RunTurnResult;
use crate::session::turn::TurnRunState;
use crate::session::turn::run_hooks_and_record_inputs;
use crate::session::turn::run_turn;
use crate::session::turn_context::TurnContext;
use crate::session_startup_prewarm::SessionStartupPrewarmResolution;
use crate::state::TaskKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnStartedEvent;
use codex_thread_store::PersistContext;
use tracing::Instrument;
use tracing::trace_span;

use super::SessionTask;
use super::SessionTaskResult;

#[derive(Default)]
pub(crate) struct RegularTask {
    completion_claim: Option<u64>,
}

impl RegularTask {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_completion_claim(completion_claim: u64) -> Self {
        Self {
            completion_claim: Some(completion_claim),
        }
    }
}

impl SessionTask for RegularTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.turn"
    }

    async fn run(
        self: Arc<Self>,
        sess: Arc<Session>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        let run_turn_span = trace_span!("run_turn");
        // Regular turns emit `TurnStarted` inline so first-turn lifecycle does
        // not wait on startup prewarm resolution.
        let prewarmed_client_session = async {
            let event = EventMsg::TurnStarted(TurnStartedEvent {
                turn_id: ctx.sub_id.clone(),
                trace_id: ctx.trace_id.clone(),
                started_at: ctx.turn_timing_state.started_at_unix_secs().await,
                model_context_window: ctx.model_context_window(),
                collaboration_mode_kind: ctx.mode(),
            });
            sess.send_event(ctx.as_ref(), event).await;
            sess.set_server_reasoning_included(/*included*/ false).await;
            sess.consume_startup_prewarm_for_regular_turn(&cancellation_token)
                .await
        }
        .instrument(trace_span!("regular_task.prepare_run_turn"))
        .await;
        let prewarmed_client_session = match prewarmed_client_session {
            SessionStartupPrewarmResolution::Cancelled => {
                run_hooks_and_record_inputs(&sess, &ctx, &input, PersistContext::Standard).await;
                return Ok(None);
            }
            SessionStartupPrewarmResolution::Unavailable { .. } => None,
            SessionStartupPrewarmResolution::Ready(prewarmed_client_session) => {
                Some(*prewarmed_client_session)
            }
        };
        let mut next_input = input;
        let mut prewarmed_client_session = prewarmed_client_session;
        let mut turn_state = TurnRunState::default();
        turn_state.completion_claim = self.completion_claim;
        loop {
            let turn_result = run_turn(
                Arc::clone(&sess),
                Arc::clone(&ctx),
                next_input,
                &mut turn_state,
                prewarmed_client_session.take(),
                cancellation_token.child_token(),
            )
            .instrument(run_turn_span.clone())
            .await;
            let last_agent_message = match turn_result {
                Ok(RunTurnResult::Completed(last_agent_message)) => last_agent_message,
                Ok(RunTurnResult::Stopped(last_agent_message)) => {
                    if !cancellation_token.is_cancelled() {
                        settle_completion_claim(&sess, &ctx, &mut turn_state).await;
                        sess.services.unified_exec_manager.completion_wake.clear();
                    }
                    return Ok(last_agent_message);
                }
                Err(err) => {
                    if !cancellation_token.is_cancelled() {
                        sess.services.unified_exec_manager.completion_wake.clear();
                    }
                    return Err(err);
                }
            };
            // Terminal errors are already reported. Let task completion preserve pending
            // input instead of restarting the failed turn for that same input.
            if ctx.terminal_error.lock().await.is_some() {
                if !cancellation_token.is_cancelled() {
                    sess.services.unified_exec_manager.completion_wake.clear();
                }
                return Ok(last_agent_message);
            }
            if turn_state.completion_claim.is_some() {
                if !cancellation_token.is_cancelled() {
                    settle_completion_claim(&sess, &ctx, &mut turn_state).await;
                }
                return Ok(last_agent_message);
            }
            turn_state.completion_claim = sess
                .services
                .unified_exec_manager
                .completion_wake
                .wait_for_input(&sess, &cancellation_token)
                .await;
            if !sess.input_queue.has_pending_input(&sess.active_turn).await {
                return Ok(last_agent_message);
            }
            next_input = Vec::new();
        }
    }
}

async fn settle_completion_claim(
    sess: &Arc<Session>,
    ctx: &Arc<TurnContext>,
    turn_state: &mut TurnRunState,
) {
    let Some(claim) = turn_state.completion_claim else {
        return;
    };
    let Some(pending_turn_state) = sess
        .input_queue
        .turn_state_for_sub_id(&sess.active_turn, &ctx.sub_id)
        .await
    else {
        return;
    };
    let pending_input = sess
        .input_queue
        .take_pending_input_for_turn_state(pending_turn_state.as_ref())
        .await;
    if pending_input.is_empty() {
        return;
    }
    run_hooks_and_record_inputs(sess, ctx, &pending_input, PersistContext::Standard).await;
    sess.services
        .unified_exec_manager
        .completion_wake
        .commit_claim(claim);
    turn_state.completion_claim = None;
}
