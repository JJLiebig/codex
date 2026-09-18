use super::*;
use crate::guardian::approval_request::guardian_assessment_action;
use codex_protocol::approvals::GuardianAssessmentAction;
use codex_utils_output_truncation::approx_token_count;
use core_test_support::PathBufExt;
use core_test_support::test_path_buf;
use pretty_assertions::assert_eq;
#[test]
fn format_guardian_action_pretty_reports_no_truncation_for_small_payload() -> serde_json::Result<()>
{
    let action = GuardianApprovalRequest::ApplyPatch {
        id: "patch-1".to_string(),
        cwd: test_path_buf("/tmp").abs().into(),
        files: Vec::new(),
        patch: "line\n".to_string(),
    };

    let rendered = format_guardian_action_pretty(&action)?;

    assert!(rendered.text.contains("\"tool\": \"apply_patch\""));
    assert!(!rendered.truncated);
    Ok(())
}

#[test]
fn format_guardian_action_pretty_caps_aggregate_nested_input() -> serde_json::Result<()> {
    let action = GuardianApprovalRequest::PreToolUse {
        id: "call-1".to_string(),
        tool_name: "nested_tool".to_string(),
        tool_input: serde_json::json!({ "values": vec![r#"\"quoted\\path\n"#.repeat(200); 100] }),
        execution_target: None,
        reason: "Review this tool call".to_string(),
        cwd: test_path_buf("/tmp").abs(),
    };

    let rendered = format_guardian_action_pretty(&action)?;
    let assessment = guardian_assessment_action(&action);
    assert!(approx_token_count(&serde_json::to_string(&assessment)?) <= 1_000);
    let GuardianAssessmentAction::PreToolUse { tool_input, .. } = assessment else {
        panic!("expected PreToolUse assessment action");
    };

    assert!(rendered.text.contains("<truncated omitted_approx_tokens="));
    assert!(rendered.text.contains("nested_tool") && rendered.text.contains("Review"));
    assert!(rendered.truncated);
    assert!(serde_json::from_str::<serde_json::Value>(&rendered.text).is_ok());
    assert!(approx_token_count(&rendered.text) <= 1_000);
    assert!(tool_input.as_str().is_some_and(|input| {
        input.contains("<truncated omitted_approx_tokens=") && input.len() < 10_000
    }));
    Ok(())
}

#[test]
fn pre_tool_use_action_bounds_reviewed_execution_target() -> serde_json::Result<()> {
    let nested_execution_target = (0..400).fold(serde_json::json!({}), |target, _| {
        serde_json::json!([target])
    });
    let action = GuardianApprovalRequest::PreToolUse {
        id: "call-1".to_string(),
        tool_name: "Bash".to_string(),
        tool_input: serde_json::json!({ "command": "rm -f reviewed-target" }),
        execution_target: Some(nested_execution_target),
        reason: "Review this shell command".to_string(),
        cwd: test_path_buf("/tmp").abs(),
    };

    let rendered = format_guardian_action_pretty(&action)?;
    let rendered_value: serde_json::Value = serde_json::from_str(&rendered.text)?;

    assert_eq!(
        rendered_value["tool_input"],
        serde_json::json!({ "command": "rm -f reviewed-target" })
    );
    assert!(
        rendered_value["execution_target"]
            .as_str()
            .is_some_and(|target| target.contains("<truncated omitted_approx_tokens="))
    );
    assert!(rendered.truncated);
    assert!(approx_token_count(&rendered.text) <= 1_000);
    Ok(())
}
