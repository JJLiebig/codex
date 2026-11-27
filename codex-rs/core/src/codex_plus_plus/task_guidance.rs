use codex_protocol::models::BaseInstructions;
use codex_protocol::models::BaseInstructionsProvenance;
use codex_protocol::models::ResponseItem;

use crate::context::ContextualUserFragment;
use crate::context::DeveloperInstructions;
use crate::session::step_context::StepContext;

const GUIDANCE: &str = include_str!("../../assets/codex_plus_plus/gpt_6_2_task_guidance.md");

pub(crate) fn prepend(
    mut input: Vec<ResponseItem>,
    step_context: &StepContext,
    base_instructions: &BaseInstructions,
) -> Vec<ResponseItem> {
    let turn = &step_context.turn;
    if matches!(
        step_context.settings.model_info.slug.as_str(),
        "gpt-6.2-sol" | "gpt-6.2-astra"
    ) && matches!(
        base_instructions.provenance,
        Some(BaseInstructionsProvenance::Model { .. })
    ) && !crate::guardian::is_basic_session_source(&turn.session_source)
        && codex_config::gpt_6_2_task_guidance(&turn.config.config_layer_stack)
    {
        input.insert(
            0,
            ContextualUserFragment::into(DeveloperInstructions::new(GUIDANCE.trim_end())),
        );
    }
    input
}
