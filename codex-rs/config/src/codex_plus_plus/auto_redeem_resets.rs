use std::num::NonZeroU64;

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Thresholds for automatic redemption of earned usage-reset credits.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AutoRedeemResets {
    /// Redeem credits this close to expiry. Omit to disable expiry-triggered redemption.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_expiry_minutes: Option<NonZeroU64>,
    /// Redeem at weekly exhaustion when the reset is this far away. Omit to disable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekly_exhausted_min_wait_hours: Option<NonZeroU64>,
}

impl Default for AutoRedeemResets {
    fn default() -> Self {
        Self {
            before_expiry_minutes: NonZeroU64::new(60),
            weekly_exhausted_min_wait_hours: None,
        }
    }
}
