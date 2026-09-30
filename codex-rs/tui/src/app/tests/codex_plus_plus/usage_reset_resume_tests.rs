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
        ChatGptAuthFixture::new("e30.eyJleHAiOjQxMDI0NDQ4MDB9.c2ln")
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
            completion: None,
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
    // The same live wait consumes an owned admission only in its current hard-stop generation.
    let host_completed_at = chrono::Utc::now().timestamp() + 60;
    for (method, status, error) in [
        ("turn/started", "inProgress", serde_json::Value::Null),
        (
            "turn/completed",
            "failed",
            json!({"message":"Usage exhausted", "codexErrorInfo":"usageLimitExceeded",
            "inferenceAttribution":{"type":"servedNative", "source":"root", "accountId":"acct_f2b6477631260f18", "displayLabel":null}}),
        ),
    ] {
        app.chat_widget.handle_server_notification(
            serde_json::from_value(json!({
                "method": method, "params":{"threadId":thread_id.to_string(),
                    "turn":{"id":"owned-failure","items":[],"itemsView":"full","status":status,
                        "completedAt":host_completed_at,"error":error}}
            }))?,
            /*replay_kind*/ None,
        );
    }
    let target = app
        .chat_widget
        .owned_reset_target(/*completion*/ None)
        .unwrap();
    // A per-thread owned provider still polls when global native display polling is disabled.
    app.chat_widget.requires_openai_auth = false;
    let usage_count = |requests: Vec<wiremock::Request>| {
        requests
            .iter()
            .filter(|request| request.url.path() == "/api/codex/usage")
            .count()
    };
    for confirmed in [false, true] {
        if confirmed {
            let mut lease = codex_login::AccountStore::new(home.path().into())
                .acquire_reset_mutation_lease(&account_id)?;
            lease.begin_manual(
                "owned-completion",
                codex_login::ResetCredentialSource::Root,
                /*credit_id*/ None,
            )?;
            lease.confirm_manual("owned-completion", target.failed_at * 1_000_000_000)?;
            let completion = lease.state()?.completion.unwrap();
            lease.reconcile_proxy(&completion, codex_login::ResetReconciliation::ObservedClear)?;
        }
        assert!(app.rate_limit_poll_deadline().is_some());
        let before = usage_count(backend.received_requests().await.unwrap());
        app.refresh_rate_limits(&server, RateLimitRefreshOrigin::Periodic);
        app.refresh_rate_limits(&server, RateLimitRefreshOrigin::Periodic);
        assert!(app.rate_limit_poll_deadline().is_none());
        let loaded = next_usage_event(&mut events).await?;
        assert!(
            matches!(&loaded, AppEvent::UsageResetAdmissionLoaded { response, .. }
            if response.is_some() == confirmed && response.as_ref().is_none_or(|response| response.reset_admission.is_none()))
        );
        app.handle_event(&mut tui, &mut server, loaded).await?;
        assert!(app.rate_limit_poll_deadline().is_some());
        assert_eq!(
            usage_count(backend.received_requests().await.unwrap()),
            before + usize::from(confirmed)
        );
    }
    let mut owned = response.clone();
    owned.ordinary_usage_allowed = Some(true);
    owned.reset_admission = Some(codex_app_server_protocol::UsageResetCompletion {
        id: "owned-completion".into(),
        source: target.source,
        account_id: target.account_id.clone(),
        completed_at: target.failed_at,
        completed_at_ns: (target.failed_at * 1_000_000_000).to_string(),
    });
    // Both completion producers request exact admission now, without waiting for a poll.
    let completion = owned.reset_admission.clone().unwrap();
    for trigger in ["automatic", "manual", "reopened"] {
        let event = if trigger != "automatic" {
            let request_id = app.chat_widget.show_rate_limit_reset_consuming_popup();
            AppEvent::RateLimitResetCreditConsumed {
                request_id,
                idempotency_key: completion.id.clone(),
                credit_id: None,
                result: if trigger == "reopened" {
                    Err("Previous reset attempt resolved. No new reset was used.".into())
                } else {
                    Ok(codex_app_server_protocol::ConsumeAccountRateLimitResetCreditResponse {
                        outcome: codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome::Reset,
                        reset_completion: Some(completion.clone()),
                    })
                },
            }
        } else {
            AppEvent::UsageResetCompleted {
                account_id: account_id.clone(),
                completed_at: completion.completed_at_ns.parse()?,
                completion: Some(completion.clone()),
            }
        };
        app.handle_event(&mut tui, &mut server, event).await?;
        loop {
            let loaded = next_usage_event(&mut events).await?;
            if let AppEvent::UsageResetAdmissionLoaded {
                target: immediate,
                periodic_request_id,
                ..
            } = &loaded
            {
                let mut expected = target.clone();
                expected.completion_id = (trigger != "reopened").then(|| completion.id.clone());
                assert_eq!((immediate, periodic_request_id), (&expected, &None));
                app.handle_event(&mut tui, &mut server, loaded).await?;
                break;
            }
            app.handle_event(&mut tui, &mut server, loaded).await?;
        }
        assert!(
            !std::iter::from_fn(|| ops.try_recv().ok()).any(|op| matches!(op, Op::UserTurn { .. }))
        );
    }
    assert!(
        !backend
            .received_requests()
            .await
            .unwrap()
            .iter()
            .any(|request| request.method == "POST"
                && request.url.path() == "/api/codex/rate-limit-reset-credits/consume")
    );
    let hard_stop_generation = app.rate_limit_hard_stop_generation;
    for (generation, expected_count) in [
        (hard_stop_generation.wrapping_add(1), 0),
        (hard_stop_generation, 1),
        (hard_stop_generation, 0),
    ] {
        app.handle_event(
            &mut tui,
            &mut server,
            AppEvent::UsageResetAdmissionLoaded {
                target: target.clone(),
                periodic_request_id: None,
                hard_stop_generation: generation,
                response: Some(owned.clone()),
            },
        )
        .await?;
        let count = std::iter::from_fn(|| ops.try_recv().ok())
            .filter(|op| matches!(op, Op::UserTurn { .. }))
            .count();
        assert_eq!(count, expected_count);
    }
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
                    AppEvent::RateLimitsLoaded { .. }
                        | AppEvent::UsageResetQuotaLoaded { .. }
                        | AppEvent::UsageResetAdmissionLoaded { .. }
                ) {
                    break event;
                }
            }
        })
        .await?,
    )
}
