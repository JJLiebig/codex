//! Reported prompt caching observed during this app's connection to a loaded thread.

use codex_app_server_protocol::RawResponseCompletedNotification;
use codex_app_server_protocol::ServerNotification;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

#[derive(Debug, Default)]
struct Usage {
    responses: HashMap<String, bool>,
    input: u128,
    cached: u128,
}

/// Shared by the existing per-thread event store and its currently displayed widget.
#[derive(Clone, Debug, Default)]
pub(crate) struct PromptCache(Arc<Mutex<Usage>>);

impl PromptCache {
    pub(crate) fn observe(&self, notification: &ServerNotification, active: bool) {
        if let ServerNotification::RawResponseCompleted(completed) = notification {
            self.observe_completion(completed);
            if !active {
                self.take_miss(&completed.response_id);
            }
        }
    }

    pub(crate) fn observe_completion(&self, completed: &RawResponseCompletedNotification) {
        #[expect(clippy::expect_used)]
        let mut usage = self.0.lock().expect("prompt cache state poisoned");
        if usage.responses.contains_key(&completed.response_id) {
            return;
        }
        let reported = completed
            .usage_metadata
            .as_ref()
            .and_then(|metadata| metadata.metadata.as_ref())
            .is_some_and(|raw| {
                raw.get("input_tokens")
                    .and_then(serde_json::Value::as_i64)
                    .is_some()
                    && raw
                        .pointer("/input_tokens_details/cached_tokens")
                        .and_then(serde_json::Value::as_i64)
                        .is_some()
            });
        let counts = completed.usage.as_ref().filter(|counts| {
            reported
                && counts.input_tokens > 0
                && (0..=counts.input_tokens).contains(&counts.cached_input_tokens)
        });
        if let Some(counts) = counts {
            usage.input += counts.input_tokens as u128;
            usage.cached += counts.cached_input_tokens as u128;
        }
        usage.responses.insert(
            completed.response_id.clone(),
            counts.is_some_and(|counts| counts.cached_input_tokens == 0),
        );
    }

    pub(crate) fn take_miss(&self, response_id: &str) -> bool {
        #[expect(clippy::expect_used)]
        let mut usage = self.0.lock().expect("prompt cache state poisoned");
        std::mem::take(usage.responses.entry(response_id.to_owned()).or_default())
    }

    pub(crate) fn status(&self) -> String {
        #[expect(clippy::expect_used)]
        let usage = self.0.lock().expect("prompt cache state poisoned");
        if usage.input == 0 {
            "Unknown (no reported cache usage this session)".into()
        } else {
            format!(
                "{:.1}% of reported input this session",
                100.0 * usage.cached as f64 / usage.input as f64
            )
        }
    }
}

#[cfg(test)]
#[path = "prompt_cache_tests.rs"]
mod tests;
