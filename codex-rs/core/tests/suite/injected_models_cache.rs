//! Integration coverage for model catalogs supplied through the public cache interface.

use codex_core::TurnInputRequest;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use anyhow::Result;
use base64::Engine;
use chrono::Utc;
use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::built_in_model_providers;
use codex_models_manager::cache::ModelsCache;
use codex_models_manager::cache::ModelsCacheEntry;
use codex_models_manager::cache::ModelsCacheError;
use codex_models_manager::cache::ModelsCacheFuture;
use codex_models_manager::manager::ModelsEndpointClient;
use codex_models_manager::manager::ModelsEndpointFuture;
use codex_models_manager::manager::ModelsEndpointResponse;
use codex_models_manager::manager::OpenAiModelsManager;
use codex_models_manager::manager::RefreshStrategy;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::error::Result as CoreResult;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::submit_thread_settings;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use tokio::sync::oneshot;
use tokio::time::Duration;
use tokio::time::timeout;

#[derive(Debug)]
struct StoreGate {
    entered: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

#[derive(Debug)]
struct TestModelsCache {
    entry: Option<ModelsCacheEntry>,
    load_error: bool,
    stored_entries: Mutex<Vec<ModelsCacheEntry>>,
    store_gate: Mutex<Option<StoreGate>>,
}

impl ModelsCache for TestModelsCache {
    fn load<'a>(
        &'a self,
        _client_version: &'a str,
    ) -> ModelsCacheFuture<'a, Result<Option<ModelsCacheEntry>, ModelsCacheError>> {
        Box::pin(async move {
            if self.load_error {
                Err(ModelsCacheError::new("test load failure"))
            } else {
                Ok(self
                    .stored_entries
                    .lock()
                    .expect("stored entries lock should not be poisoned")
                    .last()
                    .cloned()
                    .or_else(|| self.entry.clone()))
            }
        })
    }

    fn store<'a>(
        &'a self,
        entry: &'a ModelsCacheEntry,
    ) -> ModelsCacheFuture<'a, Result<(), ModelsCacheError>> {
        Box::pin(async move {
            self.stored_entries
                .lock()
                .expect("stored entries lock should not be poisoned")
                .push(entry.clone());
            let gate = self
                .store_gate
                .lock()
                .expect("store gate lock should not be poisoned")
                .take();
            if let Some(StoreGate { entered, release }) = gate {
                let _ = entered.send(());
                let _ = release.await;
            }
            Ok(())
        })
    }

    fn refresh_ttl<'a>(
        &'a self,
        _client_version: &'a str,
        _identity: &'a str,
        _etag: &'a str,
    ) -> ModelsCacheFuture<'a, Result<(), ModelsCacheError>> {
        Box::pin(async move {
            let mut entry = self
                .entry
                .clone()
                .ok_or_else(|| ModelsCacheError::new("cache not found"))?;
            entry.fetched_at = Utc::now();
            self.stored_entries
                .lock()
                .expect("stored entries lock should not be poisoned")
                .push(entry);
            Ok(())
        })
    }
}

#[derive(Debug)]
struct TestModelsEndpoint {
    models: Mutex<Vec<ModelInfo>>,
    fetch_count: AtomicUsize,
}

impl TestModelsEndpoint {
    fn new(models: Vec<ModelInfo>) -> Arc<Self> {
        Arc::new(Self {
            models: Mutex::new(models),
            fetch_count: AtomicUsize::new(0),
        })
    }
}

impl ModelsEndpointClient for TestModelsEndpoint {
    fn identity(&self) -> Option<String> {
        Some("test-provider".to_string())
    }

    fn has_command_auth(&self) -> bool {
        false
    }

    fn uses_codex_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        Box::pin(async { true })
    }

    fn list_models<'a>(
        &'a self,
        _client_version: &'a str,
        _http_client_factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<ModelsEndpointResponse>> {
        Box::pin(async move {
            self.fetch_count.fetch_add(1, Ordering::SeqCst);
            Ok(ModelsEndpointResponse {
                models: self.models.lock().unwrap().clone(),
                etag: None,
                identity: self.identity().expect("test endpoint identity"),
            })
        })
    }
}

fn remote_model(slug: &str) -> ModelInfo {
    ModelInfo {
        visibility: ModelVisibility::List,
        used_fallback_model_metadata: false,
        ..model_info_from_slug(slug)
    }
}

fn models_manager(
    cache: Arc<dyn ModelsCache>,
    endpoint: Arc<TestModelsEndpoint>,
) -> SharedModelsManager {
    Arc::new(OpenAiModelsManager::new_with_cache(
        cache,
        endpoint,
        Some(AuthManager::from_auth_for_testing(
            CodexAuth::create_dummy_chatgpt_auth_for_testing(),
        )),
    ))
}

async fn run_agent_with_model(models_manager: SharedModelsManager, model_slug: &str) -> Result<()> {
    let server = responses::start_mock_server().await;
    let response_mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let mut builder = test_codex()
        .with_auth(CodexAuth::create_dummy_chatgpt_auth_for_testing())
        .with_models_manager(models_manager);
    let test = builder.build(&server).await?;
    let available_models = test
        .thread_manager
        .get_models_manager()
        .list_models(
            RefreshStrategy::OnlineIfUncached,
            codex_core::test_support::default_http_client_factory(),
        )
        .await;
    assert!(
        available_models
            .iter()
            .any(|model| model.model == model_slug)
    );

    submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some(model_slug.to_string()),
            ..Default::default()
        },
    )
    .await?;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    loop {
        if matches!(
            wait_for_event(&test.codex, |_| true).await,
            EventMsg::TurnComplete(_)
        ) {
            break;
        }
    }

    assert_eq!(
        response_mock.single_request().body_json()["model"],
        model_slug
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn injected_cache_hit_drives_agent_model_selection() -> Result<()> {
    let model_slug = "injected-cache-model";
    let cache = Arc::new(TestModelsCache {
        entry: Some(ModelsCacheEntry {
            identity: Some("test-provider".to_string()),
            fetched_at: Utc::now(),
            etag: None,
            client_version: Some(codex_models_manager::client_version_to_whole()),
            models: vec![remote_model(model_slug)],
        }),
        load_error: false,
        stored_entries: Mutex::new(Vec::new()),
        store_gate: Mutex::new(None),
    });
    let endpoint = TestModelsEndpoint::new(Vec::new());

    run_agent_with_model(models_manager(cache, endpoint.clone()), model_slug).await?;

    assert_eq!(endpoint.fetch_count.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn injected_cache_error_falls_back_for_agent_model_selection() -> Result<()> {
    let model_slug = "injected-cache-fallback-model";
    let cache = Arc::new(TestModelsCache {
        entry: None,
        load_error: true,
        stored_entries: Mutex::new(Vec::new()),
        store_gate: Mutex::new(None),
    });
    let endpoint = TestModelsEndpoint::new(vec![remote_model(model_slug)]);

    run_agent_with_model(models_manager(cache.clone(), endpoint.clone()), model_slug).await?;

    assert!(endpoint.fetch_count.load(Ordering::SeqCst) >= 1);
    assert!(
        !cache
            .stored_entries
            .lock()
            .expect("stored entries lock should not be poisoned")
            .is_empty()
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn owned_catalogue_refresh_agrees_for_parent_description_and_child_overrides() -> Result<()> {
    use codex_features::Feature;
    use codex_models_manager::manager::CliProxyModelsManager;
    use codex_protocol::openai_models::ReasoningEffort;
    use codex_protocol::openai_models::ReasoningEffortPreset;
    use responses::ev_function_call_with_namespace;
    use responses::mount_sse_once_match;
    use responses::namespace_child_tool;
    use serde_json::Value;
    use serde_json::json;

    let parent = remote_model("gpt-5.6-sol");
    let removed = remote_model("removed-model");
    let endpoint = TestModelsEndpoint::new(vec![parent.clone(), removed]);
    let manager: SharedModelsManager = Arc::new(CliProxyModelsManager::new_with_cache(
        /*cache*/ None,
        endpoint.clone(),
    ));
    manager.set_api_key_model_discovery_enabled(/*enabled*/ false);
    let factory = codex_core::test_support::default_http_client_factory();
    manager
        .list_models(RefreshStrategy::Online, factory.clone())
        .await;
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_model("gpt-5.6-sol")
        .with_models_manager(manager.clone())
        .with_config(|config| {
            config.features.enable(Feature::Collab).unwrap();
            config.features.enable(Feature::MultiAgentV2).unwrap();
            config.multi_agent_v2.hide_spawn_agent_metadata = false;
            config.multi_agent_v2.expose_spawn_agent_model_overrides = true;
        })
        .build_with_auto_env(&server)
        .await?;
    assert!(Arc::ptr_eq(
        &manager,
        &test.thread_manager.get_models_manager()
    ));
    let mut future = remote_model("gpt-6.2-whatever");
    future.context_window = Some(456_789);
    future.effective_context_window_percent = 100;
    future.default_reasoning_level = Some(ReasoningEffort::Low);
    future.supported_reasoning_levels = vec![ReasoningEffortPreset {
        effort: ReasoningEffort::Low,
        description: "Provider low".into(),
    }];
    *endpoint.models.lock().unwrap() = vec![parent.clone(), future.clone()];
    assert_eq!(
        manager
            .raw_model_catalog(RefreshStrategy::Online, factory)
            .await
            .models,
        vec![parent, future.clone()]
    );
    assert_eq!(
        manager
            .get_model_info(
                &future.slug,
                &codex_models_manager::ModelsManagerConfig::default()
            )
            .await,
        future
    );

    for (model, effort, expected_error) in [
        ("removed-model", "low", Some("Unknown model")),
        ("gpt-6.2-whatever", "high", Some("not supported")),
        ("gpt-6.2-whatever", "low", None),
    ] {
        let call_id = format!("spawn-{model}-{effort}");
        let first = mount_sse_once(
            &server,
            sse(vec![
                ev_function_call_with_namespace(
                    &call_id,
                    "collaboration",
                    "spawn_agent",
                    &json!({
                        "task_name": "fresh", "message": "complete", "fork_turns": "none",
                        "model": model, "reasoning_effort": effort,
                    })
                    .to_string(),
                ),
                ev_completed("spawn-response"),
            ]),
        )
        .await;
        let followup_id = call_id.clone();
        let followup = mount_sse_once_match(
            &server,
            move |request: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&request.body).unwrap();
                body["model"] == "gpt-5.6-sol"
                    && body["input"].as_array().unwrap().iter().any(|item| {
                        item["type"] == "function_call_output" && item["call_id"] == followup_id
                    })
            },
            sse(vec![ev_completed("parent-done")]),
        )
        .await;
        let child = mount_sse_once_match(
            &server,
            |request: &wiremock::Request| {
                serde_json::from_slice::<Value>(&request.body).unwrap()["model"]
                    == "gpt-6.2-whatever"
            },
            sse(vec![ev_completed("child-done")]),
        )
        .await;
        test.submit_turn("spawn a worker").await?;
        let body = first.single_request().body_json();
        let description = namespace_child_tool(&body, "collaboration", "spawn_agent")
            .and_then(|tool| tool["description"].as_str())
            .unwrap();
        assert!(description.contains("gpt-6.2-whatever"));
        assert!(description.contains("Reasoning efforts: low (default)."));
        assert!(!description.contains("removed-model"));
        let result = followup
            .single_request()
            .function_call_output_text(&call_id)
            .unwrap();
        if let Some(error) = expected_error {
            assert!(result.contains(error), "{result}");
            assert!(child.requests().is_empty());
        } else {
            let child_id = test
                .thread_manager
                .list_thread_ids()
                .await
                .into_iter()
                .find(|id| *id != test.session_configured.thread_id)
                .expect("spawned child");
            let thread = test.thread_manager.get_thread(child_id).await?;
            let started =
                wait_for_event(&thread, |event| matches!(event, EventMsg::TurnStarted(_))).await;
            let EventMsg::TurnStarted(started) = started else {
                unreachable!()
            };
            assert_eq!(started.model_context_window, Some(456_789));
            wait_for_event(&thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
            assert_eq!(
                child.single_request().body_json()["reasoning"]["effort"],
                "low"
            );
        }
        server.reset().await;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn account_switch_during_cache_store_preserves_new_catalog_for_next_turn() -> Result<()> {
    let server = wiremock::MockServer::start().await;
    let home = Arc::new(TempDir::new()?);
    let initial_auth = CodexAuth::create_dummy_chatgpt_auth_for_testing();
    let auth = AuthManager::from_auth_for_testing_with_home(
        initial_auth.clone(),
        home.path().to_path_buf(),
    );
    let mut provider_info = built_in_model_providers(/*openai_base_url*/ None)["openai"].clone();
    provider_info.base_url = Some(format!("{}/v1", server.uri()));
    provider_info.supports_websockets = false;
    let cache = Arc::new(TestModelsCache {
        entry: None,
        load_error: false,
        stored_entries: Mutex::new(Vec::new()),
        store_gate: Mutex::new(None),
    });
    let manager = create_model_provider(provider_info.clone(), Some(auth.clone()))
        .models_manager_with_cache(/*config_model_catalog*/ None, cache.clone());
    let mut old_model = remote_model("gpt-5.4");
    old_model.default_reasoning_summary = ReasoningSummary::Concise;
    let initial = responses::mount_models_once(
        &server,
        ModelsResponse {
            models: vec![old_model.clone()],
        },
    )
    .await;
    let test = test_codex()
        .with_home(home.clone())
        .with_auth(initial_auth.clone())
        .with_model("gpt-5.4")
        .with_models_manager(manager.clone())
        .with_config(move |config| {
            config.model_provider = provider_info;
            config.model_reasoning_summary = None;
        })
        .build_with_auto_env(&server)
        .await?;
    assert_eq!(initial.requests().len(), 1);

    // Pause after the old catalog has been stored, before its refresh can publish it.
    let (store_entered, entered) = oneshot::channel();
    let (release_store, release) = oneshot::channel();
    *cache.store_gate.lock().unwrap() = Some(StoreGate {
        entered: store_entered,
        release,
    });
    let old_request = responses::mount_models_once(
        &server,
        ModelsResponse {
            models: vec![old_model],
        },
    )
    .await;
    let old_refresh = tokio::spawn({
        let manager = manager.clone();
        async move {
            manager
                .list_models(
                    RefreshStrategy::Online,
                    codex_core::test_support::default_http_client_factory(),
                )
                .await
        }
    });
    timeout(Duration::from_secs(10), entered).await??;

    let mut tokens = initial_auth.get_token_data()?;
    tokens.id_token.raw_jwt = format!(
        "e30.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"email":"second@example.com"}"#)
    );
    tokens.account_id = Some("second-account".into());
    tokens.access_token = "second-account-token".into();
    std::fs::write(
        home.path().join("auth.json"),
        serde_json::to_vec(&serde_json::json!({
            "auth_mode": "chatgpt", "tokens": tokens, "last_refresh": Utc::now(),
        }))?,
    )?;
    auth.reload().await;
    test.thread_manager.auth_manager().reload().await;

    let mut new_model = remote_model("gpt-5.4");
    new_model.default_reasoning_summary = ReasoningSummary::Detailed;
    let new_request = responses::mount_models_once(
        &server,
        ModelsResponse {
            models: vec![new_model.clone()],
        },
    )
    .await;
    manager
        .list_models(
            RefreshStrategy::Online,
            codex_core::test_support::default_http_client_factory(),
        )
        .await;
    assert_eq!(manager.get_remote_models().await, vec![new_model.clone()]);
    release_store.send(()).unwrap();
    timeout(Duration::from_secs(10), old_refresh).await??;
    assert_eq!(manager.get_remote_models().await, vec![new_model]);
    for (mock, account) in [(old_request, "account_id"), (new_request, "second-account")] {
        let requests = mock.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].headers["chatgpt-account-id"], account);
    }

    let response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("next-turn"),
            ev_completed("next-turn"),
        ]),
    )
    .await;
    test.submit_turn("use the new account's model defaults")
        .await?;
    let request = response.single_request();
    assert_eq!(
        request.header("chatgpt-account-id").as_deref(),
        Some("second-account")
    );
    assert_eq!(request.body_json()["reasoning"]["summary"], "detailed");
    Ok(())
}
