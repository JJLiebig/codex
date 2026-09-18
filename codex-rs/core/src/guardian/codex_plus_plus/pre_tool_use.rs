use super::ApprovalRequestReasons;
use super::GuardianApprovalRequest;
use super::GuardianReviewContext;
use super::GuardianReviewOptions;
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
        codex_guardian_reviewer::SynchronousReview::new(ReviewRuntime {
            session,
            context,
            review_id,
            request: request.into(),
            reasons: ApprovalRequestReasons::default(),
            options: GuardianReviewOptions {
                require_guardian: true,
                plugin_attribution_override: None,
                approval_request_source: GuardianApprovalRequestSource::MainTurn,
                external_cancel: None,
                require_synchronous_review: true,
            },
        })
        .review(GuardianReviewReason::FreshRequired)
        .await
        .unwrap_or_else(|| {
            ReviewDecision::denied("automatic approval review did not return a decision")
        })
    })
}
