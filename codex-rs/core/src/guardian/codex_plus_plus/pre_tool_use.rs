use super::ApprovalRequestReasons;
use super::GuardianApprovalRequest;
use super::GuardianReviewContext;
use super::GuardianReviewOptions;
use super::runtime::ReviewAction;
use super::runtime::ReviewRuntime;
use crate::session::session::Session;
use codex_analytics::GuardianApprovalRequestSource;
use codex_extension_api::SynchronousApprovalReviewer;
use codex_protocol::approvals::GuardianReviewReason;
use codex_protocol::protocol::ReviewDecision;
use futures::future::BoxFuture;
use std::sync::Arc;

pub(crate) fn review(
    session: Arc<Session>,
    context: GuardianReviewContext,
    review_id: String,
    request: GuardianApprovalRequest,
) -> BoxFuture<'static, ReviewDecision> {
    // A hook's explicit ask requires a fresh review even under full access.
    // Erase the reviewer future to keep recursive session/auth futures bounded.
    Box::pin(async move {
        let (_, history_reset) = session.history_reset().await;
        let cancellation = history_reset.child_token();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let request = ReviewAction::from(request);
        let turn = context.turn();
        let decision = codex_guardian_reviewer::ReviewRequest {
            host: ReviewRuntime {
                session: Arc::clone(&session),
                history_reset,
                context: context.clone(),
                request: request.clone(),
                reasons: ApprovalRequestReasons::default(),
                options: GuardianReviewOptions {
                    require_guardian: true,
                    plugin_attribution_override: None,
                    approval_request_source: GuardianApprovalRequestSource::MainTurn,
                    external_cancel: None,
                    require_synchronous_review: true,
                },
            },
            approval_id: &review_id,
            tool_call_id: request
                .request
                .as_ref()
                .ok()
                .and_then(super::approval_request::guardian_request_target_item_id),
            action: request.action.as_ref().ok(),
            thread_id: session.thread_id,
            thread_store: &session.services.thread_extension_data,
            category: request.category,
            approval_policy: context.approval_policy,
            approvals_reviewer: context.approvals_reviewer,
            require_guardian: true,
            require_synchronous_review: true,
            model_requires_review: false,
            async_enabled: false,
            retried: false,
            escalated_exec: false,
            full_access: false,
            cancellation: cancellation.clone(),
            model: turn.model_info(),
            telemetry: &session.services.session_telemetry,
            analytics: &session.services.analytics_events_client,
            metrics: Some(crate::session::extension_metrics::from_session_telemetry(
                turn.session_telemetry.clone(),
            )),
        }
        .review(GuardianReviewReason::FreshRequired)
        .await
        .unwrap_or_else(|| {
            ReviewDecision::denied("automatic approval review did not return a decision")
        });
        if cancellation.is_cancelled() {
            ReviewDecision::Abort
        } else {
            decision
        }
    })
}
