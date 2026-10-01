//! Readable source-model summaries at the owned OpenAI-to-Claude boundary.
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
    destination: &Arc<TurnContext>,
    usage_limit_account_attempts: &mut HashSet<String>,
) -> CodexResult<bool> {
    if !source.provider.info().is_cli_proxy() || !destination.provider.info().is_cli_proxy() {
        return Ok(false);
    }
    let destination_request = destination
        .provider
        .prepare_request(&destination.model_info().slug)
        .await?;
    if !destination_request.as_ref().is_some_and(|request| {
        request
            .route
            .as_ref()
            .is_some_and(|route| route.is_claude_model(&request.model))
    }) {
        return Ok(false);
    }
    let source_request = source
        .provider
        .prepare_request(&source.model_info().slug)
        .await?;
    if !source_request.as_ref().is_some_and(|request| {
        request
            .route
            .as_ref()
            .is_some_and(|route| route.native_source().is_some())
    }) {
        return Ok(false);
    }
    let _profile_guard = destination.turn_timing_state.begin_compaction();
    run_compact_task_inner(
        Arc::clone(sess),
        Arc::clone(source),
        vec![UserInput::Text {
            text: source
                .config
                .compact_prompt
                .as_deref()
                .unwrap_or(SUMMARIZATION_PROMPT)
                .into(),
            text_elements: Vec::new(),
        }],
        Some(usage_limit_account_attempts),
        InitialContextInjection::DoNotInject,
        CompactionTrigger::Auto,
        CompactionReason::CompHashChanged,
        CompactionPhase::PreTurn,
        LocalCompactionMode::ReadableHandoff,
    )
    .await?;
    Ok(true)
}
