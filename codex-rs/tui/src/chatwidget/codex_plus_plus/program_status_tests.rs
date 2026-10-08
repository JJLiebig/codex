use super::*;
use crate::chatwidget::tests::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn program_status_follows_live_outcomes_and_ignores_replayed_completion() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.local_settings.tui.terminal_title = Some(Vec::new());
    chat.local_settings.tui.notification_settings.notifications = Notifications::Enabled(false);
    chat.thread_name = Some("Session".into());
    let mut reports = vec![chat.current_program_status()];
    handle_turn_started(&mut chat, "turn");
    reports.push(chat.current_program_status());
    chat.add_async_questions(
        "question",
        &[AsyncUserInputQuestion {
            title: "PRIVATE QUESTION".into(),
            options: None,
        }],
    );
    reports.push(chat.current_program_status());
    chat.bottom_pane.clear_pending_questions();
    reports.push(chat.current_program_status());
    for (outcome, expected) in [
        (TurnStatus::Completed, State::Done),
        (TurnStatus::Failed, State::Error),
        (TurnStatus::Interrupted, State::Idle),
    ] {
        handle_turn_started(&mut chat, "turn");
        chat.handle_server_notification(
            ServerNotification::TurnCompleted(TurnCompletedNotification {
                thread_id: "thread".into(),
                turn: app_server_turn(
                    "turn", outcome, /*duration_ms*/ None, /*error*/ None,
                ),
            }),
            /*replay_kind*/ None,
        );
        let status = chat.current_program_status();
        assert_eq!(status.state, expected);
        reports.push(status);
    }
    for replay in [ReplayKind::ResumeInitialMessages, ReplayKind::ThreadSnapshot] {
        chat.handle_server_notification(ServerNotification::TurnCompleted(TurnCompletedNotification {
            thread_id: "thread".into(),
            turn: app_server_turn("old-failure", TurnStatus::Failed, /*duration_ms*/ None, Some(AppServerTurnError {
                message: "PRIVATE HISTORICAL ERROR".into(), codex_error_info: None, additional_details: None,
                inference_attribution: None, usage_limit_observed_at_ns: None, misalignment: None,
            })),
        }), Some(replay));
        assert_eq!(chat.current_program_status().state, State::Idle);
    }
    chat.record_program_status_completion(&TurnStatus::Completed, /*from_replay*/ true);
    assert_eq!(chat.current_program_status().state, State::Idle);
    insta::assert_debug_snapshot!(reports);
}

#[tokio::test]
async fn program_status_reports_actual_approval_and_resumes_work() {
    let (mut chat, _rx, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    handle_turn_started(&mut chat, "turn");
    let request = ExecApprovalRequestEvent {
        kind: Default::default(),
        call_id: "call".into(),
        approval_id: Some("call".into()),
        turn_id: "turn".into(),
        environment_id: None,
        command: vec!["SECRET COMMAND".into()],
        cwd: AbsolutePathBuf::current_dir().unwrap(),
        reason: None,
        network_approval_context: None,
        proposed_execpolicy_amendment: None,
        proposed_network_policy_amendments: None,
        additional_permissions: None,
        available_decisions: None,
    };
    handle_exec_approval_request(&mut chat, "request", request);
    assert_eq!(
        chat.current_program_status(),
        Status {
            state: State::Blocked,
            kind: Some(Kind::Permission),
            message: Some("Approval required".into())
        }
    );
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(chat.current_program_status().state, State::Working);
}
