use super::ChatWidget;
use codex_protocol::inference_attribution::InferenceAttribution;
use codex_protocol::inference_attribution::InferenceNativeSource;

impl ChatWidget {
    pub(super) fn handle_owned_inference_error(
        &mut self,
        message: &str,
        attribution: Option<InferenceAttribution>,
    ) -> bool {
        let attribution = attribution.or_else(|| {
            self.config
                .model_provider
                .is_cli_proxy()
                .then_some(InferenceAttribution::Unknown)
        });
        let Some(attribution) = attribution else {
            return false;
        };
        // Inference identity never replaces the login used by native account maintenance.
        self.invalidate_ordinary_usage_recovery();
        let label = match attribution {
            InferenceAttribution::ServedNative { source, .. } => match source {
                InferenceNativeSource::Root => "OpenAI root account",
                InferenceNativeSource::Imported => "Imported OpenAI account",
            },
            InferenceAttribution::IntendedNative { source, .. } => match source {
                InferenceNativeSource::Root => "OpenAI root account (request not sent)",
                InferenceNativeSource::Imported => "Imported OpenAI account (request not sent)",
            },
            InferenceAttribution::Claude => "Claude",
            InferenceAttribution::Unknown => "Model provider (account unknown)",
        };
        self.on_error(format!("{label}: {message}"));
        true
    }
}
