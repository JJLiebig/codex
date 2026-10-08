//! Reserve mailbox and completion-triggered turns under the same admission lock.
use super::*;

#[expect(
    clippy::await_holding_invalid_type,
    reason = "capture pending-work attribution atomically with its active reservation"
)]
pub(super) async fn reserve(
    session: &Session,
) -> Option<(
    Arc<tokio::sync::Mutex<TurnState>>,
    TurnStartOptions,
    Option<u64>,
    Vec<TurnInput>,
)> {
    let mut active_turn = session.active_turn.lock().await;
    if active_turn.is_some() {
        return None;
    }
    let needs_new_turn = session.input_queue.has_trigger_turn_mailbox_items().await;
    let mailbox_ready = session.input_queue.has_pending_mailbox_items().await
        && (needs_new_turn || session.has_outstanding_durable_sleep());
    let claimed_completion = session
        .services
        .unified_exec_manager
        .completion_wake
        .claim_input(session.is_interrupted());
    if claimed_completion.is_none() && !mailbox_ready {
        return None;
    }
    let (completion_claim, completions) = claimed_completion
        .map(|(claim, input)| (Some(claim), input))
        .unwrap_or_default();
    let previous_options = if needs_new_turn {
        Default::default()
    } else {
        session
            .state
            .lock()
            .await
            .turn_attribution
            .as_ref()
            .map(codex_history::TurnAttribution::start_options)
            .unwrap_or_default()
    };
    let active_turn = active_turn.get_or_insert_with(ActiveTurn::default);
    Some((
        Arc::clone(&active_turn.turn_state),
        previous_options,
        completion_claim,
        completions,
    ))
}
