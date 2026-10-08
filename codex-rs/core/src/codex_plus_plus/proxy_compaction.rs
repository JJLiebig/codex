//! Readable source-model summaries only when crossing the owned OpenAI/Claude boundary.
use super::*;

#[derive(Clone, Copy, PartialEq)]
pub(super) enum LocalCompactionMode {
    Standard,
    ReadableHandoff,
}

impl LocalCompactionMode {
    pub(super) fn buffers_output(self, phase: CompactionPhase) -> bool {
        self == Self::ReadableHandoff || matches!(phase, CompactionPhase::PostTurn)
    }

    pub(super) fn missing_summary_error(self) -> CodexErr {
        CodexErr::Stream(
            match self {
                Self::Standard => "Post-turn compaction completed without an assistant summary",
                Self::ReadableHandoff => "Model handoff completed without an assistant summary",
            }
            .into(),
        )
    }

    pub(super) fn replacement_history(
        self,
        history: &[ResponseItemEnvelope],
        summary: &str,
    ) -> CodexResult<Vec<ResponseItemEnvelope>> {
        match self {
            Self::Standard => Ok(build_compacted_history(
                Vec::new(),
                &collect_annotated_user_messages(history),
                summary,
            )),
            Self::ReadableHandoff => {
                // Enforce the model-context item ceiling without truncating source state.
                if approx_token_count(summary) > 10_000 {
                    return Err(CodexErr::Stream(
                        "Model handoff summary exceeds 10000 tokens; source history was preserved"
                            .into(),
                    ));
                }
                Ok(build_compacted_history(Vec::new(), &[], summary))
            }
        }
    }
}

pub(crate) async fn maybe_run_readable_handoff(
    sess: &Arc<Session>,
    source: &Arc<TurnContext>,
    replacement_step_context: &Arc<StepContext>,
    usage_limit_account_attempts: &mut HashSet<String>,
) -> CodexResult<bool> {
    let destination = &replacement_step_context.turn;
    if !source.provider.info().is_cli_proxy() || !destination.provider.info().is_cli_proxy() {
        return Ok(false);
    }
    let destination_request = destination
        .provider
        .prepare_request(&destination.model_info().slug)
        .await?;
    let source_request = source
        .provider
        .prepare_request(&source.model_info().slug)
        .await?;
    let (Some(source_request), Some(destination_request)) = (source_request, destination_request)
    else {
        return Ok(false);
    };
    let (Some(source_route), Some(destination_route)) =
        (&source_request.route, &destination_request.route)
    else {
        return Ok(false);
    };
    let crosses_family = (source_route.native_source().is_some()
        && destination_route.is_claude_model(&destination_request.model))
        || (source_route.is_claude_model(&source_request.model)
            && destination_route.native_source().is_some());
    if !crosses_family {
        return Ok(false);
    }
    let world_state = Arc::new(
        sess.build_world_state_for_step(&replacement_step_context, /*new_window*/ true)
            .await?,
    );
    let _profile_guard = destination.turn_timing_state.begin_compaction();
    run_compact_task_inner(
        Arc::clone(sess),
        Arc::clone(source),
        Arc::clone(replacement_step_context),
        vec![UserInput::Text {
            text: source
                .config
                .compact_prompt
                .as_deref()
                .unwrap_or(SUMMARIZATION_PROMPT)
                .into(),
            text_elements: Vec::new(),
        }],
        world_state,
        CompactionTurnMetadata::new(
            CompactionTrigger::Auto,
            CompactionReason::CompHashChanged,
            CompactionImplementation::Responses,
            CompactionPhase::PreTurn,
        ),
        Some(usage_limit_account_attempts),
        LocalCompactionMode::ReadableHandoff,
    )
    .await?;
    Ok(true)
}
