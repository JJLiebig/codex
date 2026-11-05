//! Decode retired attribution metadata without retaining it in the live protocol.
use crate::protocol::CodexErrorInfo;
use crate::protocol::ErrorEvent;
use serde::Deserialize;
use serde::Deserializer;

impl<'de> Deserialize<'de> for ErrorEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct HistoricalError {
            message: String,
            codex_error_info: Option<CodexErrorInfo>,
            inference_attribution: Option<serde::de::IgnoredAny>,
        }
        let value = HistoricalError::deserialize(deserializer)?;
        let codex_error_info = if value.inference_attribution.is_some()
            && value
                .codex_error_info
                .as_ref()
                .is_some_and(|info| !info.affects_turn_status())
        {
            Some(CodexErrorInfo::Other)
        } else {
            value.codex_error_info
        };
        Ok(Self {
            message: value.message,
            codex_error_info,
            misalignment: None,
        })
    }
}

#[cfg(test)]
#[path = "legacy_error_tests.rs"]
mod tests;
