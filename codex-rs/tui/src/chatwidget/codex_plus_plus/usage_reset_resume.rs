use super::ChatWidget;
use crate::app_command::AppCommand;
use codex_app_server_protocol::CodexErrorInfo;
use codex_app_server_protocol::GetAccountRateLimitsResponse;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UsageResetCompletion;
use codex_app_server_protocol::UsageResetTargetParams;
use codex_login::AccountId;
use codex_protocol::inference_attribution::InferenceAttribution;
use sha2::Digest;
use sha2::Sha256;

pub(in crate::chatwidget) struct UsageResetWait {
    turn_id: String,
    failed_at: Option<i64>,
    attribution: Option<InferenceAttribution>,
}

impl ChatWidget {
    pub(crate) fn is_pending_reset_consume(&self, request_id: u64) -> bool {
        self.pending_rate_limit_reset_request_id == Some(request_id)
    }

    pub(in crate::chatwidget) fn observe_usage_reset_turn(&mut self, event: &ServerNotification) {
        let failed_turn = match event {
            ServerNotification::Error(error)
                if !error.will_retry
                    && error.error.codex_error_info == Some(CodexErrorInfo::UsageLimitExceeded) =>
            {
                Some((
                    &error.turn_id,
                    error
                        .error
                        .usage_limit_observed_at_ns
                        .as_deref()
                        .and_then(|at| at.parse::<i64>().ok())
                        .filter(|at| *at >= 0),
                    error.error.inference_attribution.as_ref(),
                ))
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
                        .error
                        .as_ref()
                        .and_then(|error| error.usage_limit_observed_at_ns.as_deref())
                        .and_then(|at| at.parse::<i64>().ok())
                        .filter(|at| *at >= 0)
                        .or_else(|| {
                            turn.turn
                                .completed_at
                                .and_then(|at| at.checked_add(1)?.checked_mul(1_000_000_000))
                        }),
                    turn.turn
                        .error
                        .as_ref()
                        .and_then(|error| error.inference_attribution.as_ref()),
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
        if let Some((turn_id, completed_at, attribution)) = failed_turn {
            if matches!(
                attribution,
                Some(InferenceAttribution::Claude | InferenceAttribution::Unknown)
            ) || (attribution.is_none() && self.config.model_provider.is_cli_proxy())
            {
                self.usage_reset_wait = None;
                return;
            }
            if attribution.is_some()
                && let Some(waiting) = self.usage_reset_wait.as_mut()
                && &waiting.turn_id == turn_id
                && waiting.attribution.as_ref() == attribution
            {
                if let Some(completed_at) = completed_at {
                    // Keep the original precise host observation through duplicate delivery.
                    waiting.failed_at = Some(waiting.failed_at.unwrap_or(completed_at));
                }
            } else if self.turn_lifecycle.agent_turn_running
                && !self.input_queue.user_turn_pending_start
                && self.turn_lifecycle.last_turn_id.as_ref() == Some(turn_id)
            {
                self.usage_reset_wait = Some(UsageResetWait {
                    turn_id: turn_id.clone(),
                    attribution: attribution.cloned(),
                    failed_at: completed_at.or_else(|| {
                        attribution
                            .is_none()
                            .then(|| chrono::Utc::now().timestamp_nanos_opt().unwrap_or(i64::MAX))
                    }),
                });
            } else if let Some(waiting) = self.usage_reset_wait.as_mut()
                && &waiting.turn_id == turn_id
                && waiting.attribution.as_ref() == attribution
                && let Some(completed_at) = completed_at
            {
                waiting.failed_at =
                    Some(waiting.failed_at.unwrap_or(completed_at).min(completed_at));
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
        self.usage_reset_wait
            .as_ref()?
            .attribution
            .is_none()
            .then_some(())?;
        self.reset_ready_turn(completed_at)
    }

    fn reset_ready_turn(&self, completed_at: i64) -> Option<String> {
        let waiting = self.usage_reset_wait.as_ref()?;
        (completed_at >= waiting.failed_at?
            && self
                .last_resumed_usage_reset_at
                .is_none_or(|last| completed_at > last)
            && !self.turn_lifecycle.agent_turn_running
            && !self.input_queue.user_turn_pending_start
            && !self.input_queue.has_queued_follow_up_messages()
            && self.input_queue.pending_steers.is_empty())
        .then(|| waiting.turn_id.clone())
    }

    pub(crate) fn owned_reset_target(
        &self,
        completion: Option<&UsageResetCompletion>,
    ) -> Option<UsageResetTargetParams> {
        let turn_id = self.reset_ready_turn(i64::MAX)?;
        let waiting = self.usage_reset_wait.as_ref()?;
        let (source, account_id) = match waiting.attribution.as_ref()? {
            InferenceAttribution::ServedNative {
                source, account_id, ..
            }
            | InferenceAttribution::IntendedNative {
                source, account_id, ..
            } => (*source, account_id),
            InferenceAttribution::Claude | InferenceAttribution::Unknown => return None,
        };
        if completion.is_some_and(|completion| {
            completion.source != source || &completion.account_id != account_id
        }) {
            return None;
        }
        Some(UsageResetTargetParams {
            thread_id: self.thread_id()?.to_string(),
            turn_id,
            source,
            account_id: account_id.clone(),
            failed_at: waiting.failed_at?.saturating_add(999_999_999) / 1_000_000_000,
            failed_at_ns: Some(waiting.failed_at?.to_string()),
            completion_id: completion.map(|completion| completion.id.clone()),
        })
    }

    pub(crate) fn resume_after_owned_reset(
        &mut self,
        target: &UsageResetTargetParams,
        response: &GetAccountRateLimitsResponse,
    ) {
        let Some(completion) = &response.reset_admission else {
            return;
        };
        let Some(current) = self.owned_reset_target(Some(completion)) else {
            return;
        };
        let mut expected = target.clone();
        if expected
            .completion_id
            .as_ref()
            .is_some_and(|id| id != &completion.id)
        {
            return;
        }
        expected.completion_id = Some(completion.id.clone());
        let Some(completed_at) = completion
            .completed_at_ns
            .parse::<i64>()
            .ok()
            .filter(|at| *at >= 0)
        else {
            return;
        };
        if current != expected
            || self.reset_ready_turn(completed_at).as_deref() != Some(&target.turn_id)
            || response.ordinary_usage_allowed != Some(true)
        {
            return;
        }
        self.usage_reset_wait = None;
        self.last_resumed_usage_reset_at = Some(completed_at);
        self.submit_user_message("continue".into());
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
