use codex_model_provider_info::AMAZON_BEDROCK_GPT_6_2_ASTRA_MODEL_ID;
use codex_model_provider_info::AMAZON_BEDROCK_GPT_6_2_SOL_MODEL_ID;
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
    let slug = step_context.settings.model_info.slug.as_str();
    let bedrock_slug = slug
        .strip_prefix("global.")
        .or_else(|| slug.strip_prefix("us."))
        .unwrap_or(slug);
    let supported_model = matches!(slug, "gpt-6.2-sol" | "gpt-6.2-astra")
        || matches!(
            bedrock_slug,
            AMAZON_BEDROCK_GPT_6_2_SOL_MODEL_ID | AMAZON_BEDROCK_GPT_6_2_ASTRA_MODEL_ID
        );
    if supported_model
        && matches!(
            base_instructions.provenance,
            Some(BaseInstructionsProvenance::Model { .. })
        )
        && !crate::guardian::is_basic_session_source(&turn.session_source)
        && codex_config::gpt_6_2_task_guidance(&turn.config.config_layer_stack)
    {
        input.insert(
            0,
            ContextualUserFragment::into(DeveloperInstructions::new(GUIDANCE.trim_end())),
        );
    }
    input
}
