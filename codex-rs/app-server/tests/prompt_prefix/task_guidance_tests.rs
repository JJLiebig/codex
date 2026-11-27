use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn task_guidance_follows_the_active_model_and_setting() -> Result<()> {
    let server = responses::start_mock_server().await;
    let codex_home = TempDir::new()?;
    let catalog_path = codex_home.path().join("models.json");
    let mut catalog = codex_models_manager::bundled_models_response()?;
    for (model, alias) in [
        ("gpt-6.2-sol", "openai.gpt-6.2-sol"),
        ("gpt-6.2-astra", "global.openai.gpt-6.2-astra"),
        ("gpt-6.2-sol", "us.openai.gpt-6.2-sol"),
    ] {
        let mut entry = catalog
            .models
            .iter()
            .find(|entry| entry.slug == model)
            .unwrap()
            .clone();
        entry.slug = alias.into();
        entry.use_responses_lite = false;
        entry.tool_mode = None;
        catalog.models.push(entry);
    }
    std::fs::write(&catalog_path, serde_json::to_vec(&catalog)?)?;
    MockResponsesConfig::new(&server.uri())
        .with_model("gpt-6.2-sol")
        .with_root_config(&format!(
            "model_catalog_json = {}",
            serde_json::to_string(&catalog_path)?
        ))
        .with_root_config("features.incremental_tools = true")
        .write(codex_home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let standard = app
        .start_thread(ThreadStartParams {
            config: Some([("features.incremental_tools".into(), json!(false))].into()),
            ..Default::default()
        })
        .await?
        .thread;
    let incremental = app
        .start_thread(ThreadStartParams {
            model: Some("gpt-6.2-astra".into()),
            ..Default::default()
        })
        .await?
        .thread;
    let disabled = app
        .start_thread(ThreadStartParams {
            config: Some([("gpt_6_2_task_guidance".into(), json!(false))].into()),
            ..Default::default()
        })
        .await?
        .thread;
    let custom = app
        .start_thread(ThreadStartParams {
            base_instructions: Some("A custom prompt.".into()),
            ..Default::default()
        })
        .await?
        .thread;

    for (thread_id, model, expected_count) in [
        (&standard.id, "gpt-6.2-sol", 1),
        (&standard.id, "gpt-6.1-sol", 0),
        (&standard.id, "openai.gpt-6.2-sol", 1),
        (&standard.id, "global.openai.gpt-6.2-astra", 1),
        (&standard.id, "us.openai.gpt-6.2-sol", 1),
        (&incremental.id, "gpt-6.2-astra", 1),
        (&disabled.id, "gpt-6.2-sol", 0),
        (&custom.id, "gpt-6.2-sol", 0),
    ] {
        let response = responses::mount_sse_once(
            &server,
            responses::sse(vec![
                responses::ev_response_created("guidance-response"),
                responses::ev_assistant_message("guidance-message", "Done"),
                responses::ev_completed("guidance-response"),
            ]),
        )
        .await;
        let completed = app
            .start_turn_and_wait_for_completion(TurnStartParams {
                thread_id: thread_id.clone(),
                model: Some(model.into()),
                input: vec![UserInput::Text {
                    text: "Check the outgoing task guidance.".into(),
                    text_elements: vec![],
                }],
                ..Default::default()
            })
            .await?;
        assert_eq!(completed.turn.status, TurnStatus::Completed);
        let request = response.single_request();
        assert_eq!(request.body_json()["model"], model);
        let input = request.input();
        let guidance_items: Vec<_> = input
            .iter()
            .filter(|item| {
                item["content"][0]["text"].as_str().is_some_and(|text| {
                    text.starts_with("Before settling on an approach or making changes")
                })
            })
            .collect();
        assert_eq!(guidance_items.len(), expected_count, "model {model}");
        for item in guidance_items {
            assert_eq!(item["role"], "developer");
            assert!(
                item["content"][0]["text"].as_str().unwrap().contains(
                    "Choose the simplest approach that delivers the full result the user is after, within the limits they set."
                )
            );
        }
    }
    Ok(())
}
