use super::*;
use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::turn::TurnRunState;
use crate::session::turn::run_turn;
use crate::session::turn_context::TurnContext;
use crate::tasks::CompactTask;
use crate::tasks::SessionTask;
use codex_history::PreviousTurnSettings;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const SUMMARY: &str = "Preserved conversation summary";
const PENDING: &str = "Continue after compaction";
const OPAQUE: &str = "OpenAI-only encrypted checkpoint";

fn source_checkpoint() -> ResponseItem {
    serde_json::from_value(json!({"type":"compaction","encrypted_content":OPAQUE})).unwrap()
}

async fn owned_session(fixture: &OwnedFixture, model: &str) -> (Arc<Session>, Arc<TurnContext>) {
    let (mut session, mut turn, _) =
        crate::session::tests::make_session_and_context_with_auth_and_config_and_rx(
            CodexAuth::from_api_key("synthetic"),
            Vec::new(),
            |config| {
                config.model = Some(model.into());
                config.model_post_turn_compact_threshold_percent = 0;
            },
        )
        .await;
    let provider = create_model_provider(
        ModelProviderInfo::create_cli_proxy_provider(),
        Some(fixture.manager.clone()),
    );
    let mut client = test_model_client(SessionSource::Cli);
    Arc::get_mut(&mut client.state).unwrap().provider = provider.clone();
    let services = &mut Arc::get_mut(&mut session).unwrap().services;
    services.model_client = client;
    // Previous-model reconstruction must stay entirely inside the synthetic catalogue.
    services.models_manager = Arc::new(codex_models_manager::manager::StaticModelsManager::new(
        /*auth_manager*/ None,
        ModelsResponse {
            models: ["future-9.7", "claude-new"]
                .map(|slug| {
                    let mut info = test_model_info();
                    info.slug = slug.into();
                    info.comp_hash = Some(slug.into());
                    info
                })
                .into(),
        },
    ));
    let context = Arc::get_mut(&mut turn).unwrap();
    context.provider = provider;
    context.auth_manager = Some(fixture.manager.clone());
    crate::session::tests::update_turn_settings_for_test(context, |settings| {
        Arc::make_mut(&mut settings.model_info).comp_hash = Some(model.into());
    });
    session
        .record_conversation_items(
            &turn,
            turn.model_info(),
            &[
                responses::user_message_item("Earlier request"),
                serde_json::from_value(
                    responses::ev_assistant_message("old", "Earlier answer")["item"].clone(),
                )
                .unwrap(),
            ],
        )
        .await;
    (session, turn)
}

fn compact_response(model: &str) -> String {
    let output = if model == "claude-new" {
        responses::ev_assistant_message("summary", SUMMARY)
    } else {
        json!({"type":"response.output_item.done","item":{
            "type":"compaction","encrypted_content":SUMMARY
        }})
    };
    responses::sse(vec![
        output,
        responses::ev_completed_with_tokens("compact", 10),
    ])
}

async fn inference_requests(fixture: &OwnedFixture) -> Vec<Value> {
    fixture
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|request| request.url.path() == "/v1/responses")
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect()
}

fn assert_compact_request(request: &Value, model: &str) {
    let wire = request["model"].as_str().unwrap();
    let input = request["input"].as_array().unwrap();
    assert!(!request.to_string().contains(PENDING));
    if model == "claude-new" {
        assert_eq!(wire, model);
        assert!(
            input
                .iter()
                .all(|item| item["type"] != "compaction_trigger")
        );
        let last = input.last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(
            last["content"][0]["text"],
            crate::compact::SUMMARIZATION_PROMPT
        );
    } else {
        assert!(wire.ends_with("/future-9.7"), "{wire}");
        assert_eq!(input.last().unwrap()["type"], "compaction_trigger");
    }
}

fn pending_input() -> Vec<TurnInput> {
    vec![TurnInput::UserInput {
        metadata: Default::default(),
        content: vec![UserInput::Text {
            text: PENDING.into(),
            text_elements: Vec::new(),
        }],
        client_id: None,
    }]
}

#[tokio::test]
async fn owned_manual_compaction_uses_exact_model_support() -> anyhow::Result<()> {
    for model in ["claude-new", "future-9.7"] {
        let fixture = OwnedFixture::new().await?;
        let (session, turn) = owned_session(&fixture, model).await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(compact_response(model)))
            .expect(1)
            .mount(&fixture.server)
            .await;
        Arc::new(CompactTask)
            .run(session.clone(), turn, Vec::new(), CancellationToken::new())
            .await?;
        let requests = inference_requests(&fixture).await;
        assert_eq!(requests.len(), 1);
        assert_compact_request(&requests[0], model);
        let history: Vec<_> = session.clone_history().await.raw_items().cloned().collect();
        assert!(serde_json::to_string(&history)?.contains(SUMMARY));
    }
    Ok(())
}

#[tokio::test]
async fn owned_switch_and_threshold_compaction_preserve_pending_input() -> anyhow::Result<()> {
    // Different hashes trigger previous-model compaction; the same model uses its token limit.
    for (previous, current, hash) in [
        ("claude-new", "future-9.7", Some("claude-new")),
        ("future-9.7", "claude-new", Some("future-9.7")),
        ("future-9.7", "claude-new", None),
        ("claude-new", "claude-new", Some("claude-new")),
    ] {
        let fixture = OwnedFixture::new().await?;
        let (session, mut turn) = owned_session(&fixture, current).await;
        session
            .set_previous_turn_settings(Some(PreviousTurnSettings {
                model: previous.into(),
                comp_hash: hash.map(str::to_owned),
                cyber_access_program: None,
                realtime_active: Some(false),
            }))
            .await;
        let handoff = previous == "future-9.7" && current == "claude-new";
        if handoff {
            session
                .record_conversation_items(&turn, turn.model_info(), &[source_checkpoint()])
                .await;
            if hash.is_none() {
                crate::session::tests::update_turn_settings_for_test(
                    Arc::get_mut(&mut turn).unwrap(),
                    |settings| Arc::make_mut(&mut settings.model_info).comp_hash = None,
                );
            }
        }
        if previous == current {
            crate::session::tests::update_turn_settings_for_test(
                Arc::get_mut(&mut turn).unwrap(),
                |settings| {
                    Arc::make_mut(&mut settings.model_info).auto_compact_token_limit = Some(10_000);
                },
            );
            session
                .update_token_usage_info(
                    &turn,
                    Some(&TokenUsage {
                        input_tokens: 12_000,
                        total_tokens: 12_000,
                        ..Default::default()
                    }),
                )
                .await?;
        }
        let replies = Mutex::new(VecDeque::from([
            compact_response(if handoff { "claude-new" } else { previous }),
            responses::sse(vec![
                responses::ev_assistant_message("reply", "Acknowledged"),
                responses::ev_completed_with_tokens("done", 10),
            ]),
        ]));
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(move |_: &wiremock::Request| {
                ResponseTemplate::new(200)
                    .set_body_string(replies.lock().unwrap().pop_front().expect("no replay"))
            })
            .expect(2)
            .mount(&fixture.server)
            .await;
        run_turn(
            session.clone(),
            turn,
            pending_input(),
            &mut TurnRunState::default(),
            CancellationToken::new(),
        )
        .await?;
        let requests = inference_requests(&fixture).await;
        assert_eq!(requests.len(), 2, "{previous} -> {current}");
        if handoff {
            let request = &requests[0];
            assert!(request["model"].as_str().unwrap().ends_with("/future-9.7"));
            assert!(request.to_string().contains(OPAQUE));
            assert!(!request.to_string().contains(PENDING));
            assert_eq!(
                request["input"].as_array().unwrap().last().unwrap()["content"][0]["text"],
                crate::compact::SUMMARIZATION_PROMPT
            );
            assert!(!request.to_string().contains("compaction_trigger"));
        } else {
            assert_compact_request(&requests[0], previous);
        }
        assert_eq!(
            requests[1]["model"].as_str().unwrap().rsplit('/').next(),
            Some(current)
        );
        let followup = requests[1].to_string();
        assert_eq!(followup.matches(SUMMARY).count(), 1);
        assert_eq!(followup.matches(PENDING).count(), 1);
        if handoff {
            assert!(!followup.contains(OPAQUE));
            assert!(!followup.contains("Earlier request"));
            assert!(!followup.contains("Earlier answer"));
            assert!(followup.contains("environment_context"));
        }
        assert!(
            requests[1]["input"]
                .as_array()
                .unwrap()
                .iter()
                .all(|item| item["type"] != "compaction_trigger")
        );
        let history: Vec<_> = session.clone_history().await.raw_items().cloned().collect();
        let history = serde_json::to_string(&history)?;
        assert!(
            history.contains(SUMMARY)
                && history.contains(PENDING)
                && history.contains("Acknowledged")
        );
    }
    Ok(())
}

#[tokio::test]
async fn owned_readable_handoff_failure_preserves_source_and_pending_input() -> anyhow::Result<()> {
    let summary = responses::ev_assistant_message("uncommitted", "Uncommitted summary");
    let failed = responses::sse_failed("failed", "server_error", "Handoff failed");
    for body in [
        format!("{}{}", responses::sse(vec![summary.clone()]), failed),
        responses::sse(vec![summary]), // Output followed by EOF must also stay buffered.
        responses::sse(vec![responses::ev_completed("empty")]),
        responses::sse(vec![
            responses::ev_assistant_message("blank", "  "),
            responses::ev_completed("blank"),
        ]),
        responses::sse(vec![
            responses::ev_assistant_message("oversize", &"Uncommitted summary ".repeat(10_000)),
            responses::ev_completed("oversize"),
        ]),
        responses::sse_failed("limit", "context_length_exceeded", "Source context is full"),
    ] {
        let fixture = OwnedFixture::new().await?;
        let (session, turn) = owned_session(&fixture, "claude-new").await;
        session
            .record_conversation_items(&turn, turn.model_info(), &[source_checkpoint()])
            .await;
        let before: Vec<_> = session.clone_history().await.raw_items().cloned().collect();
        session
            .set_previous_turn_settings(Some(PreviousTurnSettings {
                model: "future-9.7".into(),
                comp_hash: None,
                cyber_access_program: None,
                realtime_active: Some(false),
            }))
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .expect(1)
            .mount(&fixture.server)
            .await;
        run_turn(
            session.clone(),
            turn.clone(),
            pending_input(),
            &mut TurnRunState::default(),
            CancellationToken::new(),
        )
        .await?;
        let requests = inference_requests(&fixture).await;
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]["model"]
                .as_str()
                .unwrap()
                .ends_with("/future-9.7")
        );
        assert!(requests[0].to_string().contains(OPAQUE));
        assert!(turn.terminal_error.lock().await.is_some());
        let after: Vec<_> = session.clone_history().await.raw_items().cloned().collect();
        assert_eq!(&after[..before.len()], before.as_slice());
        let history = serde_json::to_string(&after)?;
        assert_eq!(history.matches(PENDING).count(), 1);
        assert!(!history.contains("Uncommitted summary"));
    }
    Ok(())
}

#[tokio::test]
async fn owned_compaction_preparation_failure_preserves_input_without_request() -> anyhow::Result<()>
{
    let fixture = OwnedFixture::new().await?;
    let (session, turn) = owned_session(&fixture, "future-9.7").await;
    fixture.files.lock().unwrap().remove("claude.json");
    session
        .set_previous_turn_settings(Some(PreviousTurnSettings {
            model: "claude-new".into(),
            comp_hash: Some("claude-new".into()),
            cyber_access_program: None,
            realtime_active: Some(false),
        }))
        .await;
    run_turn(
        session.clone(),
        turn.clone(),
        pending_input(),
        &mut TurnRunState::default(),
        CancellationToken::new(),
    )
    .await?;
    assert!(inference_requests(&fixture).await.is_empty());
    let error = turn
        .terminal_error
        .lock()
        .await
        .clone()
        .expect("preparation failure");
    assert!(error.message.contains("unavailable"));
    assert!(!error.message.contains("remote compact"));
    let history: Vec<_> = session.clone_history().await.raw_items().cloned().collect();
    let history = serde_json::to_string(&history)?;
    assert!(history.contains("Earlier answer") && history.contains(PENDING));
    Ok(())
}

#[tokio::test]
async fn owned_claude_local_compaction_never_replays_accepted_output() -> anyhow::Result<()> {
    let fixture = OwnedFixture::new().await?;
    let (session, turn) = owned_session(&fixture, "claude-new").await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_string(PARTIAL))
        .expect(1)
        .mount(&fixture.server)
        .await;
    Arc::new(CompactTask)
        .run(session, turn.clone(), Vec::new(), CancellationToken::new())
        .await?;
    let requests = inference_requests(&fixture).await;
    assert_eq!(requests.len(), 1);
    assert_compact_request(&requests[0], "claude-new");
    let error = turn
        .terminal_error
        .lock()
        .await
        .clone()
        .expect("terminal EOF");
    assert_eq!(
        error.inference_attribution,
        Some(InferenceAttribution::Claude)
    );
    Ok(())
}
