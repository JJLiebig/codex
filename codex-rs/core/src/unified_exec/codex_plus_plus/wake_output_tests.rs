use super::*;
use codex_utils_output_truncation::with_serialization_allowance;
use std::time::Duration;

#[test]
fn wake_notice_fits_the_original_history_budget() {
    for policy in [TruncationPolicy::Bytes(1024), TruncationPolicy::Tokens(256)] {
        let output = WakeOutput::new(ExecCommandToolOutput {
            event_call_id: "call".into(),
            chunk_id: "chunk".into(),
            wall_time: Duration::ZERO,
            raw_output: vec![b'x'; 4096],
            truncation_policy: policy,
            max_output_tokens: Some(4096),
            process_id: Some(1000),
            exit_code: None,
            original_token_count: Some(1024),
            output_omitted_bytes: None,
            hook_command: None,
        });
        let response = output.to_response_item(
            "call",
            &ToolPayload::Function {
                arguments: "{}".into(),
            },
        );
        let ResponseInputItem::FunctionCallOutput { output, .. } = response else {
            panic!("expected function output");
        };
        let FunctionCallOutputBody::Text(text) = output.body else {
            panic!("expected text output");
        };
        assert!(text.starts_with("Completion will resume you automatically."));
        assert!(text.contains("Process running with session ID 1000"));
        assert!(text.len() <= with_serialization_allowance(policy).byte_budget());
    }
}
