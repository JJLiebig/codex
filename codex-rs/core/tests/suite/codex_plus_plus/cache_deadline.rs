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

#[test_case(3300, 3300, 114_000, None; "default waits for active tool")]
#[test_case(1800, 2400, 114_000, None; "custom interval stays fresh until known expiry")]
#[test_case(3300, 3300, 0, None; "small context continues without compaction")]
#[test_case(3300, 3300, 114_000, Some("usage_limit_reached"); "failure stops foreground and preserves history")]
#[tokio::test]
async fn cache_deadline_waits_for_active_tool_then_compacts_at_sampling_boundary(
    interval: u64,
    elapsed: u64,
    context_tokens: i64,
    failure_code: Option<&str>,
) -> Result<()> {
    use codex_protocol::config_types::CollaborationMode;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::config_types::Settings;
    use codex_protocol::request_user_input::RequestUserInputAnswer;
    use codex_protocol::request_user_input::RequestUserInputResponse;
    let harness = harness(interval).await?;
    let arguments = json!({"questions": [{"id": "choice", "header": "Choose", "question": "Continue?",
        "options": [{"label": "Yes", "description": "Continue"}, {"label": "No", "description": "Stop"}]}]}).to_string();
    let mut replies = vec![
        responses::sse(vec![
            responses::ev_function_call("question", "request_user_input", &arguments),
            responses::ev_completed_with_tokens("first", context_tokens),
        ]),
        failure_code.map_or_else(compacted, |code| {
            responses::sse_failed("compact", code, "cannot compact")
        }),
        reply("after-tool"),
    ];
    if context_tokens == 0 {
        replies.remove(1);
    }
    if failure_code.is_some() {
        replies.push(compacted());
    }
    let mock = responses::mount_sse_sequence(harness.server(), replies).await;
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
                        model: "gpt-6-astra".to_string(),
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
    advance(elapsed).await;
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
    let mut errors = 0;
    wait_for_event(&harness.test().codex, |event| {
        errors += usize::from(matches!(event, EventMsg::Error(_)));
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(errors, usize::from(failure_code.is_some()));
    if context_tokens == 0 {
        assert_eq!(mock.requests().len(), 2);
        assert!(
            mock.requests()[1]
                .inputs_of_type("compaction_trigger")
                .is_empty()
        );
        harness.test().codex.shutdown_and_wait().await?;
        return Ok(());
    }
    if failure_code.is_some() {
        assert_eq!(
            mock.requests().len(),
            2,
            "terminal compaction failure must stop inference"
        );
        advance(60).await;
        assert_eq!(
            mock.requests().len(),
            2,
            "failed work must not rearm maintenance"
        );
        prompt(&harness, "Resume after recovery").await?;
        assert!(
            mock.requests()[2].body_json()["input"]
                .to_string()
                .contains("Ask before continuing")
        );
        advance(3300).await;
        wait_for_event(&harness.test().codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
        assert_eq!(mock.requests().len(), 4);
        harness.test().codex.shutdown_and_wait().await?;
        return Ok(());
    }
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

fn builder(seconds: Option<u64>) -> TestCodexBuilder {
    let mut extensions = ExtensionRegistryBuilder::new();
    extensions.thread_lifecycle_contributor(Arc::new(ThreadIdle));
    test_codex()
        .with_extensions(Arc::new(extensions.build()))
        .with_model("gpt-6-astra")
        .with_model_info_override("gpt-6-astra", |model| {
            model.context_window = Some(400_000);
            model.effective_context_window_percent = 95;
        })
        .with_session_source(SessionSource::Cli)
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_config(move |config| {
            config.model_provider.stream_max_retries = Some(0);
            config.model_provider.request_max_retries = Some(0);
            config.config_layer_stack = config
                .config_layer_stack
                .with_user_config(
                    &config.codex_home.join("config.toml"),
                    toml::from_str(&seconds.map_or_else(String::new, |seconds| {
                        format!("[cache_deadline_compact_seconds]\n'gpt-6-astra' = {seconds}")
                    }))
                    .unwrap(),
                )
                .unwrap();
        })
}

async fn harness(seconds: u64) -> Result<TestCodexHarness> {
    TestCodexHarness::with_auto_env_builder(builder(Some(seconds))).await
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
        responses::ev_completed_with_tokens(id, 114_000),
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

#[test_case(None; "default thirty percent")]
#[test_case(Some(50); "configured fifty percent")]
#[tokio::test]
async fn cache_deadline_skips_small_current_context_but_manual_compact_still_runs(
    minimum: Option<u8>,
) -> Result<()> {
    let harness =
        TestCodexHarness::with_auto_env_builder(builder(None).with_config(move |config| {
            if let Some(percent) = minimum {
                config.config_layer_stack = config
                    .config_layer_stack
                    .with_user_config(
                        &config.codex_home.join("config.toml"),
                        toml::from_str(&format!(
                            "cache_deadline_compact_min_context_percent = {percent}"
                        ))
                        .unwrap(),
                    )
                    .unwrap();
            }
        }))
        .await?;
    let floor = i64::from(minimum.unwrap_or(30)) * 3800;
    let small_reply = |id| {
        responses::sse(vec![
            responses::ev_assistant_message(id, "reply"),
            responses::ev_completed_with_tokens(id, floor - 1),
        ])
    };
    let mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            small_reply("first"),
            small_reply("second"),
            compacted(),
            responses::sse(vec![responses::ev_completed_with_tokens("large", floor)]),
            compacted(),
        ],
    )
    .await;
    prompt(&harness, "retain small conversation").await?;
    advance(3300).await;
    prompt(&harness, "still below the minimum").await?;
    assert_eq!(mock.requests().len(), 2);
    assert!(
        mock.requests()[1].body_json()["input"]
            .to_string()
            .contains("retain small conversation")
    );
    advance(3300).await;
    // Cumulative usage exceeds the floor, but the current context remains one token below it.
    harness.test().codex.submit(Op::Compact).await?;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    ThreadIdle::wait(&harness.test().codex).await;
    assert_eq!(mock.requests().len(), 3);
    let metadata: serde_json::Value =
        serde_json::from_str(&mock.requests()[2].header("x-codex-turn-metadata").unwrap())?;
    assert_eq!(metadata["compaction"]["trigger"], "manual");
    prompt(&harness, "now exactly at the minimum").await?;
    advance(3300).await;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(mock.requests().len(), 5);
    harness.test().codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case("gpt-6-astra", false; "successful checkpoint is reused")]
#[test_case("gpt-6-astra", true; "failed compaction preserves history")]
#[test_case("gpt-6.1-sol", false; "sol defaults to timer compaction")]
#[test_case("gpt-7.2-mini", false; "future gpt defaults to timer compaction")]
#[tokio::test]
async fn cache_deadline_compacts_once_and_foreground_rearms(model: &str, fail: bool) -> Result<()> {
    let harness = TestCodexHarness::with_auto_env_builder(
        builder(None)
            .with_model(model)
            .with_model_info_override(model, |info| {
                info.context_window = Some(400_000);
                info.effective_context_window_percent = 95;
            }),
    )
    .await?;
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
    let mut errors = 0;
    let mut maintenance_id = None;
    wait_for_event(&harness.test().codex, |event| {
        errors += usize::from(matches!(event, EventMsg::Error(_)));
        if let EventMsg::TurnStarted(turn) = event {
            maintenance_id = Some(turn.turn_id.clone());
        }
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(errors, usize::from(fail), "native failure is reported once");
    assert!(maintenance_id.unwrap().starts_with("auto-compact-"));
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

#[test_case("off"; "explicit zero stays off")]
#[test_case("overdue"; "resume after expiry drops stale deadline")]
#[test_case("model"; "model changes disarm deadline")]
#[test_case("too_late"; "known expired interval stays off")]
#[tokio::test]
async fn cache_deadline_does_not_compact_stale_or_disabled_context(case: &str) -> Result<()> {
    let harness = harness(match case {
        "off" => 0,
        "too_late" => 3600,
        _ => 3300,
    })
    .await?;
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
    advance(if matches!(case, "overdue" | "too_late") {
        3601
    } else {
        3300
    })
    .await;
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
async fn cache_deadline_approval_cancel_disarms_until_foreground_inference() -> Result<()> {
    let harness =
        TestCodexHarness::with_auto_env_builder(builder(Some(3300)).with_config(|config| {
            config.permissions.approval_policy = codex_config::Constrained::allow_any(
                codex_protocol::protocol::AskForApproval::OnRequest,
            );
            config
                .set_legacy_sandbox_policy(
                    codex_protocol::protocol::SandboxPolicy::new_workspace_write_policy(),
                )
                .unwrap();
        }))
        .await?;
    let mock = responses::mount_sse_sequence(harness.server(), vec![
        responses::sse(vec![responses::ev_function_call("approval", "exec_command", &json!({
            "cmd": "echo approval", "sandbox_permissions": "require_escalated", "justification": "Confirm the test command"
        }).to_string()), responses::ev_completed("tool")]),
        reply("resumed"), compacted(),
    ]).await;
    harness
        .test()
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Ask before running the command".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let EventMsg::ExecApprovalRequest(approval) = wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::ExecApprovalRequest(_))
    })
    .await
    else {
        unreachable!()
    };
    advance(3300).await;
    assert_eq!(mock.requests().len(), 1);
    harness
        .test()
        .codex
        .submit(Op::ExecApproval {
            id: approval.effective_approval_id(),
            turn_id: Some(approval.turn_id),
            decision: codex_protocol::protocol::ReviewDecision::Abort,
        })
        .await?;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    advance(60).await;
    assert_eq!(
        mock.requests().len(),
        1,
        "Cancel must not start optional maintenance"
    );
    prompt(&harness, "A new foreground request").await?;
    advance(3300).await;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(mock.requests().len(), 3);
    harness.test().codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn cache_deadline_does_not_arm_from_hidden_title_inference() -> Result<()> {
    let harness = harness(3300).await?;
    let mut options = harness.test().start_thread_options().await;
    options.config.ephemeral = true;
    options.thread_source = Some(codex_protocol::protocol::ThreadSource::Feature(
        "thread_title".into(),
    ));
    let title = harness
        .test()
        .thread_manager
        .start_thread(options)
        .await?
        .thread;
    let mock =
        responses::mount_sse_sequence(harness.server(), vec![reply("title"), reply("next")]).await;
    for message in ["Generate a title", "Another title"] {
        title
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: message.into(),
                text_elements: Vec::new(),
            }]))
            .await?;
        wait_for_event(&title, |event| matches!(event, EventMsg::TurnComplete(_))).await;
        ThreadIdle::wait(&title).await;
        advance(3300).await;
    }
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        let metadata: serde_json::Value =
            serde_json::from_str(&request.header("x-codex-turn-metadata").unwrap())?;
        assert_eq!(metadata["thread_source"], "thread_title");
        assert_eq!(
            request.inputs_of_type("compaction_trigger"),
            Vec::<serde_json::Value>::new()
        );
    }
    title.shutdown_and_wait().await?;
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
            if request
                .headers
                .get("x-codex-turn-metadata")
                .is_some_and(|value| {
                    value
                        .to_str()
                        .is_ok_and(|metadata| metadata.contains("cache_deadline"))
                })
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
    let requests = responses::received_responses_requests(harness.server()).await;
    let last = requests.last().unwrap().body_json();
    assert!(last["input"].to_string().contains("preserve this message"));
    assert!(last["input"].to_string().contains("foreground wins"));
    assert_eq!(mock.requests().len(), 1);
    harness.test().codex.shutdown_and_wait().await?;
    Ok(())
}

#[test_case("usage_limit_reached"; "weekly reset wait is preserved")]
#[test_case("misalignment_policy_violation"; "precaution acknowledgement is preserved")]
#[tokio::test]
async fn cache_deadline_terminal_error_preserves_recovery_until_foreground_resumes(
    code: &str,
) -> Result<()> {
    let harness = harness(3300).await?;
    let mock = responses::mount_sse_sequence(
        harness.server(),
        vec![
            reply("first"),
            responses::sse_failed("failed", code, "foreground needs recovery"),
            reply("resumed"),
            compacted(),
        ],
    )
    .await;
    prompt(&harness, "retain original history").await?;
    prompt(&harness, "failed foreground turn").await?;
    advance(3300).await;
    assert_eq!(
        mock.requests().len(),
        2,
        "maintenance must not supersede pending recovery"
    );
    prompt(&harness, "acknowledged foreground resume").await?;
    assert!(
        mock.requests()[2].body_json()["input"]
            .to_string()
            .contains("retain original history")
    );
    advance(3300).await;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    assert_eq!(
        mock.requests().len(),
        4,
        "successful foreground activity rearms maintenance"
    );
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
    let reopened = builder(Some(3300))
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
