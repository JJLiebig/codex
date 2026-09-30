use super::*;
use crate::chatwidget::tests::app_server_turn;
use crate::chatwidget::tests::handle_turn_started;
use crate::chatwidget::tests::make_chatwidget_manual;
use codex_protocol::inference_attribution::InferenceScope;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn inference_status_tracks_scoped_success_and_failure_without_native_quota() {
    let (mut chat, mut events, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.config.model_provider =
        codex_model_provider_info::ModelProviderInfo::create_cli_proxy_provider();
    chat.thread_id = Some(ThreadId::new());
    let thread_id = chat.thread_id.unwrap().to_string();
    chat.has_chatgpt_account = true;
    chat.has_codex_backend_auth = true;
    chat.status_account_display = Some(StatusAccountDisplay::ChatGpt {
        email: Some("maintenance@example.com".into()),
        plan: None,
    });
    let maintenance = (
        chat.status_account_display.clone(),
        chat.has_chatgpt_account,
        chat.has_codex_backend_auth,
    );
    handle_turn_started(&mut chat, "turn");
    let quota: RateLimitSnapshot = serde_json::from_value(serde_json::json!({
        "limitId": "codex", "primary": {"usedPercent": 25, "windowDurationMins": 300}
    }))
    .unwrap();
    let native = InferenceAttribution::ServedNative {
        source: InferenceNativeSource::Imported,
        account_id: "acct_a".into(),
        display_label: Some("Work account".into()),
    };
    let mut rendered = Vec::new();
    for attribution in [
        native.clone(),
        InferenceAttribution::Claude,
        InferenceAttribution::Unknown,
    ] {
        let mut turn = app_server_turn(
            "turn",
            TurnStatus::Completed,
            /*duration_ms*/ None,
            /*error*/ None,
        );
        turn.inference_attribution = Some(attribution.clone());
        // A canonical completion alone must update identity, even with no token usage.
        chat.handle_server_notification(
            ServerNotification::TurnCompleted(
                codex_app_server_protocol::TurnCompletedNotification {
                    thread_id: thread_id.clone(),
                    turn,
                },
            ),
            /*replay_kind*/ None,
        );
        assert!(chat.token_info.is_none());
        assert!(chat.inference_status_limits().is_empty());
        let notification = ServerNotification::AccountRateLimitsUpdated(
            codex_app_server_protocol::AccountRateLimitsUpdatedNotification {
                inference: Some(InferenceScope {
                    thread_id: thread_id.clone(),
                    turn_id: "turn".into(),
                    attribution: attribution.clone(),
                }),
                rate_limits: quota.clone(),
            },
        );
        chat.handle_server_notification(notification.clone(), /*replay_kind*/ None);
        let expected =
            chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::FiveHourLimit);
        // Both a late same-account read and maintenance refresh remain in the native cache.
        chat.on_rate_limit_snapshot(Some(RateLimitSnapshot {
            primary: None,
            ..quota.clone()
        }));
        chat.finish_status_rate_limit_refresh(/*request_id*/ 0, vec![quota.clone()]);
        for (foreign_thread, foreign_turn) in [
            (ThreadId::new().to_string(), "turn"),
            (thread_id.clone(), "old-turn"),
        ] {
            let ServerNotification::AccountRateLimitsUpdated(mut late) = notification.clone()
            else {
                unreachable!()
            };
            let scope = late.inference.as_mut().unwrap();
            scope.thread_id = foreign_thread;
            scope.turn_id = foreign_turn.into();
            late.rate_limits.primary = None;
            chat.handle_server_notification(
                ServerNotification::AccountRateLimitsUpdated(late),
                /*replay_kind*/ None,
            );
        }
        assert_eq!(
            chat.status_line_value_for_item(crate::bottom_pane::StatusLineItem::FiveHourLimit),
            expected
        );
        while events.try_recv().is_ok() {}
        chat.add_status_output(/*refreshing_rate_limits*/ true, Some(0));
        while let Ok(event) = events.try_recv() {
            if let crate::app_event::AppEvent::InsertHistoryCell(cell) = event {
                rendered.extend(
                    cell.display_lines(/*width*/ 100)
                        .into_iter()
                        .map(|line| line.to_string())
                        .filter(|line| {
                            line.contains("Account:")
                                || line.contains("limit:")
                                || line.contains("Token usage:")
                        }),
                );
            }
        }
    }
    assert_eq!(
        (
            chat.status_account_display.clone(),
            chat.has_chatgpt_account,
            chat.has_codex_backend_auth
        ),
        maintenance
    );
    let mut error = app_server_turn(
        "turn",
        TurnStatus::Failed,
        /*duration_ms*/ None,
        Some(codex_app_server_protocol::TurnError {
            message: "quota".into(),
            usage_limit_observed_at_ns: None,
            inference_attribution: Some(native.clone()),
            codex_error_info: None,
            additional_details: None,
            misalignment: None,
        }),
    );
    error.inference_attribution = Some(InferenceAttribution::Claude);
    chat.handle_server_notification(
        ServerNotification::TurnCompleted(codex_app_server_protocol::TurnCompletedNotification {
            thread_id,
            turn: error,
        }),
        /*replay_kind*/ None,
    );
    assert_eq!(chat.inference_display.attribution, Some(native));
    assert!(chat.inference_status_limits().is_empty());
    chat.handle_server_notification(
        ServerNotification::ThreadTokenUsageUpdated(
            codex_app_server_protocol::ThreadTokenUsageUpdatedNotification {
                thread_id: chat.thread_id.unwrap().to_string(),
                turn_id: "turn".into(),
                inference_attribution: Some(InferenceAttribution::Claude),
                token_usage: codex_app_server_protocol::ThreadTokenUsage::from(
                    codex_protocol::protocol::TokenUsageInfo {
                        total_token_usage: Default::default(),
                        last_token_usage: Default::default(),
                        model_context_window: None,
                    },
                ),
            },
        ),
        /*replay_kind*/ None,
    );
    assert_eq!(
        chat.inference_display.attribution,
        Some(InferenceAttribution::Claude)
    );
    insta::assert_snapshot!("owned_inference_status", rendered.join("\n"));
}

#[tokio::test]
async fn metadata_only_resume_restores_identity_without_replaying_old_pages() {
    let (mut chat, _events, _ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    let mut latest = app_server_turn(
        "latest",
        TurnStatus::Completed,
        /*duration_ms*/ None,
        /*error*/ None,
    );
    latest.items_view = codex_app_server_protocol::TurnItemsView::NotLoaded;
    latest.inference_attribution = Some(InferenceAttribution::Claude);
    chat.replay_thread_turns(vec![latest.clone()], ReplayKind::ResumeInitialMessages);
    latest.id = "old".into();
    latest.inference_attribution = Some(InferenceAttribution::Unknown);
    chat.replay_thread_turns(vec![latest], ReplayKind::ResumeInitialMessages);
    assert_eq!(
        chat.inference_display.attribution,
        Some(InferenceAttribution::Claude)
    );
    assert!(chat.usage_reset_wait.is_none());
}
