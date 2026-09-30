use super::App;
use super::AppServerSession;
use super::background_requests::fetch_account_rate_limits;
use super::rate_limit_refresh::RateLimitReadStatus;
use crate::app_event::AppEvent;
use crate::app_event::RateLimitRefreshOrigin;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::GetAccountRateLimitsParams;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::UsageResetCompletion;
use codex_app_server_protocol::UsageResetTargetParams;
use codex_login::AccountId;
use std::ops::ControlFlow;
use std::time::Duration;
use std::time::Instant;

impl App {
    pub(super) fn rate_limit_poll_deadline(&self) -> Option<Instant> {
        let interval = self.chat_widget.rate_limit_refresh_interval().or_else(|| {
            self.chat_widget
                .owned_reset_target(/*completion*/ None)
                .map(|_| Duration::from_secs(/*secs*/ 60))
        })?;
        self.rate_limit_refresh_state.poll_deadline(interval)
    }

    pub(super) fn refresh_owned_reset(
        &self,
        app_server: &AppServerSession,
        periodic_request_id: Option<u64>,
        completion: Option<&UsageResetCompletion>,
    ) -> ControlFlow<()> {
        let Some(target) = self.chat_widget.owned_reset_target(completion) else {
            return ControlFlow::Continue(());
        };
        let owned_only = periodic_request_id.is_some()
            && self.chat_widget.rate_limit_refresh_interval().is_none();
        let periodic_request_id = periodic_request_id.filter(|_| owned_only);
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        let hard_stop_generation = self.rate_limit_hard_stop_generation;
        tokio::spawn(async move {
            let request = handle.request_typed(ClientRequest::GetAccountRateLimits {
                request_id: RequestId::String(format!("reset-admission-{}", uuid::Uuid::new_v4())),
                params: Some(GetAccountRateLimitsParams {
                    reset_admission: Some(target.clone()),
                    ..Default::default()
                }),
            });
            let response = tokio::time::timeout(Duration::from_secs(/*secs*/ 15), request)
                .await
                .ok()
                .and_then(Result::ok);
            tx.send(AppEvent::UsageResetAdmissionLoaded {
                target,
                periodic_request_id,
                hard_stop_generation,
                response,
            });
        });
        if owned_only {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    }

    pub(super) fn finish_owned_reset(
        &mut self,
        target: &UsageResetTargetParams,
        periodic_request_id: Option<u64>,
        hard_stop_generation: u64,
        response: Option<GetAccountRateLimitsResponse>,
    ) {
        if let Some(request_id) = periodic_request_id {
            self.rate_limit_refresh_state.finish(
                request_id,
                hard_stop_generation,
                self.rate_limit_hard_stop_generation,
                RateLimitReadStatus::Failed,
            );
        }
        if self.rate_limit_hard_stop_generation == hard_stop_generation
            && let Some(response) = response
        {
            self.chat_widget.resume_after_owned_reset(target, &response);
        }
    }

    pub(super) fn refresh_after_usage_reset(
        &self,
        app_server: &AppServerSession,
        account_id: AccountId,
        completed_at: i64,
        completion: Option<&UsageResetCompletion>,
    ) {
        if let Some(completion) = completion {
            let _ = self.refresh_owned_reset(
                app_server,
                /*periodic_request_id*/ None,
                Some(completion),
            );
        }
        let Some(thread_id) = self.chat_widget.thread_id() else {
            return;
        };
        let Some(turn_id) = self.chat_widget.usage_reset_turn(completed_at) else {
            return;
        };
        let handle = app_server.request_handle();
        let tx = self.app_event_tx.clone();
        let hard_stop_generation = self.rate_limit_hard_stop_generation;
        tokio::spawn(async move {
            if let Ok(Ok(response)) = tokio::time::timeout(
                std::time::Duration::from_secs(/*secs*/ 15),
                fetch_account_rate_limits(handle, RateLimitRefreshOrigin::Recovery),
            )
            .await
            {
                tx.send(AppEvent::UsageResetQuotaLoaded {
                    thread_id,
                    turn_id,
                    account_id,
                    completed_at,
                    hard_stop_generation,
                    response,
                });
            }
        });
    }
}
