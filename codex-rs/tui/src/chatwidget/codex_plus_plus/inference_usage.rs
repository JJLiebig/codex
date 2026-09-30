//! Inference display is separate from the native login and account maintenance quota.
use super::super::*;
use codex_protocol::inference_attribution::InferenceAttribution;
use codex_protocol::inference_attribution::InferenceNativeSource;

pub(in crate::chatwidget) struct InferenceDisplay {
    attribution: Option<InferenceAttribution>,
    account: StatusAccountDisplay,
    turn_id: Option<String>,
    rate_limits: BTreeMap<String, RateLimitSnapshotDisplay>,
}

impl Default for InferenceDisplay {
    fn default() -> Self {
        Self {
            attribution: None,
            account: StatusAccountDisplay::Inference("Model provider (account unknown)".into()),
            turn_id: None,
            rate_limits: BTreeMap::new(),
        }
    }
}

impl ChatWidget {
    pub(in crate::chatwidget) fn has_inference_display(&self) -> bool {
        self.config.model_provider.is_cli_proxy() || self.inference_display.attribution.is_some()
    }

    pub(in crate::chatwidget) fn inference_status_account(&self) -> Option<&StatusAccountDisplay> {
        if self.has_inference_display() {
            Some(&self.inference_display.account)
        } else {
            self.status_account_display.as_ref()
        }
    }

    pub(in crate::chatwidget) fn inference_status_limits(
        &self,
    ) -> &BTreeMap<String, RateLimitSnapshotDisplay> {
        if self.has_inference_display() {
            &self.inference_display.rate_limits
        } else {
            &self.rate_limit_snapshots_by_limit_id
        }
    }

    fn set_inference_display(&mut self, turn_id: &str, attribution: &InferenceAttribution) {
        if self.inference_display.attribution.as_ref() != Some(attribution)
            || self.inference_display.turn_id.as_deref() != Some(turn_id)
        {
            self.inference_display.rate_limits.clear();
        }
        let label = match attribution {
            InferenceAttribution::ServedNative {
                source,
                display_label,
                ..
            }
            | InferenceAttribution::IntendedNative {
                source,
                display_label,
                ..
            } => {
                let category = match source {
                    InferenceNativeSource::Root => "OpenAI root account",
                    InferenceNativeSource::Imported => "Imported OpenAI account",
                };
                match display_label {
                    Some(label) => format!("{category} ({label})"),
                    None => format!("{category} (label unavailable)"),
                }
            }
            InferenceAttribution::Claude => "Claude (account label unavailable)".into(),
            InferenceAttribution::Unknown => "Model provider (account unknown)".into(),
        };
        let label = if matches!(attribution, InferenceAttribution::IntendedNative { .. }) {
            format!("{label} (request not sent)")
        } else {
            label
        };
        self.inference_display.account = StatusAccountDisplay::Inference(label);
        self.inference_display.attribution = Some(attribution.clone());
        self.inference_display.turn_id = Some(turn_id.to_owned());
    }

    pub(in crate::chatwidget) fn observe_inference_usage(
        &mut self,
        notification: &ServerNotification,
    ) -> bool {
        let (thread_id, turn_id, attribution, snapshot) = match notification {
            ServerNotification::ThreadTokenUsageUpdated(event) => (
                &event.thread_id,
                &event.turn_id,
                event.inference_attribution.as_ref(),
                None,
            ),
            ServerNotification::AccountRateLimitsUpdated(event) => {
                let Some(scope) = event.inference.as_ref() else {
                    return true;
                };
                (
                    &scope.thread_id,
                    &scope.turn_id,
                    Some(&scope.attribution),
                    Some(&event.rate_limits),
                )
            }
            ServerNotification::Error(event) if !event.will_retry => (
                &event.thread_id,
                &event.turn_id,
                event.error.inference_attribution.as_ref(),
                None,
            ),
            ServerNotification::TurnCompleted(event) => (
                &event.thread_id,
                &event.turn.id,
                event
                    .turn
                    .error
                    .as_ref()
                    .and_then(|error| error.inference_attribution.as_ref())
                    .or(event.turn.inference_attribution.as_ref()),
                None,
            ),
            _ => return true,
        };
        if attribution.is_none()
            && !self.config.model_provider.is_cli_proxy()
            && matches!(notification, ServerNotification::ThreadTokenUsageUpdated(_))
            && self.turn_lifecycle.last_turn_id.as_ref() == Some(turn_id)
            && self.thread_id.map(|id| id.to_string()).as_ref() == Some(thread_id)
        {
            self.inference_display = InferenceDisplay::default();
        }
        let unknown = InferenceAttribution::Unknown;
        let Some(attribution) = attribution.or_else(|| {
            (self.config.model_provider.is_cli_proxy()
                && matches!(
                    notification,
                    ServerNotification::Error(_) | ServerNotification::TurnCompleted(_)
                ))
            .then_some(&unknown)
        }) else {
            return true;
        };
        if self.thread_id.map(|id| id.to_string()).as_ref() != Some(thread_id)
            || self.turn_lifecycle.last_turn_id.as_ref() != Some(turn_id)
        {
            return !matches!(
                notification,
                ServerNotification::ThreadTokenUsageUpdated(_)
                    | ServerNotification::AccountRateLimitsUpdated(_)
            );
        }
        self.set_inference_display(turn_id, attribution);
        if let Some(snapshot) = snapshot {
            let limit_id = snapshot.limit_id.clone().unwrap_or_else(|| "codex".into());
            let label = snapshot
                .limit_name
                .clone()
                .unwrap_or_else(|| limit_id.clone());
            self.inference_display.rate_limits.insert(
                limit_id,
                rate_limit_snapshot_display_for_limit(
                    snapshot,
                    label,
                    Local::now(),
                    self.clock_format,
                ),
            );
        }
        self.refresh_status_surfaces();
        true
    }

    pub(in crate::chatwidget) fn restore_inference_display(&mut self, turns: &[Turn]) {
        // Initial pages are chronological; subsequent older pages must not replace live identity.
        if self.inference_display.turn_id.is_none()
            && let Some(turn) = turns.last()
            && let Some(attribution) = turn
                .error
                .as_ref()
                .and_then(|error| error.inference_attribution.as_ref())
                .or(turn.inference_attribution.as_ref())
        {
            self.set_inference_display(&turn.id, attribution);
        }
    }
}

#[cfg(test)]
#[path = "inference_usage_tests.rs"]
mod tests;
