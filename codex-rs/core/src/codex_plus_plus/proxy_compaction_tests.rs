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

const SUMMARY: &str = "Preserved conversation summary; tool fact 41";
const PENDING: &str = "Continue after compaction";
const TOOL_CALL: &str = "completed-tool-call";

async fn owned_session(fixture: &OwnedFixture, model: &str) -> (Arc<Session>, Arc<TurnContext>) {
    // Advertise a second native model for switch controls.
    let files = fixture.files.clone();
    Mock::given(method("GET"))
        .and(path("/v0/management/auth-files/models"))
        .respond_with(move |request: &wiremock::Request| {
            let files = files.lock().unwrap();
            let name = request.url.query_pairs().find(|(key, _)| key == "name").unwrap().1;
            let models = files.get(name.as_ref()).map_or_else(Vec::new, |record| {
                match record["prefix"].as_str() {
                    Some(prefix) => ["future-9.7", "future-9.8"].map(|model| json!({"id":format!("{prefix}/{model}"),"type":"openai","owned_by":"openai"})).to_vec(),
                    None => Vec::new(),
                }
            });
            ResponseTemplate::new(200).set_body_json(json!({"models":models}))
        })
        .with_priority(/*p*/ 1)
        .mount(&fixture.server)
        .await;
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
            models: ["future-9.7", "future-9.8"]
                .map(|slug| {
                    let mut info = test_model_info();
                    info.slug = slug.into();
                    info.comp_hash = Some(slug.into());
                    info
                })
                .into(),
        },
        codex_models_manager::ModelOrigin::ExplicitConfig,
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
                    responses::ev_function_call(TOOL_CALL, "lookup", "{}")["item"].clone(),
                )
                .unwrap(),
                serde_json::from_value(json!({
                    "type":"function_call_output","call_id":TOOL_CALL,"output":"tool fact 41"
                }))
                .unwrap(),
                serde_json::from_value(
                    responses::ev_assistant_message("old", "Earlier answer")["item"].clone(),
                )
                .unwrap(),
            ],
        )
        .await;
    (session, turn)
}

fn compact_response() -> String {
    let output = json!({"type":"response.output_item.done","item":{
        "type":"compaction","encrypted_content":SUMMARY
    }});
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
    assert!(wire.ends_with(&format!("/{model}")), "{wire}");
    assert_eq!(input.last().unwrap()["type"], "compaction_trigger");
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
    for model in ["future-9.7"] {
        let fixture = OwnedFixture::new().await?;
        let (session, turn) = owned_session(&fixture, model).await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(ResponseTemplate::new(200).set_body_string(compact_response()))
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
        ("future-9.7", "future-9.8", Some("future-9.7")),
        ("future-9.7", "future-9.7", Some("old-hash")),
        ("future-9.7", "future-9.7", Some("future-9.7")),
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
        if previous == current && hash == Some(current) {
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
                    /*includes_reasoning*/ false,
                )
                .await?;
        }
        let replies = Mutex::new(VecDeque::from([
            compact_response(),
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
        assert_compact_request(&requests[0], previous);
        assert_eq!(
            requests[1]["model"].as_str().unwrap().rsplit('/').next(),
            Some(current)
        );
        let followup = requests[1].to_string();
        assert_eq!(
            followup.matches(SUMMARY).count(),
            1,
            "{previous} -> {current}; hash={hash:?}"
        );
        assert_eq!(
            followup.matches(PENDING).count(),
            1,
            "{previous} -> {current}; hash={hash:?}"
        );
        assert!(followup.contains("Earlier request"));
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
async fn owned_switch_without_compaction_keeps_history() -> anyhow::Result<()> {
    for (previous, current) in [("future-9.7", "future-9.8"), ("future-9.7", "future-9.7")] {
        let fixture = OwnedFixture::new().await?;
        let (session, turn) = owned_session(&fixture, current).await;
        session
            .set_previous_turn_settings(Some(PreviousTurnSettings {
                model: previous.into(),
                comp_hash: Some(current.into()),
                cyber_access_program: None,
                realtime_active: Some(false),
            }))
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(responses::sse(vec![
                    responses::ev_assistant_message("reply", "Acknowledged"),
                    responses::ev_completed_with_tokens("done", 10),
                ])),
            )
            .expect(1)
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
        assert_eq!(requests.len(), 1, "{previous} -> {current}");
        let input = requests[0]["input"].to_string();
        assert!(!input.contains("compaction_trigger"));
        assert!(!input.contains(crate::compact::SUMMARIZATION_PROMPT));
        assert!(input.contains("Earlier request") && input.contains("Earlier answer"));
        assert_eq!(input.matches(TOOL_CALL).count(), 2);
        assert_eq!(input.matches(PENDING).count(), 1);
    }
    Ok(())
}
