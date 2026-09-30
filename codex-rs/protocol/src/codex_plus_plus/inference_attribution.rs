//! Non-secret identity evidence captured for a failed owned-proxy request.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum InferenceNativeSource {
    Root,
    Imported,
}

/// Discovery or the current native login cannot establish which account served a request.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum InferenceAttribution {
    ServedNative {
        source: InferenceNativeSource,
        #[serde(rename = "accountId")]
        #[ts(rename = "accountId")]
        account_id: String,
        #[serde(rename = "displayLabel")]
        #[ts(rename = "displayLabel")]
        display_label: Option<String>,
    },
    /// A matching local cooldown prevented an upstream attempt.
    IntendedNative {
        source: InferenceNativeSource,
        #[serde(rename = "accountId")]
        #[ts(rename = "accountId")]
        account_id: String,
        #[serde(rename = "displayLabel")]
        #[ts(rename = "displayLabel")]
        display_label: Option<String>,
    },
    /// The frozen inventory proves the provider, without asserting a Claude account identity.
    Claude,
    Unknown,
}
