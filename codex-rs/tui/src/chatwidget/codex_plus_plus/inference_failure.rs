use super::ChatWidget;
use codex_app_server_protocol::TurnError;
use codex_protocol::inference_attribution::InferenceAttribution;
use codex_protocol::inference_attribution::InferenceNativeSource;

impl ChatWidget {
    pub(in crate::chatwidget) fn handle_terminal_inference_error(&mut self, error: TurnError) {
        let attribution = error.inference_attribution.or_else(|| {
            self.config
                .model_provider
                .is_cli_proxy()
                .then_some(InferenceAttribution::Unknown)
        });
        let Some(attribution) = attribution else {
            self.handle_non_retry_error(error.message, error.codex_error_info);
            return;
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
        self.on_error(format!("{label}: {}", error.message));
    }
}
