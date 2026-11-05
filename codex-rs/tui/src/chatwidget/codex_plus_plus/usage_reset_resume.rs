use super::ChatWidget;
use crate::app_command::AppCommand;
use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::TurnStatus;
use codex_login::AccountId;
use sha2::Digest;
use sha2::Sha256;

pub(in crate::chatwidget) struct UsageResetWait {
    turn_id: String,
    failed_at: i64,
}

impl ChatWidget {
    pub(in crate::chatwidget) fn observe_usage_reset_turn(&mut self, event: &ServerNotification) {
        let failed_turn = match event {
            ServerNotification::Error(error)
                if !error.will_retry
                    && error.error.codex_error_info == Some(CodexErrorInfo::UsageLimitExceeded) =>
            {
                Some((&error.turn_id, None))
            }
            ServerNotification::TurnCompleted(turn)
                if turn.turn.status == TurnStatus::Failed
                    && turn.turn.error.as_ref().is_some_and(|error| {
                        error.codex_error_info == Some(CodexErrorInfo::UsageLimitExceeded)
                    }) =>
            {
                Some((
                    &turn.turn.id,
                    turn.turn
                        .completed_at
                        .map(|at| at.saturating_add(1).saturating_mul(1_000_000_000)),
                ))
            }
            ServerNotification::TurnStarted(_)
            | ServerNotification::AccountUpdated(_)
            | ServerNotification::ThreadClosed(_) => {
                self.usage_reset_wait = None;
                None
            }
            ServerNotification::TurnCompleted(turn) if turn.turn.status != TurnStatus::Failed => {
                self.usage_reset_wait = None;
                None
            }
            _ => None,
        };
        if let Some((turn_id, completed_at)) = failed_turn {
            if self.turn_lifecycle.agent_turn_running
                && !self.input_queue.user_turn_pending_start
                && self.turn_lifecycle.last_turn_id.as_ref() == Some(turn_id)
            {
                self.usage_reset_wait = Some(UsageResetWait {
                    turn_id: turn_id.clone(),
                    failed_at: completed_at.unwrap_or_else(|| {
                        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX)
                    }),
                });
            } else if let Some(waiting) = self.usage_reset_wait.as_mut()
                && &waiting.turn_id == turn_id
                && let Some(completed_at) = completed_at
            {
                // Server time survives independent delivery; exclude its ambiguous whole second.
                waiting.failed_at = waiting.failed_at.min(completed_at);
            }
        }
    }

    pub(in crate::chatwidget) fn observe_usage_reset_command(&mut self, command: &AppCommand) {
        if matches!(
            command,
            AppCommand::Interrupt
                | AppCommand::UserTurn { .. }
                | AppCommand::Review { .. }
                | AppCommand::Compact
        ) {
            self.usage_reset_wait = None;
        }
    }

    pub(crate) fn usage_reset_turn(&self, completed_at: i64) -> Option<String> {
        let waiting = self.usage_reset_wait.as_ref()?;
        (completed_at >= waiting.failed_at
            && self
                .last_resumed_usage_reset_at
                .is_none_or(|last| completed_at > last)
            && !self.turn_lifecycle.agent_turn_running
            && !self.input_queue.user_turn_pending_start
            && !self.input_queue.has_queued_follow_up_messages()
            && self.input_queue.pending_steers.is_empty())
        .then(|| waiting.turn_id.clone())
    }

    pub(crate) fn resume_after_usage_reset(
        &mut self,
        turn_id: &str,
        account_id: &AccountId,
        completed_at: i64,
        response: &GetAccountRateLimitsResponse,
    ) {
        if self.usage_reset_turn(completed_at).as_deref() != Some(turn_id)
            || !reset_account_has_quota(account_id, response)
        {
            return;
        }
        // Consume before submission: repeated completion/read callbacks cannot enqueue twice.
        self.usage_reset_wait = None;
        self.last_resumed_usage_reset_at = Some(completed_at);
        self.submit_user_message("continue".into());
    }
}

fn reset_account_has_quota(
    account_id: &AccountId,
    response: &GetAccountRateLimitsResponse,
) -> bool {
    let Some(backend_id) = response.account_id.as_deref() else {
        return false;
    };
    let digest = Sha256::digest(format!("account:{backend_id}"));
    if format!("acct_{digest:x}").get(..21) != Some(account_id.as_str()) {
        return false;
    }
    let limits = response
        .rate_limits_by_limit_id
        .as_ref()
        .and_then(|limits| limits.get("codex"))
        .unwrap_or(&response.rate_limits);
    let windows = [limits.primary.as_ref(), limits.secondary.as_ref()];
    limits.limit_id.as_deref().is_none_or(|id| id == "codex")
        && limits.rate_limit_reached_type.is_none()
        && limits.spend_control_reached != Some(true)
        && windows
            .iter()
            .flatten()
            .any(|window| window.window_duration_mins == Some(7 * 24 * 60))
        && windows
            .iter()
            .flatten()
            .all(|window| (0..100).contains(&window.used_percent))
}

#[cfg(test)]
#[path = "usage_reset_resume_tests.rs"]
mod tests;
