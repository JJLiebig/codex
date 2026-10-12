use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_extension_api::ExtensionRegistryBuilder;
use codex_login::CodexAuth;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::ThreadIdle;
use core_test_support::responses;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::TestCodexHarness;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use test_case::test_case;

#[tokio::test]
async fn cache_deadline_waits_for_active_tool_then_compacts_at_sampling_boundary() -> Result<()> {
    use codex_protocol::config_types::CollaborationMode;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::config_types::Settings;
    use codex_protocol::request_user_input::RequestUserInputAnswer;
    use codex_protocol::request_user_input::RequestUserInputResponse;
    let harness = harness(3300).await?;
    let arguments = json!({"questions": [{"id": "choice", "header": "Choose", "question": "Continue?",
        "options": [{"label": "Yes", "description": "Continue"}, {"label": "No", "description": "Stop"}]}]}).to_string();
    let mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            responses::sse(vec![
                responses::ev_function_call("question", "request_user_input", &arguments),
                responses::ev_completed("first"),
            ]),
            compacted(),
            reply("after-tool"),
        ],
    )
    .await;
    harness
        .test()
        .codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Ask before continuing".to_string(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Plan,
                    settings: Settings {
                        model: "gpt-5.4".to_string(),
                        reasoning_effort: None,
                        developer_instructions: None,
                    },
                }),
                ..Default::default()
            }),
        )
        .await?;
    let EventMsg::RequestUserInput(request) = wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::RequestUserInput(_))
    })
    .await
    else {
        unreachable!()
    };
    advance(3300).await;
    assert_eq!(
        mock.requests().len(),
        1,
        "deadline must not interrupt a tool awaiting its user"
    );
    harness
        .test()
        .codex
        .submit(Op::UserInputAnswer {
            id: request.turn_id,
            response: RequestUserInputResponse {
                answers: [(
                    "choice".into(),
                    RequestUserInputAnswer {
                        answers: vec!["Yes".into()],
                    },
                )]
                .into(),
            },
        })
        .await?;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let metadata: serde_json::Value =
        serde_json::from_str(&requests[1].header("x-codex-turn-metadata").unwrap())?;
    assert_eq!(
        (
            metadata["compaction"]["reason"].as_str(),
            metadata["compaction"]["phase"].as_str()
        ),
        (Some("cache_deadline"), Some("mid_turn"))
    );
    assert!(
        requests[2].body_json()["input"]
            .to_string()
            .contains("CACHE_DEADLINE_SUMMARY")
    );
    harness.test().codex.shutdown_and_wait().await?;
    Ok(())
}

fn builder(seconds: u64) -> TestCodexBuilder {
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
    test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_model("gpt-5.4")
        .with_session_source(SessionSource::Cli)
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.request_max_retries = Some(0);
            config.config_layer_stack = config
                .config_layer_stack
                .with_user_config(
                    &config.codex_home.join("config.toml"),
                    toml::from_str(&if seconds == 0 {
                        String::new()
                    } else {
                        format!("[cache_deadline_compact_seconds]\n'gpt-5.4' = {seconds}")
                    })
                    .unwrap(),
                )
                .unwrap();
        })
}

async fn harness(seconds: u64) -> Result<TestCodexHarness> {
    TestCodexHarness::with_auto_env_builder(builder(seconds)).await
}

async fn prompt(harness: &TestCodexHarness, text: &str) -> Result<()> {
    harness
        .test()
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    ThreadIdle::wait(&harness.test().codex).await;
    Ok(())
}

async fn advance(seconds: u64) {
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(seconds)).await;
    tokio::time::resume();
    tokio::task::yield_now().await;
}

fn reply(id: &str) -> String {
    responses::sse(vec![
        responses::ev_assistant_message(id, "reply"),
        responses::ev_completed(id),
    ])
}

fn compacted() -> String {
    responses::sse(vec![
        json!({
            "type": "response.output_item.done",
            "item": {"type": "compaction", "encrypted_content": "CACHE_DEADLINE_SUMMARY"},
        }),
        responses::ev_completed("compact"),
    ])
}

#[test_case(false; "successful checkpoint is reused")]
#[test_case(true; "failed compaction preserves history")]
#[tokio::test]
async fn cache_deadline_compacts_once_and_foreground_rearms(fail: bool) -> Result<()> {
    let harness = harness(3300).await?;
    let mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            reply("first"),
            if fail {
                responses::sse_failed("compact", "invalid_request_error", "cannot compact")
            } else {
                compacted()
            },
            reply("next"),
            compacted(),
        ],
    )
    .await;
    prompt(&harness, "retained user message").await?;
    advance(54 * 60).await;
    assert_eq!(mock.requests().len(), 1);
    advance(60).await;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    ThreadIdle::wait(&harness.test().codex).await;
    let metadata: serde_json::Value =
        serde_json::from_str(&mock.requests()[1].header("x-codex-turn-metadata").unwrap())?;
    assert_eq!(
        metadata["compaction"],
        json!({
            "trigger": "auto", "reason": "cache_deadline", "implementation": "responses_compaction_v2",
            "phase": "standalone_turn", "strategy": "memento",
        })
    );
    advance(2 * 60 * 60).await;
    assert_eq!(
        mock.requests().len(),
        2,
        "compaction must not keep its own cache alive"
    );
    prompt(&harness, "next foreground message").await?;
    let next = mock.requests()[2].body_json()["input"].to_string();
    assert!(next.contains(if fail {
        "retained user message"
    } else {
        "CACHE_DEADLINE_SUMMARY"
    }));
    advance(3300).await;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(mock.requests().len(), 4);
    harness.test().codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case("off"; "missing deadline stays off")]
#[test_case("overdue"; "resume after expiry drops stale deadline")]
#[test_case("model"; "model changes disarm deadline")]
#[tokio::test]
async fn cache_deadline_does_not_compact_stale_or_disabled_context(case: &str) -> Result<()> {
    let harness = harness(if case == "off" { 0 } else { 3300 }).await?;
    let mock =
        responses::mount_sse_sequence(harness.server(), vec![reply("first"), reply("next")]).await;
    prompt(&harness, "retain original history").await?;
    if case == "model" {
        harness
            .test()
            .codex
            .update_thread_settings(ThreadSettingsOverrides {
                model: Some("gpt-5.4-mini".to_string()),
                ..Default::default()
            })
            .await?;
    }
    advance(if case == "overdue" { 3601 } else { 3300 }).await;
    prompt(&harness, "continue").await?;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        requests[1].body_json()["input"]
            .to_string()
            .contains("retain original history")
    );
    assert_eq!(
        requests[1].inputs_of_type("compaction_trigger"),
        Vec::<serde_json::Value>::new()
    );
    harness.test().codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn cache_deadline_foreground_cancels_idle_compaction_without_losing_history() -> Result<()> {
    let harness = harness(3300).await?;
    let requested = Arc::new(tokio::sync::Notify::new());
    let notify = Arc::clone(&requested);
    let mock = responses::mount_sse_once(harness.server(), reply("first")).await;
    prompt(&harness, "preserve this message").await?;
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/v1/responses"))
        .respond_with(move |request: &wiremock::Request| {
            if request.body_json::<serde_json::Value>().unwrap()["input"]
                .to_string()
                .contains("compaction_trigger")
            {
                notify.notify_one();
                responses::sse_response(compacted()).set_delay(Duration::from_secs(60))
            } else {
                responses::sse_response(reply("foreground"))
            }
        })
        .mount(harness.server())
        .await;
    advance(3300).await;
    tokio::time::timeout(Duration::from_secs(10), requested.notified()).await?;
    prompt(&harness, "foreground wins").await?;
    let requests = harness.server().received_requests().await.unwrap();
    let last = requests.last().unwrap().body_json::<serde_json::Value>()?;
    assert!(last["input"].to_string().contains("preserve this message"));
    assert!(last["input"].to_string().contains("foreground wins"));
    assert_eq!(mock.requests().len(), 1);
    harness.test().codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn cache_deadline_reopen_does_not_restore_an_old_timer() -> Result<()> {
    let harness = harness(3300).await?;
    let mock =
        responses::mount_sse_sequence(harness.server(), vec![reply("first"), reply("reopened")])
            .await;
    prompt(&harness, "history survives restart").await?;
    let reopened = builder(3300)
        .restart_with_auto_env(harness.server(), harness.test())
        .await?;
    advance(3300).await;
    reopened
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "continue after restart".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&reopened.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(mock.requests().len(), 2);
    assert!(
        mock.requests()[1].body_json()["input"]
            .to_string()
            .contains("history survives restart")
    );
    reopened.codex.shutdown_and_wait().await?;
    Ok(())
}
