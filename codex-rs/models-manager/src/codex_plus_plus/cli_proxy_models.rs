//! Authoritative owned-proxy discovery, shared by parent and child sessions.

use super::*;
use tokio::sync::Semaphore;

/// Keeps the last complete canonical catalogue, including a successful empty result.
/// Cached metadata is only a discovery view; it grants no credential routing authority.
#[derive(Debug)]
pub struct CliProxyModelsManager {
    snapshot: RwLock<Option<ModelsCacheEntry>>,
    refresh: Semaphore,
    cache: Option<Arc<dyn ModelsCache>>,
    endpoint: SharedModelsEndpointClient,
}

impl CliProxyModelsManager {
    pub fn new(codex_home: PathBuf, endpoint: Arc<dyn ModelsEndpointClient>) -> Self {
        Self::new_with_cache(
            Some(Arc::new(FileModelsCache::new(
                codex_home.join(MODEL_CACHE_FILE),
                DEFAULT_MODEL_CACHE_TTL,
            ))),
            endpoint,
        )
    }

    pub fn new_with_cache(
        cache: Option<Arc<dyn ModelsCache>>,
        endpoint: Arc<dyn ModelsEndpointClient>,
    ) -> Self {
        Self {
            snapshot: RwLock::new(None),
            refresh: Semaphore::new(/*permits*/ 1),
            cache,
            endpoint,
        }
    }

    async fn refresh_catalogue(
        &self,
        strategy: RefreshStrategy,
        factory: HttpClientFactory,
    ) -> CoreResult<()> {
        // Serialize refreshes so a slower earlier reply cannot overwrite a newer catalogue.
        let _refresh = self
            .refresh
            .acquire()
            .await
            .map_err(std::io::Error::other)?;
        let version = crate::client_version_to_whole();
        let identity = self.endpoint.identity();
        let current = self
            .snapshot
            .read()
            .await
            .as_ref()
            .map(|entry| (entry.identity.clone(), entry.fetched_at));
        if strategy != RefreshStrategy::Online {
            if let Some((current_identity, fetched_at)) = &current
                && (strategy == RefreshStrategy::Offline
                    || (*current_identity == identity
                        && Utc::now().signed_duration_since(*fetched_at)
                            < chrono::Duration::seconds(DEFAULT_MODEL_CACHE_TTL.as_secs() as i64)))
            {
                return Ok(());
            }
            if let Some(cache) = &self.cache {
                match cache.load(&version).await {
                    Ok(Some(entry))
                        if identity.is_some()
                            && entry.identity == identity
                            && entry.identity == self.endpoint.identity()
                            && entry.client_version.as_deref() == Some(version.as_str())
                            && current
                                .as_ref()
                                .is_none_or(|(_, fetched_at)| entry.fetched_at > *fetched_at) =>
                    {
                        *self.snapshot.write().await = Some(entry);
                        return Ok(());
                    }
                    Err(error) => error!("failed to load owned model cache: {error}"),
                    Ok(Some(_)) | Ok(None) => {}
                }
            }
            if strategy == RefreshStrategy::Offline {
                return Ok(());
            }
        }
        let response = self.endpoint.list_models(&version, factory).await?;
        if Some(&response.identity) != self.endpoint.identity().as_ref() {
            return Ok(());
        }
        let entry = ModelsCacheEntry {
            fetched_at: Utc::now(),
            etag: response.etag,
            client_version: Some(version),
            identity: Some(response.identity),
            models: response.models,
        };
        if let Some(cache) = &self.cache
            && let Err(error) = cache.store(&entry).await
        {
            error!("failed to write owned model cache: {error}");
        }
        if entry.identity == self.endpoint.identity() {
            *self.snapshot.write().await = Some(entry);
        }
        Ok(())
    }
}

impl ModelsManager for CliProxyModelsManager {
    fn raw_model_catalog(
        &self,
        refresh_strategy: RefreshStrategy,
        http_client_factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ModelsResponse> {
        Box::pin(async move {
            if let Err(error) = self
                .refresh_catalogue(refresh_strategy, http_client_factory)
                .await
            {
                error!("failed to refresh owned model catalogue: {error}");
            }
            ModelsResponse {
                models: self.get_remote_models().await,
            }
        })
    }

    fn refresh_after_auth_change(&self, factory: HttpClientFactory) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async move {
            // Endpoint identity includes observed native auth changes; unchanged turns use TTL.
            // Preserve the existing best-effort turn-start deadline, including cache access.
            if tokio::time::timeout(
                Duration::from_secs(/*secs*/ 5),
                self.raw_model_catalog(RefreshStrategy::OnlineIfUncached, factory),
            )
            .await
            .is_err()
            {
                error!("owned model catalogue refresh after auth change timed out");
            }
        })
    }

    fn get_remote_models(&self) -> ModelsManagerFuture<'_, Vec<ModelInfo>> {
        Box::pin(async move {
            self.snapshot
                .read()
                .await
                .as_ref()
                .map(|entry| entry.models.clone())
                .unwrap_or_default()
        })
    }

    fn try_get_remote_models(&self) -> Result<Vec<ModelInfo>, TryLockError> {
        Ok(self
            .snapshot
            .try_read()?
            .as_ref()
            .map(|entry| entry.models.clone())
            .unwrap_or_default())
    }

    fn auth_manager(&self) -> Option<&AuthManager> {
        None
    }

    fn build_available_models(&self, mut models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        // Exact proxy membership, not the root native login, determines availability.
        models.sort_by_key(|model| model.priority);
        let mut presets = models.into_iter().map(Into::into).collect::<Vec<_>>();
        ModelPreset::mark_default_by_picker_visibility(&mut presets);
        presets
    }

    fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets()
    }

    fn refresh_if_new_etag(
        &self,
        etag: String,
        factory: HttpClientFactory,
    ) -> ModelsManagerFuture<'_, ()> {
        Box::pin(async move {
            // An unchanged rich ETag is not a credential inventory proof; keep the normal TTL.
            let unchanged = self
                .snapshot
                .read()
                .await
                .as_ref()
                .is_some_and(|entry| entry.etag.as_deref() == Some(etag.as_str()));
            let strategy = if unchanged {
                RefreshStrategy::OnlineIfUncached
            } else {
                RefreshStrategy::Online
            };
            self.raw_model_catalog(strategy, factory).await;
        })
    }
}

#[cfg(test)]
#[path = "cli_proxy_models_tests.rs"]
mod tests;
