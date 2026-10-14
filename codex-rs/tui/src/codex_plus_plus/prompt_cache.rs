//! Reported prompt caching observed during this app's connection to a loaded thread.

use codex_app_server_protocol::RawResponseCompletedNotification;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::Turn;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Default)]
struct Usage {
    responses: HashSet<String>,
    input: u128,
    cached: u128,
    latest: Option<(i64, i64)>,
    miss_until: Option<Instant>,
    has_prior_context: bool,
}

/// Shared by the existing per-thread event store and its currently displayed widget.
#[derive(Clone, Debug, Default)]
pub(crate) struct PromptCache(Arc<Mutex<Usage>>);

pub(crate) enum CacheObservation {
    Active,
    Background,
    Replay,
}

impl PromptCache {
    pub(crate) fn observe_history(&self, turns: &[Turn]) {
        if !turns.is_empty() {
            #[expect(clippy::expect_used)]
            let mut usage = self.0.lock().expect("prompt cache state poisoned");
            usage.has_prior_context = true;
        }
    }

    pub(crate) fn observe(&self, notification: &ServerNotification, active: bool) {
        if let ServerNotification::RawResponseCompleted(completed) = notification {
            self.observe_completion(
                completed,
                if active {
                    CacheObservation::Active
                } else {
                    CacheObservation::Background
                },
            );
        }
    }

    pub(crate) fn observe_completion(
        &self,
        completed: &RawResponseCompletedNotification,
        observation: CacheObservation,
    ) {
        #[expect(clippy::expect_used)]
        let mut usage = self.0.lock().expect("prompt cache state poisoned");
        if !usage.responses.insert(completed.response_id.clone()) {
            return;
        }
        let has_prior_context = std::mem::replace(&mut usage.has_prior_context, /*src*/ true);
        if matches!(observation, CacheObservation::Replay) {
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
        usage.latest = counts.map(|counts| (counts.input_tokens, counts.cached_input_tokens));
        if has_prior_context
            && matches!(observation, CacheObservation::Active)
            && counts.is_some_and(|counts| counts.cached_input_tokens == 0)
        {
            usage.miss_until = Some(Instant::now() + Duration::from_secs(/*secs*/ 3));
        } else if matches!(observation, CacheObservation::Background) {
            usage.miss_until = None;
        }
    }

    /// Keep a miss visible briefly even when the next response arrives immediately.
    pub(crate) fn status_line(&self) -> (String, Option<Duration>) {
        #[expect(clippy::expect_used)]
        let usage = self.0.lock().expect("prompt cache state poisoned");
        if let Some(remaining) = usage
            .miss_until
            .and_then(|deadline| deadline.checked_duration_since(Instant::now()))
            .filter(|remaining| !remaining.is_zero())
        {
            return ("Cache 0.0% (miss)".into(), Some(remaining));
        }
        let text = match usage.latest {
            Some((input, cached)) => format!("Cache {:.1}%", 100.0 * cached as f64 / input as f64),
            None => "Cache unknown".into(),
        };
        (text, None)
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
