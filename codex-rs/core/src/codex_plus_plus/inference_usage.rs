//! Keep rolling quota paired with the frozen request that supplied it.
use crate::client::ModelClientSession;
use crate::session::turn_context::TurnContext;
use codex_protocol::inference_attribution::InferenceAttribution;
use codex_protocol::protocol::RateLimitSnapshot;
use codex_protocol::protocol::TokenCountEvent;

#[derive(Debug, Default)]
pub(crate) struct InferenceUsage {
    attribution: Option<InferenceAttribution>,
    rate_limits: Option<RateLimitSnapshot>,
}

impl TurnContext {
    pub(crate) async fn begin_inference_request(&self, client: &ModelClientSession) {
        *self.inference_usage.lock().await = InferenceUsage {
            attribution: client.owned_inference_attribution(),
            rate_limits: None,
        };
    }

    pub(crate) async fn record_inference_rate_limits(&self, snapshot: &RateLimitSnapshot) -> bool {
        let mut usage = self.inference_usage.lock().await;
        if usage.attribution.is_none() && !self.config.model_provider.is_cli_proxy() {
            return false;
        }
        usage.rate_limits = Some(snapshot.clone());
        true
    }

    pub(crate) async fn attribute_token_count(&self, event: &mut TokenCountEvent) {
        let usage = self.inference_usage.lock().await;
        if usage.attribution.is_some() || self.config.model_provider.is_cli_proxy() {
            event.inference_attribution = Some(
                usage
                    .attribution
                    .clone()
                    .unwrap_or(InferenceAttribution::Unknown),
            );
            // The session cache may belong to a previous account or to native maintenance.
            event.rate_limits = usage.rate_limits.clone();
        }
    }

    pub(crate) async fn inference_attribution(&self) -> Option<InferenceAttribution> {
        self.inference_usage.lock().await.attribution.clone()
    }
}
