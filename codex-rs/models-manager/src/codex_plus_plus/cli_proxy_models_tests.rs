use super::*;
use codex_http_client::OutboundProxyPolicy;
use codex_protocol::error::CodexErr;
use pretty_assertions::assert_eq;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::AtomicU64;

#[derive(Debug)]
struct Endpoint(StdMutex<VecDeque<CoreResult<Vec<ModelInfo>>>>, AtomicU64);

impl ModelsEndpointClient for Endpoint {
    fn identity(&self) -> Option<String> {
        Some(format!("owned-pool-{}", self.1.load(Ordering::SeqCst)))
    }
    fn has_command_auth(&self) -> bool {
        false
    }
    fn uses_codex_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn list_models<'a>(
        &'a self,
        _version: &'a str,
        _factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<ModelsEndpointResponse>> {
        Box::pin(async move {
            Ok(ModelsEndpointResponse {
                models: self
                    .0
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected fetch")?,
                etag: Some("unchanged-rich-catalogue".into()),
                identity: self.identity().unwrap(),
            })
        })
    }
}

#[tokio::test]
async fn refresh_replaces_complete_snapshots_including_empty_and_keeps_failures() {
    let home = tempfile::tempdir().unwrap();
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let mut model = crate::bundled_models_response().unwrap().models.remove(0);
    model.slug = "gpt-6.2-whatever".into();
    model.visibility = ModelVisibility::List;
    model.supported_in_api = false;
    model.context_window = Some(456_789);
    let mut future = model.clone();
    future.slug = "future-x9.7".into();
    let endpoint = Arc::new(Endpoint(
        StdMutex::new(VecDeque::from([
            Ok(vec![model.clone()]),
            Err(CodexErr::RequestTimeout),
            Ok(vec![future.clone()]),
            Ok(Vec::new()),
        ])),
        AtomicU64::new(0),
    ));
    let manager = CliProxyModelsManager::new(home.path().to_owned(), endpoint.clone());
    manager.set_api_key_model_discovery_enabled(/*enabled*/ false);
    assert!(manager.try_list_models().unwrap().is_empty());
    assert_eq!(
        manager
            .raw_model_catalog(RefreshStrategy::Online, factory.clone())
            .await
            .models,
        vec![model.clone()]
    );
    assert_eq!(manager.try_list_models().unwrap()[0].model, model.slug);
    assert_eq!(
        manager
            .raw_model_catalog(RefreshStrategy::Online, factory.clone())
            .await
            .models,
        vec![model]
    );

    // Observed auth changes bypass a fresh snapshot without a per-turn network fetch.
    endpoint.1.fetch_add(1, Ordering::SeqCst);
    manager.refresh_after_auth_change(factory.clone()).await;
    manager
        .refresh_if_new_etag("unchanged-rich-catalogue".into(), factory.clone())
        .await;
    assert_eq!(manager.get_remote_models().await, vec![future.clone()]);
    assert_eq!(
        manager
            .raw_model_catalog(RefreshStrategy::Offline, factory.clone())
            .await
            .models,
        vec![future]
    );
    manager
        .raw_model_catalog(RefreshStrategy::Online, factory.clone())
        .await;
    assert!(manager.try_list_models().unwrap().is_empty());
    // Persisted empty is a hit, not a reason to fetch or reinstate bundled models.
    let restored = CliProxyModelsManager::new(home.path().to_owned(), endpoint.clone());
    assert!(
        restored
            .list_models(RefreshStrategy::OnlineIfUncached, factory)
            .await
            .is_empty()
    );
    assert!(endpoint.0.lock().unwrap().is_empty());
}
