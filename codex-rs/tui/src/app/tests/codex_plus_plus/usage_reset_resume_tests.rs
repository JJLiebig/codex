use super::*;
use app_test_support::ChatGptAuthFixture;
use app_test_support::write_chatgpt_auth;
use codex_config::types::AuthCredentialsStoreMode;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn automatic_usage_reset_reads_current_account_and_submits_one_continuation() -> Result<()> {
    let backend = MockServer::start().await;
    let home = tempdir()?;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new("local-test-token")
            .account_id("reset-account")
            .chatgpt_user_id("user-a")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )
    .expect("write synthetic auth");
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "chatgpt_base_url = {:?}\ncli_auth_credentials_store = \"file\"\n",
            backend.uri()
        ),
    )?;
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    app.config.codex_home = home.path().to_path_buf().abs();
    app.config.chatgpt_base_url = backend.uri();
    app.config.sqlite = codex_state::SqliteConfig::new_for_testing(home.path().abs());
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "account_id":"reset-account", "plan_type":"pro",
            "rate_limit":{"allowed":true,"limit_reached":false,
                "secondary_window":{"used_percent":1,"limit_window_seconds":604800,
                    "reset_after_seconds":3600,"reset_at":2000000000}},
            "rate_limit_reset_credits":{"available_count":0}
        })))
        .mount(&backend)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"available_count":0,"credits":[]})),
        )
        .mount(&backend)
        .await;
    let mut server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let started = server.start_thread(&app.config).await?;
    app.enqueue_primary_thread_session(started.session, started.turns)
        .await?;
    set_chatgpt_auth(&mut app.chat_widget);
    let thread_id = app.chat_widget.thread_id().unwrap();
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}
    for (method, status, error) in [
        ("turn/started", "inProgress", serde_json::Value::Null),
        (
            "turn/completed",
            "failed",
            json!({"message":"Usage exhausted","codexErrorInfo":"usageLimitExceeded"}),
        ),
    ] {
        app.chat_widget.handle_server_notification(serde_json::from_value(json!({
            "method":method, "params":{"threadId":thread_id.to_string(),
                "turn":{"id":"failed-turn","items":[],"itemsView":"full","status":status,"error":error}}
        }))?, /*replay_kind*/ None);
    }
    // Complete the ordinary post-error recovery before the reset arrives.
    while let Ok(event) = events.try_recv() {
        if matches!(event, AppEvent::RefreshRateLimits { .. }) {
            app.handle_event(&mut tui, &mut server, event).await?;
        }
    }
    let recovered = next_usage_event(&mut events).await?;
    app.handle_event(&mut tui, &mut server, recovered).await?;
    let account_id = serde_json::from_str("\"acct_f2b6477631260f18\"")?;
    app.handle_event(
        &mut tui,
        &mut server,
        AppEvent::UsageResetCompleted {
            account_id,
            completed_at: chrono::Utc::now().timestamp_nanos_opt().unwrap(),
        },
    )
    .await?;
    let ready = next_usage_event(&mut events).await?;
    let AppEvent::UsageResetQuotaLoaded {
        thread_id,
        turn_id,
        account_id,
        completed_at,
        hard_stop_generation,
        response,
    } = ready
    else {
        panic!("expected fresh reset quota response");
    };
    for _ in 0..2 {
        app.handle_event(
            &mut tui,
            &mut server,
            AppEvent::UsageResetQuotaLoaded {
                thread_id,
                turn_id: turn_id.clone(),
                account_id: account_id.clone(),
                completed_at,
                hard_stop_generation,
                response: response.clone(),
            },
        )
        .await?;
    }
    let submissions = std::iter::from_fn(|| ops.try_recv().ok())
        .filter_map(|op| match op {
            Op::UserTurn { items, .. } => Some(items),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut transcript = String::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            transcript.push_str(&lines_to_single_string(&cell.display_lines(/*width*/ 80)));
        }
    }
    assert_eq!(
        submissions,
        vec![vec![UserInput::Text {
            text: "continue".into(),
            text_elements: vec![]
        }]]
    );
    insta::assert_snapshot!(transcript.trim(), @"› continue");
    server.shutdown().await?;
    Ok(())
}

async fn next_usage_event(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> Result<AppEvent> {
    Ok(
        tokio::time::timeout(std::time::Duration::from_secs(/*secs*/ 10), async {
            loop {
                let event = events.recv().await.expect("app event channel");
                if matches!(
                    event,
                    AppEvent::RateLimitsLoaded { .. } | AppEvent::UsageResetQuotaLoaded { .. }
                ) {
                    break event;
                }
            }
        })
        .await?,
    )
}
