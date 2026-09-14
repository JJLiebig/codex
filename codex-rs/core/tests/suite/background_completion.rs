use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_protocol::AgentPath;
use codex_protocol::protocol::AgentStatus;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InterAgentCommunication;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::sse_failed;
use core_test_support::test_codex::TestCodexHarness;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use test_case::test_case;

#[derive(Clone, Copy)]
enum Finish {
    Wake,
    DeferredMail,
    TriggeredMail,
    Compact,
    Steer,
    Read,
    Cancel,
    Ordinary,
    Exec,
    Subagent,
}

#[test_case(Finish::Wake; "idle completion resumes once")]
#[test_case(Finish::DeferredMail; "deferred mail keeps completion wait active")]
#[test_case(Finish::TriggeredMail; "triggering mail interrupts completion wait")]
#[test_case(Finish::Compact; "completion wakes after compact replacement")]
#[test_case(Finish::Steer; "completion wakes after ordinary steering")]
#[test_case(Finish::Read; "manual observation suppresses duplicate wake")]
#[test_case(Finish::Cancel; "interrupt disarms idle wake")]
#[test_case(Finish::Subagent; "subagents do not report completion while waiting")]
#[test_case(Finish::Exec; "one turn host does not offer wakes")]
#[test_case(Finish::Ordinary; "ordinary background commands do not wake")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_completion(finish: Finish) -> Result<()> {
    let source = if matches!(finish, Finish::Exec) {
        codex_protocol::protocol::SessionSource::Exec
    } else if matches!(finish, Finish::Subagent) {
        codex_protocol::protocol::SessionSource::SubAgent(
            codex_protocol::protocol::SubAgentSource::Other("worker".into()),
        )
    } else {
        codex_protocol::protocol::SessionSource::Cli
    };
    let harness =
        TestCodexHarness::with_auto_env_builder(test_codex().with_session_source(source)).await?;
    let command = match core_test_support::test_target_os() {
        core_test_support::TestTargetOs::Windows => {
            "while (!(Test-Path release)) { Start-Sleep -Milliseconds 20 }; Write-Output completed; exit 7"
        }
        core_test_support::TestTargetOs::Linux | core_test_support::TestTargetOs::MacOs => {
            "while [ ! -f release ]; do sleep 0.02; done; echo completed; exit 7"
        }
    };
    let mut args = json!({"cmd": command, "yield_time_ms": 250});
    if !matches!(finish, Finish::Ordinary) {
        args["on_exit"] = json!("wake");
    }
    let mut responses = vec![
        sse(vec![
            ev_function_call("background", "exec_command", &args.to_string()),
            ev_completed("r1"),
        ]),
        sse(vec![
            ev_assistant_message("m1", "Waiting for completion."),
            ev_completed("r2"),
        ]),
    ];
    if matches!(finish, Finish::Read) {
        responses.push(sse(vec![
            ev_function_call(
                "observe",
                "write_stdin",
                &json!({"session_id":1000,"chars":"","yield_time_ms":30000}).to_string(),
            ),
            ev_completed("r3"),
        ]));
    }
    if matches!(finish, Finish::TriggeredMail) {
        responses.push(sse(vec![
            ev_assistant_message("m2", "Mail handled."),
            ev_completed("r3"),
        ]));
    }
    if matches!(finish, Finish::Compact) {
        responses.push(sse(vec![
            ev_assistant_message("summary", "Compacted."),
            ev_completed("r3"),
        ]));
    }
    if matches!(finish, Finish::Steer) {
        responses.push(sse(vec![
            ev_assistant_message("m2", "Steer handled."),
            ev_completed("r3"),
        ]));
    }
    if !matches!(finish, Finish::Ordinary | Finish::Exec | Finish::Subagent) {
        responses.push(sse(vec![
            ev_assistant_message("m2", "Done."),
            ev_completed("r4"),
        ]));
    }
    let mock = mount_sse_sequence(harness.server(), responses).await;
    let waits_for_completion = matches!(
        finish,
        Finish::Wake
            | Finish::DeferredMail
            | Finish::TriggeredMail
            | Finish::Compact
            | Finish::Steer
            | Finish::Read
            | Finish::Cancel
    );
    let mut submit =
        waits_for_completion.then(|| Box::pin(harness.submit("Run the background command.")));
    if let Some(submit) = submit.as_mut() {
        let waiting = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while mock.requests().len() < 2 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        tokio::pin!(waiting);
        tokio::select! {
            result = submit.as_mut() => panic!("turn completed before background exit: {result:?}"),
            result = &mut waiting => result.expect("background command did not reach waiting state"),
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(200), submit.as_mut())
                .await
                .is_err(),
            "turn completed before background exit"
        );
    } else {
        harness.submit("Run the background command.").await?;
    }

    if matches!(finish, Finish::DeferredMail | Finish::TriggeredMail) {
        harness
            .test()
            .codex
            .submit(Op::InterAgentCommunication {
                communication: InterAgentCommunication::new(
                    AgentPath::try_from("/root/worker").expect("worker path should parse"),
                    AgentPath::root(),
                    Vec::new(),
                    "late queue-only update".to_string(),
                    /*trigger_turn*/ matches!(finish, Finish::TriggeredMail),
                ),
                start_options: Default::default(),
            })
            .await?;
        if matches!(finish, Finish::TriggeredMail) {
            tokio::time::timeout(std::time::Duration::from_secs(20), async {
                while mock.requests().len() < 3 {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("triggering mail did not start its turn");
            submit.as_mut().expect("active submission").await?;
        } else {
            harness
                .test()
                .codex
                .submit(Op::RealtimeConversationListVoices)
                .await?;
            wait_for_event(&harness.test().codex, |event| {
                matches!(event, EventMsg::RealtimeConversationListVoicesResponse(_))
            })
            .await;
        }
        assert_eq!(
            harness.test().codex.agent_status().await,
            AgentStatus::Running
        );
    }
    assert_eq!(
        mock.requests().len(),
        if matches!(finish, Finish::TriggeredMail) {
            3
        } else {
            2
        }
    );
    assert_eq!(
        mock.requests()[0].body_json()["tools"]
            .to_string()
            .contains("on_exit"),
        !matches!(finish, Finish::Exec | Finish::Subagent)
    );
    let output = mock.requests()[1]
        .function_call_output_text("background")
        .expect("background tool result");
    assert!(output.contains("Process running with session ID 1000"));
    if waits_for_completion {
        assert_eq!(
            harness.test().codex.agent_status().await,
            AgentStatus::Running
        );
    }

    if matches!(finish, Finish::Compact) {
        harness.test().codex.submit(Op::Compact).await?;
        wait_for_event(&harness.test().codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
        submit.take();
    }

    if matches!(finish, Finish::Cancel) {
        harness.test().codex.submit(Op::Interrupt).await?;
        wait_for_event(&harness.test().codex, |e| {
            matches!(e, EventMsg::TurnAborted(_))
        })
        .await;
        harness.submit("Acknowledge cancellation.").await?;
    }
    if matches!(finish, Finish::Read) {
        harness
            .test()
            .codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Read the command result now.".into(),
                text_elements: Vec::new(),
            }]))
            .await?;
    }
    if matches!(finish, Finish::Steer) {
        harness
            .test()
            .codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: "Acknowledge without reading the process.".into(),
                text_elements: Vec::new(),
            }]))
            .await?;
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while mock.requests().len() < 3 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("steered input did not sample");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(200),
                submit.as_mut().expect("active submission")
            )
            .await
            .is_err(),
            "turn completed before background exit"
        );
    }
    harness.write_file("release", b"go").await?;
    if matches!(finish, Finish::TriggeredMail) {
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            while mock.requests().len() < 4 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("background completion did not resume the triggered-mail turn");
    } else if matches!(
        finish,
        Finish::Wake | Finish::DeferredMail | Finish::Steer | Finish::Read
    ) {
        submit.as_mut().expect("active submission").await?;
    } else if matches!(finish, Finish::Compact) {
        wait_for_event(&harness.test().codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await;
    } else {
        wait_for_event(&harness.test().codex, |e| {
            matches!(e, EventMsg::ExecCommandEnd(_))
        })
        .await;
    }
    // A second continuation is an observable regression, even with no matching mock response.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        match finish {
            Finish::Wake | Finish::DeferredMail | Finish::Cancel => 3,
            Finish::TriggeredMail | Finish::Compact | Finish::Steer | Finish::Read => 4,
            Finish::Ordinary | Finish::Exec | Finish::Subagent => 2,
        }
    );
    if matches!(
        finish,
        Finish::Wake
            | Finish::DeferredMail
            | Finish::TriggeredMail
            | Finish::Compact
            | Finish::Steer
    ) {
        let request = if matches!(
            finish,
            Finish::TriggeredMail | Finish::Compact | Finish::Steer
        ) {
            &requests[3]
        } else {
            &requests[2]
        };
        let input = request.body_json()["input"].to_string();
        assert!(input.contains("<background_completion>"));
        assert!(input.contains("exit_code=7"));
    }
    if matches!(finish, Finish::Read) {
        assert!(
            !requests[3].body_json()["input"]
                .to_string()
                .contains("<background_completion>")
        );
    }
    if matches!(finish, Finish::DeferredMail | Finish::TriggeredMail) {
        assert!(
            requests[2].body_json()["input"]
                .to_string()
                .contains("late queue-only update")
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_turn_disarms_background_completion_wake() -> Result<()> {
    let harness = TestCodexHarness::with_auto_env_builder(test_codex().with_config(|config| {
        config.model_provider.request_max_retries = Some(0);
        config.model_provider.stream_max_retries = Some(0);
    }))
    .await?;
    let command = match core_test_support::test_target_os() {
        core_test_support::TestTargetOs::Windows => {
            "while (!(Test-Path release)) { Start-Sleep -Milliseconds 20 }; exit 0"
        }
        core_test_support::TestTargetOs::Linux | core_test_support::TestTargetOs::MacOs => {
            "while [ ! -f release ]; do sleep 0.02; done; exit 0"
        }
    };
    let mock = mount_sse_sequence(
        harness.server(),
        vec![
            sse(vec![
                ev_function_call(
                    "background",
                    "exec_command",
                    &json!({"cmd":command,"yield_time_ms":250,"on_exit":"wake"}).to_string(),
                ),
                ev_completed("r1"),
            ]),
            sse_failed("r2", "invalid_request_error", "request failed"),
        ],
    )
    .await;

    harness.submit("Run the background command.").await?;
    harness.write_file("release", b"go").await?;
    wait_for_event(&harness.test().codex, |event| {
        matches!(event, EventMsg::ExecCommandEnd(_))
    })
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn background_completion_batches_exits_before_final_answer() -> Result<()> {
    let harness = TestCodexHarness::with_auto_env_builder(
        test_codex().with_session_source(codex_protocol::protocol::SessionSource::Cli),
    )
    .await?;
    let (wait, release) = match core_test_support::test_target_os() {
        core_test_support::TestTargetOs::Windows => (
            "while (!(Test-Path release)) { Start-Sleep -Milliseconds 20 }; exit 0",
            "Set-Content release go; Start-Sleep -Seconds 1",
        ),
        core_test_support::TestTargetOs::Linux | core_test_support::TestTargetOs::MacOs => (
            "while [ ! -f release ]; do sleep 0.02; done; exit 0",
            "touch release; sleep 1",
        ),
    };
    let args = json!({"cmd":wait,"yield_time_ms":250,"on_exit":"wake"}).to_string();
    let mock = mount_sse_sequence(
        harness.server(),
        vec![
            sse(vec![
                ev_function_call("a", "exec_command", &args),
                ev_function_call("b", "exec_command", &args),
                ev_completed("r1"),
            ]),
            sse(vec![
                ev_function_call(
                    "release",
                    "exec_command",
                    &json!({"cmd":release,"yield_time_ms":10000}).to_string(),
                ),
                ev_completed("r2"),
            ]),
            sse(vec![
                ev_assistant_message("m1", "Waiting."),
                ev_completed("r3"),
            ]),
            sse(vec![
                ev_assistant_message("m2", "Both finished."),
                ev_completed("r4"),
            ]),
        ],
    )
    .await;
    harness.submit("Run both commands.").await?;
    assert_eq!(mock.requests().len(), 4);
    let input = mock.requests()[3].body_json()["input"].to_string();
    assert_eq!(input.matches("<background_completion>").count(), 1);
    assert!(input.contains("session_id=1000"));
    assert!(input.contains("session_id=1001"));
    Ok(())
}

#[test_case(None; "quiet progress is the default")]
#[test_case(Some(false); "quiet progress can be disabled")]
#[tokio::test]
async fn background_completion_progress_instructions(setting: Option<bool>) -> Result<()> {
    let original = "Keep useful guidance. Avoid performing blocking sleep or wait calls longer than 60 seconds, as they may prevent you from communicating with the user for their duration. Keep custom suffix.\nThe user appreciates consistent, frequent communication during your turn, and should not be left without a commentary update for more than 60 seconds during ongoing work. Keep trailing guidance.";
    let harness =
        TestCodexHarness::with_auto_env_builder(test_codex().with_config(move |config| {
            config.base_instructions = Some(original.into());
            let path = config.codex_home.join("config.toml");
            config.config_layer_stack = config
                .config_layer_stack
                .with_user_config(&path, toml::Value::Table(Default::default()))
                .expect("quiet-update test configuration");
            if let Some(enabled) = setting {
                let path = config.codex_home.join("config.toml");
                config.config_layer_stack = config
                    .config_layer_stack
                    .with_user_config(
                        &path,
                        toml::from_str(&format!("disable_unnecessary_updates = {enabled}"))
                            .expect("quiet-update TOML"),
                    )
                    .expect("quiet-update test configuration");
            }
        }))
        .await?;
    let mock = core_test_support::responses::mount_sse_once(
        harness.server(),
        sse(vec![ev_assistant_message("m", "Done."), ev_completed("r")]),
    )
    .await;
    harness.submit("Say done.").await?;
    let request = mock.single_request().body_json();
    let instructions = request["instructions"].as_str().expect("instructions");
    if setting == Some(false) {
        assert_eq!(instructions, original);
    } else {
        assert!(instructions.contains("Keep useful guidance."));
        assert!(instructions.contains("Keep custom suffix."));
        assert!(instructions.contains("Keep trailing guidance."));
        assert!(!instructions.contains("60 seconds"));
        assert!(
            instructions
                .contains("Do not send updates or check status merely because time passed.")
        );
    }
    Ok(())
}
