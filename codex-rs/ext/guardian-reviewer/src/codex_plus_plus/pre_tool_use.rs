use crate::guardian_truncate_text;
use codex_protocol::approvals::GuardianAssessmentAction;
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

const GUARDIAN_MAX_ASSESSMENT_REASON_TOKENS: usize = 50;
const GUARDIAN_MAX_ASSESSMENT_INPUT_TOKENS: usize = 100;

fn bounded_assessment_input(value: &Value) -> Value {
    let (summary, truncated) =
        guardian_truncate_text(&value.to_string(), GUARDIAN_MAX_ASSESSMENT_INPUT_TOKENS);
    if truncated {
        Value::String(summary)
    } else {
        value.clone()
    }
}

#[derive(Serialize)]
struct PreToolUseApprovalAction<'a> {
    tool: &'static str,
    tool_name: &'a str,
    tool_input: &'a Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    execution_target: Option<Value>,
    reason: &'a str,
    cwd: &'a Path,
}

pub(crate) fn action_json(
    tool_name: &str,
    tool_input: &Value,
    execution_target: Option<&Value>,
    reason: &str,
    cwd: &Path,
) -> serde_json::Result<Value> {
    serde_json::to_value(PreToolUseApprovalAction {
        tool: "pre_tool_use",
        tool_name,
        tool_input,
        execution_target: execution_target.map(bounded_assessment_input),
        reason,
        cwd,
    })
}

pub(crate) fn assessment_action(
    tool_name: &str,
    tool_input: &Value,
    reason: &str,
) -> GuardianAssessmentAction {
    GuardianAssessmentAction::PreToolUse {
        tool_name: guardian_truncate_text(tool_name, GUARDIAN_MAX_ASSESSMENT_REASON_TOKENS).0,
        tool_input: bounded_assessment_input(tool_input),
        reason: guardian_truncate_text(reason, GUARDIAN_MAX_ASSESSMENT_REASON_TOKENS).0,
    }
}
