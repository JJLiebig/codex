use super::*;
use codex_login::NativeCredentialSource;
use codex_login::auth::ImportedAccountSwitchOutcome;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::inference_attribution::InferenceAttribution;
use codex_protocol::inference_attribution::InferenceNativeSource;
use pretty_assertions::assert_eq;
use std::collections::HashSet;

#[path = "proxy_request_fixture.rs"]
mod fixture;
use fixture::OwnedFixture;

#[path = "proxy_auth_recovery_tests.rs"]
mod auth_recovery;

const NATIVE_TRACE: &str = "20260930010000-0000000000000001-aabbccdd";
const CLAUDE_TRACE: &str = "20260930010000-0000000000000002-aabbccdd";
const COMPLETED: &str =
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"done\",\"output\":[]}}\n\n";
const QUOTA: &str = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"type\":\"usage_limit_reached\"}}}\n\n";
const PARTIAL: &str = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"accepted\"}\n\n";

#[tokio::test]
async fn owned_compaction_failures_preserve_source_in_terminal_event() -> anyhow::Result<()> {
    for (remote, model, body, trace) in [
        (false, "future-9.7", PARTIAL, NATIVE_TRACE),
        (true, "future-9.7", PARTIAL, NATIVE_TRACE),
        (false, "claude-new", QUOTA, CLAUDE_TRACE),
        (true, "claude-new", QUOTA, CLAUDE_TRACE),
    ] {
        let fixture = OwnedFixture::new().await?;
        let source = fixture
            .manager
            .export_native_credentials()
            .await?
            .selected_source()
            .cloned();
        let Some(NativeCredentialSource::Root(id)) = source else {
            panic!("synthetic root account");
        };
        let expected = Some(if model == "claude-new" {
            InferenceAttribution::Claude
        } else {
            InferenceAttribution::ServedNative {
                source: InferenceNativeSource::Root,
                account_id: id.to_string(),
            }
        });
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-cpa-trace-id", trace)
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&fixture.server)
            .await;
        let (mut session, mut turn, events) =
            crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
                CodexAuth::from_api_key("synthetic"),
                Vec::new(),
                |config| config.model = Some(model.into()),
            )
            .await;
        let provider = create_model_provider(
            ModelProviderInfo::create_cli_proxy_provider(),
            Some(fixture.manager.clone()),
        );
        let mut client = test_model_client(SessionSource::Cli);
        Arc::get_mut(&mut client.state).unwrap().provider = provider.clone();
        Arc::get_mut(&mut session).unwrap().services.model_client = client;
        let context = Arc::get_mut(&mut turn).unwrap();
        context.provider = provider;
        context.auth_manager = Some(fixture.manager.clone());
        let result = if remote {
            crate::compact_remote_v2::run_remote_compact_task(session, turn.clone()).await
        } else {
            crate::compact::run_compact_task(session, turn.clone(), Vec::new()).await
        };
        assert_eq!(
            result.unwrap_err().inference_attribution().cloned(),
            expected
        );
        let terminal = turn
            .terminal_error
            .lock()
            .await
            .clone()
            .expect("terminal failure");
        assert_eq!(terminal.inference_attribution, expected);
        assert!(terminal.affects_turn_status());
        let emitted = std::iter::from_fn(|| events.try_recv().ok())
            .find_map(|event| {
                if let codex_protocol::protocol::EventMsg::Error(error) = event.msg {
                    Some(error)
                } else {
                    None
                }
            })
            .expect("canonical error event");
        assert_eq!(emitted, terminal);
    }
    Ok(())
}

#[tokio::test]
async fn owned_http_uses_public_preparation_and_frozen_trace_membership() -> anyhow::Result<()> {
    let fixture = OwnedFixture::new().await?;
    let mut client = test_model_client(SessionSource::Cli);
    Arc::get_mut(&mut client.state).unwrap().provider = create_model_provider(
        ModelProviderInfo::create_cli_proxy_provider(),
        Some(fixture.manager.clone()),
    );
    let source = fixture
        .manager
        .export_native_credentials()
        .await?
        .selected_source()
        .cloned();
    let Some(NativeCredentialSource::Root(id)) = &source else {
        panic!("synthetic root account");
    };
    let metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        "stable-session".into(),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    let oversized_trace = "x".repeat(129);
    for (model, trace) in [
        ("future-9.7", Some(NATIVE_TRACE)),
        ("claude-new", Some(CLAUDE_TRACE)),
        ("future-9.7", Some(CLAUDE_TRACE)),
        ("future-9.7", Some("malformed")),
        ("future-9.7", Some(oversized_trace.as_str())),
        ("future-9.7", None),
    ] {
        let mut response = ResponseTemplate::new(200).set_body_string(COMPLETED);
        if let Some(trace) = trace {
            response = response.insert_header("x-cpa-trace-id", trace);
        }
        let _mock = Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(response)
            .expect(1)
            .mount_as_scoped(&fixture.server)
            .await;
        let mut model_info = test_model_info();
        model_info.slug = model.into();
        let canonical = model_info.clone();
        let mut session = client.new_session();
        let mut stream = session
            .stream_responses_api(
                &Prompt::default(),
                &model_info,
                &test_session_telemetry(),
                /*effort*/ None,
                ReasoningSummaryConfig::None,
                /*service_tier*/ None,
                &metadata,
                &InferenceTraceContext::disabled(),
            )
            .await?;
        // The request must retain the inventory it used even if management changes afterward.
        fixture
            .files
            .lock()
            .unwrap()
            .retain(|name, _| name == "claude.json");
        while let Some(event) = stream.next().await {
            event?;
        }
        let terminal = session
            .attribute_owned_error(CodexErr::Stream("stream closed".into()))
            .to_error_event(Some("Error running remote compact task".into()));
        assert_eq!(
            terminal.inference_attribution,
            Some(if trace == Some(NATIVE_TRACE) {
                InferenceAttribution::ServedNative {
                    source: InferenceNativeSource::Root,
                    account_id: id.to_string(),
                }
            } else if model == "claude-new" {
                InferenceAttribution::Claude
            } else {
                InferenceAttribution::Unknown
            })
        );
        let captured = session.owned_request.as_ref().unwrap();
        assert_eq!(
            captured.response_trace.get(),
            Some(&if trace == Some(oversized_trace.as_str()) {
                None
            } else {
                trace.map(str::to_owned)
            })
        );
        assert_eq!(
            captured.served_native_source.get(),
            Some(&if trace == Some(NATIVE_TRACE) {
                source.clone()
            } else {
                None
            })
        );
        let requests = fixture.server.received_requests().await.unwrap();
        let request = requests
            .iter()
            .rev()
            .find(|request| request.url.path() == "/v1/responses")
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&request.body)?;
        let expected_model = if model == "future-9.7" {
            format!("codex-native-root-{id}/future-9.7")
        } else {
            model.into()
        };
        assert_eq!(
            (&body["model"], &body["prompt_cache_key"]),
            (
                &json!(expected_model),
                &json!(client.prompt_cache_key(&metadata))
            )
        );
        assert_eq!(
            request.headers.get("session-id").unwrap().to_str()?,
            client.responses_session_id(&metadata)
        );
        assert_eq!(
            request.headers.get("authorization").unwrap().to_str()?,
            format!("Bearer {}", "a".repeat(64))
        );
        assert_eq!(model_info, canonical);
    }
    Ok(())
}

#[tokio::test]
async fn owned_http_terminal_errors_and_partial_output_cannot_recover_or_replay()
-> anyhow::Result<()> {
    let fixture = OwnedFixture::new().await?;
    let mut client = test_model_client(SessionSource::Cli);
    Arc::get_mut(&mut client.state).unwrap().provider = create_model_provider(
        ModelProviderInfo::create_cli_proxy_provider(),
        Some(fixture.manager.clone()),
    );
    let metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        "stable-session".into(),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    let mut messages = Vec::new();
    for model in ["future-9.7", "claude-new"] {
        for case in [
            "http_quota",
            "local_401",
            "cooldown",
            "stream_quota",
            "partial_quota",
            "tool_partial",
            "truncated",
            "pre_output_500",
            "pre_output_overload",
        ] {
            let response = match case {
                "http_quota" => ResponseTemplate::new(429).set_body_json(json!({"error":{"type":"usage_limit_reached"}})),
                "local_401" => ResponseTemplate::new(401),
                "cooldown" => ResponseTemplate::new(429).set_body_json(json!({"error":{"code":"model_cooldown","model":model,"last_upstream_error":"usage_limit_reached"}})),
                "stream_quota" => ResponseTemplate::new(200).set_body_string(QUOTA),
                "partial_quota" => ResponseTemplate::new(200).set_body_string(format!("{PARTIAL}{QUOTA}")),
                "tool_partial" => ResponseTemplate::new(200).set_body_string(format!("data: {{\"type\":\"response.output_item.done\",\"item\":{{\"type\":\"function_call\",\"name\":\"test\",\"call_id\":\"call\",\"arguments\":\"{{}}\"}}}}\n\n{QUOTA}")),
                "truncated" => ResponseTemplate::new(200).set_body_string(PARTIAL),
                "pre_output_500" => ResponseTemplate::new(500),
                "pre_output_overload" => ResponseTemplate::new(200).set_body_string("data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_is_overloaded\"}}}\n\n"),
                _ => unreachable!(),
            };
            let response = if matches!(case, "local_401" | "cooldown") {
                response
            } else {
                response.insert_header(
                    "x-cpa-trace-id",
                    if model == "future-9.7" {
                        NATIVE_TRACE
                    } else {
                        CLAUDE_TRACE
                    },
                )
            };
            let _mock = Mock::given(method("POST"))
                .and(path("/v1/responses"))
                .respond_with(response)
                .expect(1)
                .mount_as_scoped(&fixture.server)
                .await;
            let mut model_info = test_model_info();
            model_info.slug = model.into();
            let mut session = client.new_session();
            let revision = *fixture.manager.auth_change_receiver().borrow();
            let result = session
                .stream_responses_api(
                    &Prompt::default(),
                    &model_info,
                    &test_session_telemetry(),
                    /*effort*/ None,
                    ReasoningSummaryConfig::None,
                    /*service_tier*/ None,
                    &metadata,
                    &InferenceTraceContext::disabled(),
                )
                .await;
            let mut error = None;
            match result {
                Ok(mut stream) => {
                    while let Some(event) = stream.next().await {
                        if let Err(err) = event {
                            error = Some(err);
                        }
                    }
                }
                Err(err) => error = Some(err),
            }
            let native_quota =
                model == "future-9.7" && matches!(case, "http_quota" | "stream_quota");
            if let Some(error) = &error {
                let expected = if model == "claude-new" {
                    InferenceAttribution::Claude
                } else if matches!(case, "local_401" | "cooldown") {
                    InferenceAttribution::Unknown
                } else {
                    let NativeCredentialSource::Root(id) = fixture
                        .manager
                        .export_native_credentials()
                        .await?
                        .selected_source()
                        .unwrap()
                        .clone()
                    else {
                        panic!("root source")
                    };
                    InferenceAttribution::ServedNative {
                        source: InferenceNativeSource::Root,
                        account_id: id.to_string(),
                    }
                };
                assert_eq!(
                    error
                        .to_error_event(/*message_prefix*/ None)
                        .inference_attribution,
                    Some(expected),
                    "{case}"
                );
            }
            if native_quota {
                let CodexErrorDetails::UsageLimitReached(usage) = error.as_ref().unwrap().details()
                else {
                    panic!("bound native quota");
                };
                assert_eq!(
                    session
                        .switch_owned_quota(&mut HashSet::new(), usage)
                        .await?,
                    Some(ImportedAccountSwitchOutcome::NoCandidate)
                );
            }
            if !native_quota
                && !matches!(case, "truncated" | "pre_output_500" | "pre_output_overload")
            {
                let error = error.as_ref().expect("provider rejection must surface");
                assert!(
                    matches!(error.details(), CodexErrorDetails::UnsupportedOperation(_)),
                    "{model}/{case}: {error}"
                );
                assert!(session.owned_retry_forbidden(error));
                if model == "claude-new"
                    && matches!(case, "http_quota" | "local_401" | "partial_quota")
                {
                    messages.push(error.to_string());
                }
            }
            if matches!(case, "pre_output_500" | "pre_output_overload") {
                assert!(!session.owned_retry_forbidden(error.as_ref().expect("HTTP failure")));
            }
            if case == "pre_output_overload" {
                assert!(
                    crate::codex_plus_plus::model_capacity_retry::applies_to_sampling(
                        error.as_ref().unwrap(),
                        &SessionSource::Cli,
                    )
                );
            }
            if matches!(case, "partial_quota" | "tool_partial" | "truncated") {
                assert!(session.owned_retry_forbidden(&CodexErr::Stream("stream ended".into())));
            }
            let captured = session.owned_request.as_ref().unwrap();
            assert_eq!(
                captured.response_trace.get(),
                Some(&if matches!(case, "local_401" | "cooldown") {
                    None
                } else {
                    Some(
                        if model == "future-9.7" {
                            NATIVE_TRACE
                        } else {
                            CLAUDE_TRACE
                        }
                        .into(),
                    )
                })
            );
            if model == "claude-new" || matches!(case, "local_401" | "cooldown") {
                assert_eq!(captured.served_native_source.get(), Some(&None));
            }
            assert_eq!(*fixture.manager.auth_change_receiver().borrow(), revision);
            assert_eq!(fixture.manager.active_account_id(), None);
            assert!(
                AccountStore::new(fixture.home.path().to_path_buf())
                    .list()?
                    .is_empty()
            );
        }
    }
    insta::assert_snapshot!(messages.join("\n"), @r###"
    unsupported operation: The model provider reached its usage limit.
    unsupported operation: The model provider rejected authentication.
    unsupported operation: Model response interrupted after output: The model provider reached its usage limit.
    "###);
    Ok(())
}

#[tokio::test]
async fn owned_quota_switches_only_the_bound_native_source() -> anyhow::Result<()> {
    use super::super::proxy_request::NativeQuotaAttribution;
    for case in [
        "http",
        "stream",
        "cooldown",
        "cooldown_pinned",
        "missing",
        "mismatch",
        "wrong_model",
        "claude",
        "401",
        "stale",
        "partial",
    ] {
        let fixture = OwnedFixture::new().await?;
        let ids = fixture.import_pair().await?;
        let store = AccountStore::new(fixture.home.path().to_path_buf());
        if case == "cooldown_pinned" {
            store.set_automation_enabled(&ids[1], /*automation_enabled*/ false)?;
        }
        let mut client = test_model_client(SessionSource::Cli);
        Arc::get_mut(&mut client.state).unwrap().provider = create_model_provider(
            ModelProviderInfo::create_cli_proxy_provider(),
            Some(fixture.manager.clone()),
        );
        let metadata = test_responses_metadata_for_client(
            &client,
            /*turn_id*/ None,
            "stable-session".into(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        );
        let a_model = format!("codex-native-imported-{}/future-9.7", ids[0]);
        let b_model = format!("codex-native-imported-{}/future-9.7", ids[1]);
        let resets_at = chrono::Utc::now().timestamp() + 3600;
        let auth_home = store
            .enabled_file_accounts()?
            .into_iter()
            .find(|(id, _)| id == &ids[0])
            .unwrap()
            .1;
        let expected_a = a_model.clone();
        let switches = matches!(case, "http" | "stream" | "cooldown");
        let _mock = Mock::given(method("POST")).and(path("/v1/responses"))
            .respond_with(move |request: &wiremock::Request| {
                let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
                if body["model"] != expected_a && case != "claude" {
                    return ResponseTemplate::new(200).insert_header("x-cpa-trace-id", "20260930010000-0000000000000003-aabbccdd").set_body_string(COMPLETED);
                }
                if case == "stale" {
                    let mut auth: codex_login::auth::AuthDotJson = serde_json::from_slice(&std::fs::read(auth_home.join("auth.json")).unwrap()).unwrap();
                    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json!({"exp":chrono::Utc::now().timestamp()+7200}).to_string());
                    auth.tokens.as_mut().unwrap().access_token = format!("e30.{token}.sig");
                    codex_login::save_auth(&auth_home, &auth, AuthCredentialsStoreMode::File, AuthKeyringBackendKind::default()).unwrap();
                }
                let response = match case {
                    "cooldown" | "cooldown_pinned" | "wrong_model" => ResponseTemplate::new(429).set_body_json(json!({"error":{
                        "code":"model_cooldown", "model": if case == "wrong_model" { "other/model" } else { expected_a.as_str() },
                        "reset_seconds":3600, "last_upstream_error":{"type":"usage_limit_reached"}
                    }})),
                    "401" => ResponseTemplate::new(401),
                    "stream" => ResponseTemplate::new(200).set_body_string(QUOTA),
                    "partial" => ResponseTemplate::new(200).set_body_string(format!("{PARTIAL}{QUOTA}")),
                    _ => ResponseTemplate::new(429).set_body_json(json!({"error":{"type":"usage_limit_reached","resets_at":resets_at}})),
                };
                if matches!(case, "cooldown" | "cooldown_pinned" | "wrong_model" | "missing") { response } else {
                    response.insert_header("x-cpa-trace-id", if matches!(case, "mismatch" | "claude") { CLAUDE_TRACE } else { NATIVE_TRACE })
                }
            }).expect(if switches { 2 } else { 1 }).mount_as_scoped(&fixture.server).await;
        let mut model_info = test_model_info();
        model_info.slug = if case == "claude" {
            "claude-new"
        } else {
            "future-9.7"
        }
        .into();
        let mut session = client.new_session();
        let mut attempted = HashSet::new();
        let mut error = None;
        loop {
            match session
                .stream_responses_api(
                    &Prompt::default(),
                    &model_info,
                    &test_session_telemetry(),
                    /*effort*/ None,
                    ReasoningSummaryConfig::None,
                    /*service_tier*/ None,
                    &metadata,
                    &InferenceTraceContext::disabled(),
                )
                .await
            {
                Ok(mut stream) => {
                    while let Some(event) = stream.next().await {
                        if let Err(err) = event {
                            error = Some(err);
                        }
                    }
                }
                Err(err) => error = Some(err),
            }
            if case == "stream"
                && let Some(err) = error.take()
            {
                let CodexErrorDetails::UsageLimitReached(usage) = err.details() else {
                    panic!("native stream quota: {err}");
                };
                assert_eq!(
                    session.owned_quota_attribution(),
                    Some(NativeQuotaAttribution::Served)
                );
                assert_eq!(
                    session.switch_owned_quota(&mut attempted, usage).await?,
                    Some(ImportedAccountSwitchOutcome::ReadyToRetry)
                );
                assert_eq!(fixture.manager.active_account_id(), Some(ids[1].clone()));
                // The failed request stays bound to A after selection has moved to B.
                let replacement =
                    CodexErr::UnsupportedOperation("manual selection guidance".into())
                        .with_inference_attribution_from(&err);
                assert_eq!(
                    replacement
                        .to_error_event(/*message_prefix*/ None)
                        .inference_attribution,
                    Some(InferenceAttribution::ServedNative {
                        source: InferenceNativeSource::Imported,
                        account_id: ids[0].to_string(),
                    })
                );
                continue;
            }
            break;
        }
        if case == "cooldown_pinned" {
            assert_eq!(
                error
                    .as_ref()
                    .unwrap()
                    .to_error_event(/*message_prefix*/ None)
                    .inference_attribution,
                Some(InferenceAttribution::IntendedNative {
                    source: InferenceNativeSource::Imported,
                    account_id: ids[0].to_string(),
                })
            );
            assert!(matches!(
                error.as_ref().unwrap().details(),
                CodexErrorDetails::UsageLimitReached(_)
            ));
            let request = session.owned_request.as_ref().unwrap();
            assert_eq!(
                session.owned_quota_attribution(),
                Some(NativeQuotaAttribution::Intended)
            );
            assert_eq!(request.served_native_source.get(), Some(&None));
            assert_eq!(
                session
                    .take_usage_limit_failover_tracking()
                    .attempted_account_ids,
                HashSet::from([ids[0].to_string()])
            );
        } else if !switches {
            assert!(
                matches!(
                    error.as_ref().unwrap().details(),
                    CodexErrorDetails::UnsupportedOperation(_)
                ),
                "{case}"
            );
        } else {
            assert!(error.is_none());
        }
        if case == "stale" {
            insta::assert_snapshot!(error.as_ref().unwrap().to_string(), @"unsupported operation: The account changed while the request was running. Try again.");
        }
        assert_eq!(
            fixture.manager.active_account_id(),
            Some(ids[usize::from(switches)].clone()),
            "{case}"
        );
        let charged = matches!(case, "http" | "cooldown" | "cooldown_pinned");
        let profiles = store.list()?;
        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.usage_limit_resets_at.is_some())
                .collect::<Vec<_>>(),
            vec![charged, false],
            "{case}"
        );
        let requests: Vec<_> = fixture
            .server
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|request| request.url.path() == "/v1/responses")
            .collect();
        if switches {
            let mut bodies: Vec<serde_json::Value> = requests
                .iter()
                .map(|request| serde_json::from_slice(&request.body).unwrap())
                .collect();
            assert_eq!(
                (&bodies[0]["model"], &bodies[1]["model"]),
                (&json!(a_model), &json!(b_model))
            );
            bodies[0]["model"] = bodies[1]["model"].clone();
            assert_eq!(bodies[0], bodies[1]);
            assert_eq!(
                requests[0].headers.get("session-id"),
                requests[1].headers.get("session-id")
            );
            let tracking = session.take_usage_limit_failover_tracking();
            assert_eq!(
                (
                    tracking.attempted_account_ids,
                    tracking.selected_account_ids
                ),
                (HashSet::from([ids[0].to_string()]), vec![ids[1].clone()])
            );
        }
    }
    Ok(())
}
