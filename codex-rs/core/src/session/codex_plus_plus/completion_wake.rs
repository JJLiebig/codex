use super::*;
use crate::context::codex_plus_plus::BackgroundCompletion;

#[derive(Default)]
pub(crate) struct TurnRunState {
    pub(super) mcp_startup_requirements: McpStartupRequirements,
    pub(super) turn_diff_tracker: Option<SharedTurnDiffTracker>,
    pub(super) usage_limit_account_attempts: HashSet<String>,
    pub(crate) completion_claim: Option<u64>,
    pub(super) client_session: Option<ModelClientSession>,
    pub(super) stop_hook_active: bool,
    pub(crate) stopped: bool,
}

impl TurnRunState {
    pub(crate) fn from_prewarmed_client_session(
        client_session: Option<ModelClientSession>,
    ) -> Self {
        Self {
            client_session,
            ..Default::default()
        }
    }

    pub(super) fn is_continuation(&self) -> bool {
        self.turn_diff_tracker.is_some()
    }

    pub(super) fn stop(&mut self) {
        self.stopped = true;
    }
}

pub(super) fn turn_diff_tracker(
    turn_diff_tracker: &mut Option<SharedTurnDiffTracker>,
    display_roots: Vec<(String, PathUri)>,
) -> SharedTurnDiffTracker {
    Arc::clone(turn_diff_tracker.get_or_insert_with(|| {
        Arc::new(tokio::sync::Mutex::new(
            TurnDiffTracker::with_environment_display_roots(display_roots),
        ))
    }))
}

pub(super) async fn track_initial_analytics(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    input: &[TurnInput],
    is_continuation: bool,
) {
    if !is_continuation {
        track_turn_resolved_config_analytics(sess, turn_context, input).await;
    }
}

pub(super) async fn pending_input(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    is_continuation: bool,
) -> Vec<TurnInput> {
    if !is_continuation {
        return Vec::new();
    }
    let Some(pending_turn_state) = sess
        .input_queue
        .turn_state_for_sub_id(&sess.active_turn, &turn_context.sub_id)
        .await
    else {
        return Vec::new();
    };
    sess.input_queue
        .pending_input_for_turn_state(pending_turn_state.as_ref())
        .await
}

pub(super) async fn record_cancelled_input(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    input: &[TurnInput],
    continuation_input: &[TurnInput],
    cancellation_token: &CancellationToken,
    completion_claim: &mut Option<u64>,
) {
    let records_continuation = cancellation_token.is_cancelled() && !continuation_input.is_empty();
    let input = if records_continuation {
        continuation_input
    } else {
        input
    };
    run_hooks_and_collect_inputs(
        sess,
        turn_context,
        &turn_context.capture_current_model_info(),
        input,
        PersistContext::Standard,
        completion_claim,
    )
    .await;
}

pub(super) async fn initial_injections(
    sess: &Arc<Session>,
    step_context: &StepContext,
    user_input: &[UserInput],
    mentioned_plugins: &[crate::plugins::PluginCapabilitySummary],
    cancellation_token: &CancellationToken,
    is_continuation: bool,
) -> Option<(Vec<ResponseItem>, HashSet<String>)> {
    if is_continuation {
        return Some(Default::default());
    }
    build_injections(
        sess,
        step_context,
        user_input,
        mentioned_plugins,
        cancellation_token,
    )
    .await
}

pub(super) async fn continuation_injections(
    sess: &Arc<Session>,
    step_context: &StepContext,
    user_input: &[UserInput],
    mentioned_plugins: &[crate::plugins::PluginCapabilitySummary],
    cancellation_token: &CancellationToken,
) -> Option<(Vec<ResponseItem>, HashSet<String>)> {
    if user_input.is_empty() {
        return Some(Default::default());
    }
    build_injections(
        sess,
        step_context,
        user_input,
        mentioned_plugins,
        cancellation_token,
    )
    .await
}

async fn build_injections(
    sess: &Arc<Session>,
    step_context: &StepContext,
    user_input: &[UserInput],
    mentioned_plugins: &[crate::plugins::PluginCapabilitySummary],
    cancellation_token: &CancellationToken,
) -> Option<(Vec<ResponseItem>, HashSet<String>)> {
    let extension_items =
        build_extension_turn_input_items(sess, step_context, user_input, cancellation_token)
            .await?;
    let (mut injection_items, explicitly_enabled_connectors) = build_skills_and_plugins(
        sess,
        step_context,
        user_input,
        mentioned_plugins,
        cancellation_token,
    )
    .await?;
    injection_items.extend(extension_items);
    Some((injection_items, explicitly_enabled_connectors))
}

pub(super) async fn record_initial_injections(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    model_info: &ModelInfo,
    injection_items: &mut Vec<ResponseItem>,
    explicitly_enabled_connectors: &mut HashSet<String>,
    is_continuation: bool,
) {
    if is_continuation {
        return;
    }
    sess.merge_connector_selection(std::mem::take(explicitly_enabled_connectors))
        .await;
    sess.set_previous_turn_settings(Some(PreviousTurnSettings {
        model: model_info.slug.clone(),
        comp_hash: model_info.comp_hash.clone(),
        realtime_active: Some(turn_context.realtime_active),
    }))
    .await;
    record_injections(sess, turn_context, model_info, injection_items).await;
}

pub(super) async fn record_pending_injections(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    model_info: &ModelInfo,
    pending_input: &[TurnInput],
    injection_items: &mut Vec<ResponseItem>,
    explicitly_enabled_connectors: &mut HashSet<String>,
) {
    if pending_input.is_empty() {
        return;
    }
    sess.merge_connector_selection(std::mem::take(explicitly_enabled_connectors))
        .await;
    record_injections(sess, turn_context, model_info, injection_items).await;
}

pub(super) async fn record_claimed_completion(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    model_info: &ModelInfo,
    input: &TurnInput,
    completion_claim: &mut Option<u64>,
) -> bool {
    let (TurnInput::ResponseItem(envelope), Some(claim)) = (input, *completion_claim) else {
        return false;
    };
    if !is_background_completion(input) {
        return false;
    }
    completion_claim.take();
    sess.record_conversation_items_then(
        turn_context,
        model_info,
        std::slice::from_ref(&envelope.item),
        || {
            sess.services
                .unified_exec_manager
                .completion_wake
                .commit_claim(claim);
        },
    )
    .await;
    true
}

fn is_background_completion(input: &TurnInput) -> bool {
    let TurnInput::ResponseItem(envelope) = input else {
        return false;
    };
    let ResponseItem::Message { content, .. } = &envelope.item else {
        return false;
    };
    content.iter().any(|item| {
        matches!(item, ContentItem::InputText { text } if BackgroundCompletion::matches_text(text))
    })
}

pub(crate) async fn record_claimed_input(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    input: &[TurnInput],
    completion_claim: &mut Option<u64>,
) -> bool {
    run_hooks_and_collect_inputs(
        sess,
        turn_context,
        &turn_context.capture_current_model_info(),
        input,
        PersistContext::Standard,
        completion_claim,
    )
    .await
    .should_stop
}

async fn record_injections(
    sess: &Arc<Session>,
    turn_context: &Arc<TurnContext>,
    model_info: &ModelInfo,
    injection_items: &mut Vec<ResponseItem>,
) {
    for response_item in injection_items.drain(..) {
        sess.record_conversation_items(
            turn_context,
            model_info,
            std::slice::from_ref(&response_item),
        )
        .await;
    }
}
