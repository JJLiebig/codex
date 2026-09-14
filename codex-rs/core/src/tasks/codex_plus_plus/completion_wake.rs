use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::session::session::Session;
use crate::session::turn::TurnRunState;
use crate::session::turn::run_hooks_and_record_inputs;
use crate::session::turn_context::TurnContext;
use codex_thread_store::PersistContext;

pub(crate) async fn settle_stopped_turn(
    sess: &Arc<Session>,
    ctx: &Arc<TurnContext>,
    turn_state: &mut TurnRunState,
    cancellation_token: &CancellationToken,
) {
    if cancellation_token.is_cancelled() {
        return;
    }
    settle_completion_claim(sess, ctx, turn_state).await;
    sess.services.unified_exec_manager.completion_wake.clear();
}

pub(crate) async fn settle_completion_claim(
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
