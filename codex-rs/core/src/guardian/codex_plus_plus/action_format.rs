use super::approval_request::GuardianApprovalRequest;
use super::approval_request::guardian_approval_request_to_json;
use super::prompt::guardian_truncate_text;
use serde::ser::Error as _;
use serde_json::Value;
const GUARDIAN_MAX_ACTION_TOKENS: usize = 1_000;
const GUARDIAN_MAX_ACTION_SUMMARY_TOKENS: usize = 100;
const GUARDIAN_MAX_ACTION_BYTES: usize = 50_000 * 4;
const GUARDIAN_MAX_ACTION_STRING_TOKENS: usize = 16_000;
const GUARDIAN_MAX_ASSESSMENT_INPUT_TOKENS: usize = 100;
fn truncate_guardian_action_value(value: Value) -> (Value, bool) {
    match value {
        Value::String(text) => {
            let (text, truncated) =
                guardian_truncate_text(&text, GUARDIAN_MAX_ACTION_STRING_TOKENS);
            (Value::String(text), truncated)
        }
        Value::Array(values) => {
            let mut truncated = false;
            let values = values
                .into_iter()
                .map(|value| {
                    let (value, value_truncated) = truncate_guardian_action_value(value);
                    truncated |= value_truncated;
                    value
                })
                .collect::<Vec<_>>();
            (Value::Array(values), truncated)
        }
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            let mut truncated = false;
            let values = entries
                .into_iter()
                .map(|(key, value)| {
                    let (value, value_truncated) = truncate_guardian_action_value(value);
                    truncated |= value_truncated;
                    (key, value)
                })
                .collect();
            (Value::Object(values), truncated)
        }
        other => (other, false),
    }
}

pub(crate) fn bounded_guardian_assessment_input(value: &Value) -> Value {
    let (summary, truncated) =
        guardian_truncate_text(&value.to_string(), GUARDIAN_MAX_ASSESSMENT_INPUT_TOKENS);
    if truncated {
        Value::String(summary)
    } else {
        value.clone()
    }
}

fn bounded_guardian_pretty_assessment_input(value: &Value) -> Value {
    let value = bounded_guardian_assessment_input(value);
    let Ok(pretty) = serde_json::to_string_pretty(&value) else {
        return value;
    };
    let (summary, truncated) =
        guardian_truncate_text(&pretty, GUARDIAN_MAX_ASSESSMENT_INPUT_TOKENS);
    if truncated {
        Value::String(summary)
    } else {
        value
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FormattedGuardianAction {
    pub(crate) text: String,
    pub(crate) truncated: bool,
}

pub(crate) fn format_guardian_action_pretty(
    action: &GuardianApprovalRequest,
) -> serde_json::Result<FormattedGuardianAction> {
    let execution_target_truncated = if let GuardianApprovalRequest::PreToolUse {
        execution_target: Some(target),
        ..
    } = action
    {
        guardian_truncate_text(&target.to_string(), GUARDIAN_MAX_ASSESSMENT_INPUT_TOKENS).1
    } else {
        false
    };
    let value = guardian_approval_request_to_json(action)?;
    let (value, fields_truncated) = truncate_guardian_action_value(value);
    let text = serde_json::to_string_pretty(&value)?;
    let (_, action_truncated) = guardian_truncate_text(&text, GUARDIAN_MAX_ACTION_TOKENS);
    let text = if action_truncated {
        let summary = guardian_truncate_text(&text, GUARDIAN_MAX_ACTION_SUMMARY_TOKENS).0;
        serde_json::to_string_pretty(&serde_json::json!({
            "tool": value.get("tool"),
            "tool_name": value.get("tool_name").map(bounded_guardian_assessment_input),
            "tool_input": value.get("tool_input").map(bounded_guardian_pretty_assessment_input),
            "execution_target": value.get("execution_target").map(bounded_guardian_pretty_assessment_input),
            "reason": value.get("reason").map(bounded_guardian_assessment_input),
            "summary": summary,
        }))?
    } else {
        text
    };
    Ok(FormattedGuardianAction {
        text: enforce_guardian_action_byte_limit(text)?,
        truncated: execution_target_truncated || fields_truncated || action_truncated,
    })
}

fn enforce_guardian_action_byte_limit(text: String) -> serde_json::Result<String> {
    if text.len() > GUARDIAN_MAX_ACTION_BYTES {
        return Err(serde_json::Error::custom(format!(
            "Guardian action exceeds the {GUARDIAN_MAX_ACTION_BYTES}-byte review limit"
        )));
    }
    Ok(text)
}

#[cfg(test)]
#[path = "action_format_tests.rs"]
mod tests;
