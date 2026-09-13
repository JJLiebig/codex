use crate::ConfigLayerStack;

/// Whether progress updates should be driven by meaningful events rather than elapsed time.
pub fn disable_unnecessary_updates(config: &ConfigLayerStack) -> bool {
    config
        .effective_config()
        .get("disable_unnecessary_updates")
        .and_then(toml::Value::as_bool)
        .unwrap_or(true)
}
