use super::*;
use crate::chatwidget::tests::make_chatwidget_manual_with_sender;
use crate::diff_model::FileChange;
use crate::exec_cell::CommandOutput;
use crate::exec_cell::new_active_exec_command;
use crate::history_cell;
use codex_protocol::mcp::CallToolResult;
fn text(lines: Vec<Line<'static>>) -> String {
    lines
        .into_iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}
#[tokio::test]
async fn compact_history_policy_keeps_complete_transcript_and_raw_output() {
    let (mut chat, _sender, mut events, _operations) = make_chatwidget_manual_with_sender().await;
    chat.config.codex_plus_plus_tool_activity = codex_config::ToolActivityPresentation::Compact;
    let mut exec = new_active_exec_command(
        "exec".to_string(),
        vec!["printf one".to_string()],
        Vec::new(),
        CommandExecutionSource::Agent,
        /*interaction_input*/ None,
        /*animations_enabled*/ false,
    );
    exec.complete_call(
        "exec",
        CommandOutput::new(/*exit_code*/ 0, "one\n".to_string()),
        std::time::Duration::from_millis(5),
    );
    chat.raw_output_mode = true;
    let exec = chat.compact_history_cell(Box::new(exec));
    chat.raw_output_mode = false;
    let invocation = history_cell::McpInvocation {
        server: "workspace".to_string(),
        tool: "inspect".to_string(),
        arguments: Some(serde_json::json!({"path": "README.md"})),
    };
    let parallel = chat.compact_history_cell(Box::new(history_cell::new_active_mcp_tool_call(
        "mcp-a".to_string(),
        invocation.clone(),
        /*animations_enabled*/ false,
    )));
    let mut mcp = history_cell::new_active_mcp_tool_call(
        "mcp-b".to_string(),
        invocation,
        /*animations_enabled*/ false,
    );
    mcp.complete(
        std::time::Duration::from_millis(5),
        Ok(CallToolResult {
            content: vec![serde_json::json!({"type": "text", "text": "full result"})],
            structured_content: None,
            is_error: None,
            meta: None,
        }),
    );
    chat.transcript.active_cell = Some(Box::new(mcp));
    chat.retain_fast_compact_completion_status();
    let mcp = chat
        .transcript
        .take_active_cell()
        .expect("completed MCP cell");
    let mcp = chat.compact_history_cell(mcp);
    let web = chat.compact_history_cell(Box::new(history_cell::new_web_search_call(
        "web".to_string(),
        "compact policy".to_string(),
        codex_app_server_protocol::WebSearchAction::Other,
    )));
    let hook_source_path = chat.config.cwd.clone();
    let hook_run = |id: &str,
                    event_name: codex_app_server_protocol::HookEventName,
                    status: codex_app_server_protocol::HookRunStatus,
                    entries: Vec<codex_app_server_protocol::HookOutputEntry>| {
        codex_app_server_protocol::HookRunSummary {
            id: id.to_string(),
            event_name,
            handler_type: codex_app_server_protocol::HookHandlerType::Command,
            execution_mode: codex_app_server_protocol::HookExecutionMode::Sync,
            scope: codex_app_server_protocol::HookScope::Turn,
            source_path: hook_source_path.clone().into(),
            source: codex_app_server_protocol::HookSource::User,
            display_order: 0,
            status,
            status_message: Some("checking command policy".to_string()),
            started_at: 1,
            completed_at: Some(2),
            duration_ms: Some(1),
            entries,
        }
    };
    chat.on_hook_completed(hook_run(
        "pre-tool-use:0:/tmp/hooks.json",
        codex_app_server_protocol::HookEventName::PreToolUse,
        codex_app_server_protocol::HookRunStatus::Completed,
        vec![codex_app_server_protocol::HookOutputEntry {
            kind: codex_app_server_protocol::HookOutputEntryKind::Feedback,
            text: "DCG flagged core.git:git-alias-semantic-unverified".to_string(),
        }],
    ));
    let completed_hook = match events.try_recv().expect("completed hook history") {
        crate::app_event::AppEvent::InsertHistoryCell(cell) => cell,
        event => panic!("expected completed hook history, got {event:?}"),
    };
    chat.on_hook_completed(hook_run(
        "post-tool-use:0:/tmp/hooks.json",
        codex_app_server_protocol::HookEventName::PostToolUse,
        codex_app_server_protocol::HookRunStatus::Failed,
        vec![codex_app_server_protocol::HookOutputEntry {
            kind: codex_app_server_protocol::HookOutputEntryKind::Error,
            text: "hook exited with code 7".to_string(),
        }],
    ));
    let failed_hook = match events.try_recv().expect("failed hook history") {
        crate::app_event::AppEvent::InsertHistoryCell(cell) => cell,
        event => panic!("expected failed hook history, got {event:?}"),
    };
    let patch = chat.compact_history_cell(Box::new(history_cell::new_patch_event(
        std::collections::HashMap::from([(
            std::path::PathBuf::from("src/lib.rs"),
            FileChange::Add {
                content: "new\n".to_string(),
            },
        )]),
        &chat.config.cwd,
    )));
    insta::assert_snapshot!(format!(
        "status:\n{}\nmain:\n{}\n{}\n{}\n{}\n{}\n{}\n{}\ntranscript:\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        chat.status_state.current_status.header,
        text(exec.display_lines(80)),
        text(parallel.display_lines(80)),
        text(mcp.display_lines(80)),
        text(web.display_lines(80)),
        text(completed_hook.display_lines(80)),
        text(failed_hook.display_lines(80)),
        text(patch.display_lines(80)),
        text(exec.transcript_lines(80)),
        text(parallel.transcript_lines(80)),
        text(mcp.transcript_lines(80)),
        text(web.transcript_lines(80)),
        text(completed_hook.transcript_lines(80)),
        text(failed_hook.transcript_lines(80)),
        text(patch.transcript_lines(80)),
    ), @r###"
    status:
    Called workspace.inspect({"path":"README.md"})
    main:





    • Hook failed
      └ hook exited with code 7
    • Added src/lib.rs (+1 -0)
    transcript:
    $ 'printf one'
    one
    ✓ • 5ms
    • Calling workspace.inspect({"path":"README.md"})
    • Called workspace.inspect({"path":"README.md"})
      └ full result
    • Searched the web for compact policy
    • Hook completed
      └ DCG flagged core.git:git-alias-semantic-unverified
    • Hook failed
      └ hook exited with code 7
    • Added src/lib.rs (+1 -0)
        1 +new
    "###);
    assert!(text(exec.raw_lines()).contains("one"));
}
