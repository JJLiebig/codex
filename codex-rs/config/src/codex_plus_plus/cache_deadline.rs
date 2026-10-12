use crate::ConfigLayerStack;
use std::time::Duration;

/// Opt-in delay for this exact model. Missing or zero disables cache-deadline compaction.
pub fn cache_deadline_compact_interval(config: &ConfigLayerStack, model: &str) -> Option<Duration> {
    config
        .effective_config()
        .get("cache_deadline_compact_seconds")?
        .get(model)?
        .as_integer()
        .and_then(|seconds| u64::try_from(seconds).ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
}
