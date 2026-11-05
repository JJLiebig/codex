use super::App;
use super::AppServerSession;
use super::background_requests::fetch_account_rate_limits;
use crate::app_event::AppEvent;
use crate::app_event::RateLimitRefreshOrigin;
use codex_login::AccountId;
use std::time::Instant;

impl App {
    pub(super) fn rate_limit_poll_deadline(&self) -> Option<Instant> {
        self.rate_limit_refresh_state
            .poll_deadline(self.chat_widget.rate_limit_refresh_interval()?)
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
