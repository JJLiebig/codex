use crate::McpRefreshReason;
use std::sync::Arc;

use codex_async_utils::OrCancelExt;
use codex_extension_api::TurnStartPhase;
use codex_protocol::protocol::InternalSessionSource;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use futures::future::BoxFuture;
use tokio_util::sync::CancellationToken;

use crate::client::ModelClientSession;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::startup_prewarm::SessionStartupPrewarmResolution;
use crate::session::turn::McpStartupRequirements;
use crate::session::turn::TurnRunState;
use crate::session::turn::run_hooks_and_record_inputs;
use crate::session::turn_context::TurnContext;
use crate::state::TaskKind;
use codex_thread_store::PersistContext;
use tracing::Instrument;
use tracing::trace_span;

use super::SessionTask;
use super::SessionTaskResult;
use super::codex_plus_plus::completion_wake;

/// The last answer and the MCP requirements needed if a steer reopens sampling.
pub(crate) struct RegularTaskOutput {
    pub(super) last_agent_message: Option<String>,
    pub(super) mcp_startup_requirements: McpStartupRequirements,
}

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
        if ctx
            .config
            .features
            .enabled(codex_features::Feature::CriticalPathTiming)
            && !matches!(
                ctx.session_source,
                SessionSource::Internal(InternalSessionSource::MemoryConsolidation)
                    | SessionSource::SubAgent(SubAgentSource::MemoryConsolidation)
            )
        {
            ctx.turn_timing_state.enable_critical_path();
        }
        // Regular turns emit `TurnStarted` inline so first-turn lifecycle does
        // not wait on startup prewarm resolution.
        let prewarmed_client_session = async {
            sess.emit_turn_started(&ctx, TaskKind::Regular).await;
            // Regular-start contributors run once, after the task is visible and interruptible.
            let prepares_mcp = sess
                .services
                .extensions
                .turn_lifecycle_contributors()
                .iter()
                .any(|contributor| {
                    contributor.turn_start_phase(&sess.services.thread_extension_data)
                        == TurnStartPhase::RegularTaskStart
                        && contributor.requires_mcp_runtime(&sess.services.thread_extension_data)
                });
            let preparation = async {
                sess.emit_turn_start_lifecycle(
                    &ctx,
                    /*token_usage_at_turn_start*/ None,
                    TurnStartPhase::RegularTaskStart,
                )
                .await;
                if prepares_mcp {
                    // Resource discovery can overlap the fetch. Wait before reprojection.
                    sess.wait_for_apps_tool_refresh().await;
                }
            }
            .or_cancel(&cancellation_token)
            .await;
            // Even cancelled discovery may have cleared the previous account's catalog.
            if prepares_mcp {
                sess.request_mcp_runtime_reprojection(
                    McpRefreshReason::TurnStartContributorPreparedMcp,
                );
            }
            if preparation.is_err() {
                return SessionStartupPrewarmResolution::Cancelled;
            }
            sess.consume_startup_prewarm_for_regular_turn(&cancellation_token)
                .await
        }
        .instrument(trace_span!("regular_task.prepare_run_turn"))
        .await;
        let prewarmed_client_session = match prewarmed_client_session {
            SessionStartupPrewarmResolution::Cancelled => {
                let _ = run_hooks_and_record_inputs(
                    &sess,
                    &ctx,
                    &ctx.capture_current_model_info(),
                    &input,
                    PersistContext::Standard,
                )
                .await;
                return Ok(None);
            }
            SessionStartupPrewarmResolution::Unavailable { .. } => None,
            SessionStartupPrewarmResolution::Ready(prewarmed_client_session) => {
                Some(*prewarmed_client_session)
            }
        };
        let next_input = input;
        let mut turn_state = TurnRunState::from_prewarmed_client_session(prewarmed_client_session);
        turn_state.completion_claim = self.completion_claim;
        completion_wake::run_turn_loop(sess, ctx, next_input, cancellation_token, turn_state).await
    }
}

impl RegularTask {
    /// Runs model requests retaining explicit MCP requirements across steers.
    pub(super) fn run_turns(
        sess: Arc<Session>,
        ctx: Arc<TurnContext>,
        input: Vec<TurnInput>,
        mcp_startup_requirements: McpStartupRequirements,
        prewarmed_client_session: Option<ModelClientSession>,
        cancellation_token: CancellationToken,
    ) -> BoxFuture<'static, SessionTaskResult> {
        let mut turn_state = TurnRunState::from_prewarmed_client_session(prewarmed_client_session);
        turn_state.mcp_startup_requirements = mcp_startup_requirements;
        Box::pin(completion_wake::run_turn_loop(
            sess,
            ctx,
            input,
            cancellation_token,
            turn_state,
        ))
    }
}
