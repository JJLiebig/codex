use crate::ConfigLayerStack;
use std::time::Duration;

/// Tested cache lifetime for the native Astra/Sol model keys.
pub fn cache_deadline_known_lifetime(model: &str) -> Option<Duration> {
    matches!(model, "gpt-6-astra" | "gpt-6.1-sol" | "gpt-6-sol")
        .then_some(Duration::from_secs(60 * 60))
}

/// Opt-in delay for this exact model. Missing, zero, or past a known lifetime is off.
pub fn cache_deadline_compact_interval(config: &ConfigLayerStack, model: &str) -> Option<Duration> {
    config
        .effective_config()
        .get("cache_deadline_compact_seconds")?
        .get(model)?
        .as_integer()
        .and_then(|seconds| u64::try_from(seconds).ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .filter(|interval| cache_deadline_known_lifetime(model).is_none_or(|ttl| *interval < ttl))
}
