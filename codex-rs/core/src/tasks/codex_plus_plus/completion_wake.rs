use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::TurnRunState;
use crate::session::turn::record_claimed_input;
use crate::session::turn::run_turn;
use crate::session::turn_context::TurnContext;
use crate::tasks::RegularTask;
use crate::tasks::SessionTaskResult;
use tracing::Instrument;
use tracing::trace_span;

pub(in crate::tasks) fn regular_task(completion_claim: Option<u64>) -> RegularTask {
    match completion_claim {
        Some(completion_claim) => RegularTask::with_completion_claim(completion_claim),
        None => RegularTask::new(),
    }
}

pub(crate) async fn run_turn_loop(
    sess: Arc<Session>,
    ctx: Arc<TurnContext>,
    mut next_input: Vec<TurnInput>,
    cancellation_token: CancellationToken,
    mut turn_state: TurnRunState,
) -> SessionTaskResult {
    let run_turn_span = trace_span!("run_turn");
    loop {
        let turn_result = run_turn(
            Arc::clone(&sess),
            Arc::clone(&ctx),
            next_input,
            &mut turn_state,
            cancellation_token.child_token(),
        )
        .instrument(run_turn_span.clone())
        .await;
        let last_agent_message = match turn_result {
            Ok(last_agent_message) => last_agent_message,
            Err(err) => {
                if !cancellation_token.is_cancelled() {
                    sess.services.unified_exec_manager.completion_wake.clear();
                }
                return Err(err);
            }
        };
        if turn_state.stopped {
            settle_stopped_turn(&sess, &ctx, &mut turn_state, &cancellation_token).await;
            return Ok(last_agent_message);
        }
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

async fn settle_stopped_turn(
    sess: &Arc<Session>,
    ctx: &Arc<TurnContext>,
    turn_state: &mut TurnRunState,
    cancellation_token: &CancellationToken,
) {
    if cancellation_token.is_cancelled() {
        return;
    }
    settle_completion_claim(sess, ctx, turn_state).await;
    if !cancellation_token.is_cancelled() {
        sess.services.unified_exec_manager.completion_wake.clear();
    }
}

async fn settle_completion_claim(
    sess: &Arc<Session>,
    ctx: &Arc<TurnContext>,
    turn_state: &mut TurnRunState,
) {
    if turn_state.completion_claim.is_none() {
        return;
    }
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
    record_claimed_input(sess, ctx, &pending_input, &mut turn_state.completion_claim).await;
}
