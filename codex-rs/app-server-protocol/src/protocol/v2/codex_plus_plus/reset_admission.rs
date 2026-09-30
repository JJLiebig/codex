use crate::JsonSchema;
use crate::TS;
use codex_protocol::inference_attribution::InferenceNativeSource;
use serde::Deserialize;
use serde::Serialize;

/// An existing live wait's exact native source; this read never changes account selection.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct UsageResetTargetParams {
    pub thread_id: String,
    pub turn_id: String,
    pub source: InferenceNativeSource,
    pub account_id: String,
    /// Earliest eligible completion, in Unix seconds on the inference host.
    #[ts(type = "number")]
    pub failed_at: i64,
    /// When supplied, admission requires this exact durable completion.
    #[ts(optional = nullable)]
    pub completion_id: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct UsageResetCompletion {
    pub id: String,
    pub source: InferenceNativeSource,
    pub account_id: String,
    /// Unix seconds; does not prove present quota or proxy readiness.
    #[ts(type = "number")]
    pub completed_at: i64,
}
