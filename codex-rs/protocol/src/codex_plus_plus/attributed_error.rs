use crate::error::CodexErr;
use crate::inference_attribution::InferenceAttribution;

impl CodexErr {
    pub fn inference_attribution(&self) -> Option<&InferenceAttribution> {
        self.inference_attribution.as_deref()
    }

    /// Preserve request evidence when replacing user guidance without changing the failed request.
    pub fn with_inference_attribution_from(mut self, original: &Self) -> Self {
        self.inference_attribution = original.inference_attribution.clone();
        self
    }

    /// Retain frozen request evidence through terminal sampling and compaction error mapping.
    pub fn with_inference_attribution(mut self, attribution: InferenceAttribution) -> Self {
        self.inference_attribution = Some(Box::new(attribution));
        self
    }
}
