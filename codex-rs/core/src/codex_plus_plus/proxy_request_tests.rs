use super::*;
use codex_login::NativeCredentialSource;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use pretty_assertions::assert_eq;

#[path = "proxy_request_fixture.rs"]
mod fixture;
use fixture::OwnedFixture;

const NATIVE_TRACE: &str = "20260930010000-0000000000000001-aabbccdd";
const CLAUDE_TRACE: &str = "20260930010000-0000000000000002-aabbccdd";
const COMPLETED: &str =
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"done\",\"output\":[]}}\n\n";
const QUOTA: &str = "data: {\"type\":\"response.failed\",\"response\":{\"error\":{\"type\":\"usage_limit_reached\"}}}\n\n";
const PARTIAL: &str = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"accepted\"}\n\n";

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
            if !matches!(case, "truncated" | "pre_output_500" | "pre_output_overload") {
                let error = error.as_ref().expect("provider rejection must surface");
                assert!(
                    matches!(error.details(), CodexErrorDetails::UnsupportedOperation(_)),
                    "{model}/{case}: {error}"
                );
                assert!(session.owned_retry_forbidden(error));
                if model == "future-9.7"
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
