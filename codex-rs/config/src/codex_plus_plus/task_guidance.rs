use crate::ConfigLayerStack;

/// Whether to include the additional GPT-6.2 task guidance.
pub fn gpt_6_2_task_guidance(config: &ConfigLayerStack) -> bool {
    config
        .effective_config()
        .get("gpt_6_2_task_guidance")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}
