use super::ChatWidget;
use crate::chatwidget::model_popups::ADVANCED_REASONING_VIEW_ID;
use crate::chatwidget::model_popups::MODEL_REASONING_VIEW_ID;
use crate::chatwidget::model_popups::PLAN_REASONING_SCOPE_VIEW_ID;
use codex_protocol::openai_models::ModelPreset;

impl ChatWidget {
    pub(in crate::chatwidget) fn ignore_empty_model_catalogue(
        &self,
        presets: &[ModelPreset],
    ) -> bool {
        presets.is_empty() && !self.config.model_provider.is_cli_proxy()
    }

    pub(in crate::chatwidget) fn dismiss_stale_owned_reasoning_choices(&mut self) {
        if self.config.model_provider.is_cli_proxy() {
            // These actions capture model metadata. Reopen them from the refreshed parent.
            for view_id in [
                PLAN_REASONING_SCOPE_VIEW_ID,
                ADVANCED_REASONING_VIEW_ID,
                MODEL_REASONING_VIEW_ID,
            ] {
                self.bottom_pane.dismiss_view_by_id(view_id);
            }
        }
    }
}
