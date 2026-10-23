use crate::ConfigLayerStack;
use std::time::Duration;

/// Tested cache lifetime for the native Astra/Sol model keys.
pub fn cache_deadline_known_lifetime(model: &str) -> Option<Duration> {
    matches!(model, "gpt-6-astra" | "gpt-6.1-sol" | "gpt-6-sol")
        .then_some(Duration::from_secs(60 * 60))
}

/// Opt-in compaction interval for an exact model ID.
pub fn cache_deadline_compact_interval(config: &ConfigLayerStack, model: &str) -> Option<Duration> {
    let effective = config.effective_config();
    let seconds = effective
        .get("cache_deadline_compact_seconds")
        .and_then(|models| models.get(model))?
        .as_integer()?;
    let seconds = u64::try_from(seconds).ok()?;
    Some(seconds)
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .filter(|interval| cache_deadline_known_lifetime(model).is_none_or(|ttl| *interval < ttl))
}

/// Minimum current context usage for timer-based compaction; values above 100 are capped.
pub fn cache_deadline_compact_min_context_percent(config: &ConfigLayerStack) -> u8 {
    config
        .effective_config()
        .get("cache_deadline_compact_min_context_percent")
        .and_then(toml::Value::as_integer)
        .unwrap_or(30)
        .clamp(0, 100) as u8
}

#[cfg(test)]
#[path = "cache_deadline_tests.rs"]
mod tests;
