use super::ChatWidget;
use codex_protocol::openai_models::ModelPreset;

impl ChatWidget {
    pub(in crate::chatwidget) fn ignore_empty_model_catalogue(
        &self,
        presets: &[ModelPreset],
    ) -> bool {
        presets.is_empty() && !self.config.model_provider.is_cli_proxy()
    }
}
