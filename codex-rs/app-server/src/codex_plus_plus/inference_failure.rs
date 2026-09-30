use codex_app_server_protocol::TurnError;
use codex_protocol::protocol::ErrorEvent;

pub(super) fn completed_error(error: Option<ErrorEvent>) -> Option<TurnError> {
    error
        .filter(|error| error.inference_attribution.is_some())
        .map(error_event)
}

pub(super) fn error_event(error: ErrorEvent) -> TurnError {
    TurnError {
        inference_attribution: error.inference_attribution,
        message: error.message,
        codex_error_info: error.codex_error_info.map(Into::into),
        additional_details: None,
        misalignment: error.misalignment.map(Into::into),
    }
}

#[cfg(test)]
#[path = "inference_failure_tests.rs"]
mod tests;
