use super::*;
use crate::chatwidget::tests::assert_no_submit_op;
use crate::chatwidget::tests::handle_turn_completed;
use crate::chatwidget::tests::handle_turn_interrupted;
use crate::chatwidget::tests::handle_turn_started;
use crate::chatwidget::tests::make_chatwidget_manual;
use crate::chatwidget::tests::next_submit_op;
use codex_protocol::ThreadId;
use codex_protocol::inference_attribution::InferenceNativeSource;
use pretty_assertions::assert_eq;

fn account() -> AccountId {
    serde_json::from_str("\"acct_f2b6477631260f18\"").unwrap()
}

fn quota() -> GetAccountRateLimitsResponse {
    serde_json::from_value(serde_json::json!({
        "accountId": "reset-account",
        "rateLimits": {
            "limitId": "codex",
            "primary": { "usedPercent": 1, "windowDurationMins": 300, "resetsAt": 9999999999_i64 },
            "secondary": { "usedPercent": 1, "windowDurationMins": 10080, "resetsAt": 9999999999_i64 }
        }
    })).unwrap()
}

fn failure(chat: &ChatWidget) -> ServerNotification {
    serde_json::from_value(serde_json::json!({
        "method": "error",
        "params": {
            "threadId": chat.thread_id().unwrap().to_string(),
            "turnId": "failed-turn",
            "willRetry": false,
            "error": { "message": "Usage exhausted", "codexErrorInfo": "usageLimitExceeded" }
        }
    }))
    .unwrap()
}

fn complete_failure(chat: &mut ChatWidget, completed_at: i64) {
    let ServerNotification::Error(error) = failure(chat) else {
        unreachable!()
    };
    let mut turn = crate::chatwidget::tests::app_server_turn(
        "failed-turn",
        TurnStatus::Failed,
        /*duration_ms*/ None,
        Some(error.error),
    );
    turn.completed_at = Some(completed_at);
    chat.handle_server_notification(
        ServerNotification::TurnCompleted(codex_app_server_protocol::TurnCompletedNotification {
            thread_id: error.thread_id,
            turn,
        }),
        /*replay_kind*/ None,
    );
}

#[tokio::test]
async fn reset_resumes_live_failed_turn_once_with_available_matching_quota() {
    let (mut chat, _events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    handle_turn_started(&mut chat, "failed-turn");
    chat.handle_server_notification(failure(&chat), /*replay_kind*/ None);
    let failed_at = chat.usage_reset_wait.as_ref().unwrap().failed_at;
    let reset_at = failed_at - 1;
    assert_eq!(chat.usage_reset_turn(reset_at), None);
    // A completion was delivered before the failure; authoritative turn time corrects ordering.
    let server_completed_at = failed_at / 1_000_000_000 - 2;
    complete_failure(&mut chat, server_completed_at);
    assert_eq!(
        chat.usage_reset_turn((server_completed_at + 1) * 1_000_000_000 - 1),
        None
    );
    assert_eq!(chat.usage_reset_turn(reset_at), Some("failed-turn".into()));
    chat.resume_after_usage_reset("failed-turn", &account(), reset_at, &quota());
    let AppCommand::UserTurn { items, .. } = next_submit_op(&mut ops) else {
        unreachable!()
    };
    assert_eq!(
        items,
        vec![codex_app_server_protocol::UserInput::Text {
            text: "continue".into(),
            text_elements: vec![],
        }]
    );
    chat.resume_after_usage_reset("failed-turn", &account(), reset_at, &quota());
    assert_no_submit_op(&mut ops);
    handle_turn_started(&mut chat, "failed-turn");
    chat.handle_server_notification(failure(&chat), /*replay_kind*/ None);
    complete_failure(&mut chat, server_completed_at);
    // Repeated terminal/readiness notifications must not reuse the consumed reset.
    chat.resume_after_usage_reset("failed-turn", &account(), reset_at, &quota());
    assert_no_submit_op(&mut ops);
}

#[tokio::test]
async fn reset_does_not_resume_replayed_cancelled_completed_or_superseded_work() {
    for action in [
        "replay", "cancel", "complete", "new-turn", "input", "account",
    ] {
        let (mut chat, _events, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.thread_id = Some(ThreadId::new());
        handle_turn_started(&mut chat, "failed-turn");
        let replay =
            (action == "replay").then_some(crate::chatwidget::ReplayKind::ResumeInitialMessages);
        chat.handle_server_notification(failure(&chat), replay);
        match action {
            "cancel" => handle_turn_interrupted(&mut chat, "failed-turn"),
            "complete" => {
                handle_turn_completed(&mut chat, "failed-turn", /*duration_ms*/ None)
            }
            "new-turn" => handle_turn_started(&mut chat, "new-turn"),
            "input" => chat.submit_user_message("new request".into()),
            "account" => chat.update_account_state(
                /*status_account_display*/ None, /*plan_type*/ None,
                /*has_chatgpt_account*/ true, /*has_codex_backend_auth*/ true,
            ),
            _ => {}
        }
        while ops.try_recv().is_ok() {}
        chat.resume_after_usage_reset("failed-turn", &account(), i64::MAX, &quota());
        assert_no_submit_op(&mut ops);
    }
}

#[test]
fn reset_requires_matching_identity_and_available_weekly_and_short_term_quota() {
    assert!(reset_account_has_quota(&account(), &quota()));
    let mut response = quota();
    response.account_id = Some("unrelated-account".into());
    assert!(!reset_account_has_quota(&account(), &response));
    response.account_id = None;
    assert!(!reset_account_has_quota(&account(), &response));
    for weekly in [false, true] {
        for used_percent in [100, -1] {
            let mut response = quota();
            let window = if weekly {
                &mut response.rate_limits.secondary
            } else {
                &mut response.rate_limits.primary
            };
            window.as_mut().unwrap().used_percent = used_percent;
            assert!(!reset_account_has_quota(&account(), &response));
        }
    }
    let mut response = quota();
    response.rate_limits.secondary = None;
    assert!(!reset_account_has_quota(&account(), &response));
    let mut response = quota();
    response.rate_limits.spend_control_reached = Some(true);
    assert!(!reset_account_has_quota(&account(), &response));
}

#[tokio::test]
async fn owned_failure_identity_is_displayed_without_native_recovery_or_replay_admission() {
    let mut displayed = Vec::new();
    for attribution in [
        Some(InferenceAttribution::ServedNative {
            source: InferenceNativeSource::Root,
            account_id: account().to_string(),
            display_label: None,
        }),
        Some(InferenceAttribution::ServedNative {
            source: InferenceNativeSource::Imported,
            account_id: account().to_string(),
            display_label: Some("Work account".into()),
        }),
        Some(InferenceAttribution::IntendedNative {
            source: InferenceNativeSource::Imported,
            account_id: account().to_string(),
            display_label: Some("Work account".into()),
        }),
        Some(InferenceAttribution::Claude),
        Some(InferenceAttribution::Unknown),
        None,
    ] {
        for mode in ["live", "completed", "replay"] {
            let (mut chat, mut events, mut ops) =
                make_chatwidget_manual(/*model_override*/ None).await;
            chat.config.model_provider =
                codex_model_provider_info::ModelProviderInfo::create_cli_proxy_provider();
            chat.thread_id = Some(ThreadId::new());
            chat.has_chatgpt_account = true;
            let maintenance = (
                chat.status_account_display.clone(),
                chat.has_chatgpt_account,
                chat.has_codex_backend_auth,
            );
            handle_turn_started(&mut chat, "failed-turn");
            while events.try_recv().is_ok() {}
            let ServerNotification::Error(mut error) = failure(&chat) else {
                unreachable!()
            };
            error.error.inference_attribution = attribution.clone();
            if mode != "completed" {
                chat.handle_server_notification(
                    ServerNotification::Error(error.clone()),
                    (mode == "replay")
                        .then_some(crate::chatwidget::ReplayKind::ResumeInitialMessages),
                );
            }
            if mode != "replay" {
                let turn = crate::chatwidget::tests::app_server_turn(
                    "failed-turn",
                    TurnStatus::Failed,
                    /*duration_ms*/ None,
                    Some(error.error),
                );
                chat.handle_server_notification(
                    ServerNotification::TurnCompleted(
                        codex_app_server_protocol::TurnCompletedNotification {
                            thread_id: error.thread_id,
                            turn,
                        },
                    ),
                    /*replay_kind*/ None,
                );
            }
            let expected_wait = attribution.as_ref().filter(|value| {
                mode != "replay"
                    && matches!(
                        value,
                        InferenceAttribution::ServedNative { .. }
                            | InferenceAttribution::IntendedNative { .. }
                    )
            });
            assert_eq!(
                chat.usage_reset_wait
                    .as_ref()
                    .and_then(|wait| wait.attribution.as_ref()),
                expected_wait
            );
            assert_eq!(chat.usage_reset_turn(i64::MAX), None);
            chat.resume_after_usage_reset("failed-turn", &account(), i64::MAX, &quota());
            assert_no_submit_op(&mut ops);
            assert_eq!(
                (
                    chat.status_account_display.clone(),
                    chat.has_chatgpt_account,
                    chat.has_codex_backend_auth
                ),
                maintenance
            );
            let mut errors = Vec::new();
            while let Ok(event) = events.try_recv() {
                match event {
                    crate::app_event::AppEvent::InsertHistoryCell(cell) => {
                        errors.push(
                            cell.display_lines(/*width*/ 80)
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join("\n"),
                        )
                    }
                    crate::app_event::AppEvent::RefreshRateLimits { .. } => {
                        panic!("owned error requested native recovery")
                    }
                    _ => {}
                }
            }
            assert_eq!(errors.len(), 1);
            if mode == "live" {
                displayed.extend(errors);
            }
            handle_turn_interrupted(&mut chat, "failed-turn");
            assert!(chat.usage_reset_wait.is_none());
        }
    }
    insta::assert_snapshot!("owned_inference_failures", displayed.join("\n"));
}
