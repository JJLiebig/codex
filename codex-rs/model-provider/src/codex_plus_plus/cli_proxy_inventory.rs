//! Exact management membership joined to the native publication identity map.

use std::collections::BTreeMap;
use std::io;

use codex_http_client::HttpClient;
use codex_http_client::HttpTransport;
use codex_http_client::Request;
use codex_http_client::ReqwestTransport;
use codex_login::NativeCredentialSource;
use http::Method;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::cli_proxy_credentials::native_route;
use super::cli_proxy_runtime::RuntimeEndpoint;

// This is the aggregate compact membership budget, independent of rich prompt metadata.
const INVENTORY_BYTES: usize = 1024 * 1024;
const INVENTORY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub(super) struct CredentialModels {
    pub source: Option<NativeCredentialSource>,
    pub name: String,
    pub auth_index: Option<String>,
    pub provider: Option<String>,
    pub prefix: Option<String>,
    pub disabled: bool,
    pub models: Vec<RegisteredModel>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(super) struct RegisteredModel {
    pub id: String,
    #[serde(rename = "type")]
    pub provider: Option<String>,
    pub owned_by: Option<String>,
}

#[derive(Deserialize)]
struct AuthFiles {
    files: Vec<AuthFile>,
}

#[derive(Deserialize)]
struct AuthFile {
    name: String,
    #[serde(rename = "type")]
    kind: Option<String>,
    provider: Option<String>,
    auth_index: Option<String>,
    // Absent state cannot grant native eligibility (disk-only listings lack this field).
    disabled: Option<bool>,
}

#[derive(Deserialize)]
struct Models {
    models: Vec<RegisteredModel>,
}

pub(super) async fn read_inventory(
    client: HttpClient,
    endpoint: &RuntimeEndpoint,
    sources: &[NativeCredentialSource],
) -> io::Result<Vec<CredentialModels>> {
    tokio::time::timeout(INVENTORY_TIMEOUT, async {
        let transport = ReqwestTransport::from_http_client(client);
        let url = url::Url::parse(&endpoint.base_url)
            .and_then(|url| url.join("/v0/management/auth-files"))
            .map_err(io::Error::other)?;
        let mut remaining = INVENTORY_BYTES;
        let files: AuthFiles = read_json(&transport, url.clone(), endpoint, &mut remaining).await?;
        let native: BTreeMap<_, _> = sources
            .iter()
            .map(|source| {
                let (name, prefix) = native_route(source);
                (name, (source, prefix))
            })
            .collect();
        let mut inventory = Vec::new();
        for file in files.files {
            let mut models_url = url.join("auth-files/models").map_err(io::Error::other)?;
            models_url.query_pairs_mut().append_pair("name", &file.name);
            let models: Models =
                read_json(&transport, models_url, endpoint, &mut remaining).await?;
            let provider = match (&file.kind, &file.provider) {
                (Some(kind), Some(provider)) if kind != provider => None,
                (_, Some(provider)) | (Some(provider), None) => Some(provider.clone()),
                (None, None) => None,
            };
            let identity = native.get(&file.name).filter(|_| {
                provider.as_deref() == Some("codex")
                    && file.auth_index.as_ref().is_some_and(|index| !index.is_empty())
            });
            inventory.push(CredentialModels {
                source: identity.map(|(source, _)| (*source).clone()),
                prefix: identity.map(|(_, prefix)| prefix.clone()),
                name: file.name,
                auth_index: file.auth_index,
                provider,
                disabled: file.disabled.unwrap_or(true),
                models: models.models,
            });
        }
        Ok(inventory)
    })
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "CLIProxyAPI inventory timed out"))?
}

async fn read_json<T: DeserializeOwned>(
    transport: &ReqwestTransport,
    url: url::Url,
    endpoint: &RuntimeEndpoint,
    remaining: &mut usize,
) -> io::Result<T> {
    let mut request = Request::new(Method::GET, url.into());
    let mut authorization = format!("Bearer {}", endpoint.management_key)
        .parse::<http::HeaderValue>()
        .map_err(io::Error::other)?;
    authorization.set_sensitive(true);
    request.headers.insert(http::header::AUTHORIZATION, authorization);
    request.timeout = Some(INVENTORY_TIMEOUT);
    request.response_body_limit_bytes = Some(*remaining);
    let response = transport.execute(request).await.map_err(io::Error::other)?;
    *remaining -= response.body.len();
    serde_json::from_slice(&response.body).map_err(|error| {
        io::Error::other(format!(
            "CLIProxyAPI inventory could not be decoded at line {} column {}",
            error.line(), error.column()
        ))
    })
}
