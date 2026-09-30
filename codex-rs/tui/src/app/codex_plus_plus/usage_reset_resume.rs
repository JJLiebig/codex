use super::App;
use super::AppServerSession;
use super::background_requests::fetch_account_rate_limits;
use crate::app_event::AppEvent;
use crate::app_event::RateLimitRefreshOrigin;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::GetAccountRateLimitsParams;
use codex_app_server_protocol::RequestId;
use codex_login::AccountId;

impl App {
    pub(super) fn refresh_owned_reset(&self, app_server: &AppServerSession) {
        let Some(target) = self.chat_widget.owned_reset_target(/*completion*/ None) else {
            return;
        };
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
            if let Ok(Ok(response)) =
                tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 15), request).await
            {
                tx.send(AppEvent::UsageResetAdmissionLoaded {
                    target,
                    hard_stop_generation,
                    response,
                });
            }
        });
    }

    pub(super) fn refresh_after_usage_reset(
        &self,
        app_server: &AppServerSession,
        account_id: AccountId,
        completed_at: i64,
    ) {
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
