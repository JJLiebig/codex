use crate::error::CodexErr;
use crate::inference_attribution::InferenceAttribution;

impl CodexErr {
    /// Host observation of this owned native quota failure, retained through delayed error mapping.
    pub fn usage_limit_observed_at_ns(&self) -> Option<i64> {
        self.usage_limit_observed_at_ns
    }

    pub fn with_usage_limit_observed_at_ns(mut self, observed_at: Option<i64>) -> Self {
        self.usage_limit_observed_at_ns = observed_at;
        self
    }

    pub fn inference_attribution(&self) -> Option<&InferenceAttribution> {
        self.inference_attribution.as_deref()
    }

    /// Preserve request evidence when replacing user guidance without changing the failed request.
    pub fn with_inference_attribution_from(mut self, original: &Self) -> Self {
        self.inference_attribution = original.inference_attribution.clone();
        self.usage_limit_observed_at_ns = original.usage_limit_observed_at_ns;
        self
    }

    /// Retain frozen request evidence through terminal sampling and compaction error mapping.
    pub fn with_inference_attribution(mut self, attribution: InferenceAttribution) -> Self {
        self.inference_attribution = Some(Box::new(attribution));
        self
    }
}
