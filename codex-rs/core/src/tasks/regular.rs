use std::sync::Arc;

use codex_async_utils::OrCancelExt;
use codex_extension_api::TurnStartPhase;
use tokio_util::sync::CancellationToken;

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::TurnRunState;
use crate::session::turn::run_hooks_and_record_inputs;
use crate::session::turn_context::TurnContext;
use crate::session_startup_prewarm::SessionStartupPrewarmResolution;
use crate::state::TaskKind;
use codex_thread_store::PersistContext;
use tracing::Instrument;
use tracing::trace_span;

use super::SessionTask;
use super::SessionTaskResult;
use super::codex_plus_plus::completion_wake;

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
        // Regular turns emit `TurnStarted` inline so first-turn lifecycle does
        // not wait on startup prewarm resolution.
        let prewarmed_client_session = async {
            sess.emit_turn_started(&ctx).await;
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
            let preparation = sess
                .emit_turn_start_lifecycle(
                    &ctx,
                    /*token_usage_at_turn_start*/ None,
                    TurnStartPhase::RegularTaskStart,
                )
                .or_cancel(&cancellation_token)
                .await;
            // Even cancelled discovery may have cleared the previous account's catalog.
            if prepares_mcp {
                sess.request_mcp_runtime_reprojection();
            }
            if preparation.is_err() {
                return SessionStartupPrewarmResolution::Cancelled;
            }
            sess.set_server_reasoning_included(/*included*/ false).await;
            sess.consume_startup_prewarm_for_regular_turn(&cancellation_token)
                .await
        }
        .instrument(trace_span!("regular_task.prepare_run_turn"))
        .await;
        let prewarmed_client_session = match prewarmed_client_session {
            SessionStartupPrewarmResolution::Cancelled => {
                run_hooks_and_record_inputs(
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
