use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use codex_api::ModelsClient;
use codex_api::Provider;
use codex_api::ReqwestTransport;
use codex_api::SharedAuthProvider;
use codex_api::map_api_error;
use codex_http_client::HttpClient;
use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::cache::ModelsCache;
use codex_models_manager::manager::CliProxyModelsManager;
use codex_models_manager::manager::ModelsEndpointClient;
use codex_models_manager::manager::ModelsEndpointFuture;
use codex_models_manager::manager::ModelsEndpointResponse;
use codex_models_manager::manager::SharedModelsManager;
use codex_models_manager::manager::StaticModelsManager;
use codex_protocol::error::Result as CoreResult;
use codex_protocol::openai_models::ModelsResponse;
use http::HeaderMap;
use sha2::Digest;
use sha2::Sha256;

use super::cli_proxy_runtime::CliProxyRuntime;
use super::cli_proxy_runtime::RuntimeEndpoint;
use crate::BearerAuthProvider;
use crate::auth::ProviderAuthScope;
use crate::auth::ResolvedProviderAuth;
use crate::provider::ModelProvider;
use crate::provider::ModelProviderFuture;
use crate::provider::ProviderAccountResult;
use crate::provider::ProviderCapabilities;
use crate::provider::RemoteCompactionSupport;
use crate::provider::SharedModelProvider;

// Ten account copies of the current 392,476-byte rich catalogue fit with growth headroom.
const OWNED_MODEL_CATALOG_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub(crate) struct CliProxyModelProvider {
    info: ModelProviderInfo,
    native: SharedModelProvider,
    runtime: Option<CliProxyRuntime>,
}

impl CliProxyModelProvider {
    pub(crate) fn new(_info: ModelProviderInfo, auth_manager: Option<Arc<AuthManager>>) -> Self {
        // The reserved name is the opt-in identity; caller URLs and auth never reach transport.
        let info = ModelProviderInfo::create_cli_proxy_provider();
        let native = crate::provider::create_model_provider(
            ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            auth_manager.clone(),
        );
        let runtime = auth_manager.map(|manager| {
            CliProxyRuntime::new(
                manager.runtime_config().codex_home,
                std::env::var_os("CODEX_CLI_PROXY_EXECUTABLE").map(PathBuf::from),
            )
        });
        Self {
            info,
            native,
            runtime,
        }
    }

    fn runtime(&self) -> CoreResult<&CliProxyRuntime> {
        self.runtime.as_ref().ok_or_else(|| {
            codex_protocol::error::CodexErr::UnsupportedOperation(
                "CLIProxyAPI needs an inference host with a CODEX_HOME auth runtime".into(),
            )
        })
    }

    fn http_client_factory(&self) -> CoreResult<HttpClientFactory> {
        self.native
            .auth_manager()
            .map(|manager| manager.http_client_factory())
            .ok_or_else(|| {
                codex_protocol::error::CodexErr::UnsupportedOperation(
                    "CLIProxyAPI needs a CODEX_HOME auth runtime".into(),
                )
            })
    }

    async fn prepare(&self) -> CoreResult<RuntimeEndpoint> {
        let manager = self.auth_manager().ok_or_else(|| {
            codex_protocol::error::CodexErr::UnsupportedOperation(
                "CLIProxyAPI needs a CODEX_HOME auth runtime".into(),
            )
        })?;
        Ok(self
            .runtime()?
            .prepare(&manager, manager.http_client_factory())
            .await?)
    }

    pub(super) fn models_endpoint(&self, home: Option<PathBuf>) -> Arc<dyn ModelsEndpointClient> {
        let runtime = self.runtime.clone().or_else(|| {
            home.map(|home| {
                CliProxyRuntime::new(
                    home,
                    std::env::var_os("CODEX_CLI_PROXY_EXECUTABLE").map(PathBuf::from),
                )
            })
        });
        Arc::new(CliProxyModelsEndpoint::new(runtime, self.auth_manager()))
    }
}

impl ModelProvider for CliProxyModelProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn api_http_client(&self) -> CoreResult<Option<HttpClient>> {
        Ok(Some(
            self.runtime()?.http_client(&self.http_client_factory()?)?,
        ))
    }

    fn prepare_request<'a>(
        &'a self,
        model: &'a str,
    ) -> ModelProviderFuture<'a, CoreResult<Option<crate::PreparedModelRequest>>> {
        Box::pin(async move {
            let manager = self.auth_manager().ok_or_else(|| {
                codex_protocol::error::CodexErr::UnsupportedOperation(
                    "CLIProxyAPI needs a CODEX_HOME auth runtime".into(),
                )
            })?;
            let prepared = self
                .runtime()?
                .prepare_inventory(&manager, manager.http_client_factory())
                .await?;
            let (model, mut route) = super::prepared_request::resolve_route(
                model,
                prepared.selected_source.as_ref(),
                prepared.inventory,
                prepared.generation,
            )?;
            route.native_expectation = prepared
                .expectation
                .filter(|expected| route.native_source() == Some(expected.source()));
            let mut provider = self.info.to_api_provider(/*auth_mode*/ None)?;
            provider.base_url = prepared.endpoint.base_url;
            Ok(Some(crate::PreparedModelRequest {
                provider,
                auth: ResolvedProviderAuth::new(Arc::new(BearerAuthProvider::new(
                    prepared.endpoint.inference_key,
                ))),
                http_client: Some(prepared.client),
                model,
                route: Some(route),
            }))
        })
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            remote_compaction: RemoteCompactionSupport::V2,
            ..ProviderCapabilities::default()
        }
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.native.auth_manager()
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        self.native.auth()
    }

    fn account_state(&self) -> ProviderAccountResult {
        self.native.account_state()
    }

    fn api_provider(&self) -> ModelProviderFuture<'_, CoreResult<Provider>> {
        Box::pin(async move {
            let endpoint = self.prepare().await?;
            let mut provider = self.info.to_api_provider(/*auth_mode*/ None)?;
            provider.base_url = endpoint.base_url;
            Ok(provider)
        })
    }

    fn runtime_base_url(&self) -> ModelProviderFuture<'_, CoreResult<Option<String>>> {
        Box::pin(async move { Ok(Some(self.prepare().await?.base_url)) })
    }

    fn api_auth(&self) -> ModelProviderFuture<'_, CoreResult<SharedAuthProvider>> {
        Box::pin(async move {
            let endpoint = self.prepare().await?;
            Ok(Arc::new(BearerAuthProvider::new(endpoint.inference_key)) as SharedAuthProvider)
        })
    }

    fn api_auth_for_scope(
        &self,
        _scope: ProviderAuthScope,
    ) -> ModelProviderFuture<'_, CoreResult<ResolvedProviderAuth>> {
        Box::pin(async move { self.api_auth().await.map(ResolvedProviderAuth::new) })
    }

    fn models_manager(
        &self,
        codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        if let Some(catalog) = config_model_catalog {
            return Arc::new(StaticModelsManager::new(self.auth_manager(), catalog));
        }
        Arc::new(CliProxyModelsManager::new(
            codex_home.clone(),
            self.models_endpoint(Some(codex_home)),
        ))
    }

    fn models_manager_without_cache(
        &self,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        if let Some(catalog) = config_model_catalog {
            return Arc::new(StaticModelsManager::new(self.auth_manager(), catalog));
        }
        let endpoint = self.models_endpoint(None);
        Arc::new(CliProxyModelsManager::new_with_cache(
            /*cache*/ None, endpoint,
        ))
    }

    fn models_manager_with_cache(
        &self,
        config_model_catalog: Option<ModelsResponse>,
        cache: Arc<dyn ModelsCache>,
    ) -> SharedModelsManager {
        if let Some(catalog) = config_model_catalog {
            return Arc::new(StaticModelsManager::new(self.auth_manager(), catalog));
        }
        let endpoint = self.models_endpoint(None);
        Arc::new(CliProxyModelsManager::new_with_cache(Some(cache), endpoint))
    }
}

#[derive(Debug)]
struct CliProxyModelsEndpoint {
    runtime: Option<CliProxyRuntime>,
    identity: Option<String>,
    auth_manager: Option<Arc<AuthManager>>,
}

impl CliProxyModelsEndpoint {
    fn new(runtime: Option<CliProxyRuntime>, auth_manager: Option<Arc<AuthManager>>) -> Self {
        let identity = runtime.as_ref().map(|runtime| {
            let mut digest = Sha256::new();
            digest.update(b"cli-proxy-models-v2-canonical");
            digest.update(runtime.home().as_os_str().as_encoded_bytes());
            format!("{:x}", digest.finalize())
        });
        Self {
            runtime,
            identity,
            auth_manager,
        }
    }
}

impl ModelsEndpointClient for CliProxyModelsEndpoint {
    fn identity(&self) -> Option<String> {
        self.identity.as_ref().map(|identity| {
            let generation = self
                .auth_manager
                .as_ref()
                .map(|manager| *manager.auth_change_receiver().borrow())
                .unwrap_or_default();
            format!("{identity}:{generation}")
        })
    }

    fn has_provider_api_key(&self) -> bool {
        true
    }

    fn has_command_auth(&self) -> bool {
        false
    }

    fn supports_api_key_models(&self) -> bool {
        true
    }

    fn uses_codex_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        Box::pin(async { true })
    }

    fn list_models<'a>(
        &'a self,
        client_version: &'a str,
        http_client_factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<ModelsEndpointResponse>> {
        Box::pin(async move {
            let runtime = self.runtime.as_ref().ok_or_else(|| {
                codex_protocol::error::CodexErr::UnsupportedOperation(
                    "CLIProxyAPI needs an inference host with a CODEX_HOME auth runtime".into(),
                )
            })?;
            let manager = self.auth_manager.as_ref().ok_or_else(|| {
                codex_protocol::error::CodexErr::UnsupportedOperation(
                    "CLIProxyAPI needs a CODEX_HOME auth runtime".into(),
                )
            })?;
            let (endpoint, inventory, generation) = runtime
                .prepare_catalogue(manager, http_client_factory.clone())
                .await?;
            let identity = self
                .identity
                .as_ref()
                .map(|identity| format!("{identity}:{generation}"))
                .ok_or_else(|| {
                    codex_protocol::error::CodexErr::UnsupportedOperation(
                        "CLIProxyAPI model cache identity is unavailable".into(),
                    )
                })?;
            let mut provider = ModelProviderInfo::create_cli_proxy_provider()
                .to_api_provider(/*auth_mode*/ None)?;
            provider.base_url = endpoint.base_url;
            let request_url =
                ModelsClient::<ReqwestTransport>::request_url(&provider, client_version);
            let transport =
                ReqwestTransport::from_http_client(runtime.http_client(&http_client_factory)?);
            let auth: SharedAuthProvider =
                Arc::new(BearerAuthProvider::new(endpoint.inference_key));
            let client = ModelsClient::new(transport, provider, auth);
            let (models, etag) = tokio::time::timeout(
                Duration::from_secs(5),
                client.list_models(
                    request_url,
                    HeaderMap::new(),
                    Some(OWNED_MODEL_CATALOG_BYTES),
                ),
            )
            .await
            .map_err(|_| codex_protocol::error::CodexErr::RequestTimeout)?
            .map_err(map_api_error)?;
            Ok(ModelsEndpointResponse {
                models: super::cli_proxy_inventory::normalize_catalogue(models, &inventory),
                etag,
                identity,
            })
        })
    }
}

#[cfg(test)]
#[path = "cli_proxy_provider_tests.rs"]
mod tests;
