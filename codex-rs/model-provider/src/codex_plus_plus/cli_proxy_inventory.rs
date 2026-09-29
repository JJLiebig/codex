//! Exact management membership joined to the native publication identity map.

use std::collections::BTreeMap;
use std::io;

use codex_http_client::HttpClient;
use codex_http_client::HttpTransport;
use codex_http_client::Request;
use codex_http_client::ReqwestTransport;
use codex_login::NativeCredentialSource;
use codex_protocol::openai_models::ModelInfo;
use http::Method;
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::cli_proxy_credentials::native_route;
use super::cli_proxy_runtime::RuntimeEndpoint;

// This is the aggregate compact membership budget, independent of rich prompt metadata.
const INVENTORY_BYTES: usize = 1024 * 1024;
const INVENTORY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct CredentialModels {
    pub source: Option<NativeCredentialSource>,
    pub name: String,
    pub auth_index: Option<String>,
    pub provider: Option<String>,
    pub prefix: Option<String>,
    pub disabled: bool,
    pub models: Vec<RegisteredModel>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
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
                    && file
                        .auth_index
                        .as_ref()
                        .is_some_and(|index| !index.is_empty())
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
    request
        .headers
        .insert(http::header::AUTHORIZATION, authorization);
    request.timeout = Some(INVENTORY_TIMEOUT);
    request.response_body_limit_bytes = Some(*remaining);
    let response = transport
        .execute(request)
        .await
        .map_err(|error| match error {
            codex_http_client::TransportError::Http { status, .. } => io::Error::other(format!(
                "CLIProxyAPI inventory request failed with HTTP {status}"
            )),
            error => io::Error::other(error),
        })?;
    *remaining -= response.body.len();
    serde_json::from_slice(&response.body).map_err(|error| {
        io::Error::other(format!(
            "CLIProxyAPI inventory could not be decoded at line {} column {}",
            error.line(),
            error.column()
        ))
    })
}

/// Only exact, unambiguous membership can turn a wire slug into a native model slug.
pub(super) fn normalize_catalogue(
    models: Vec<ModelInfo>,
    inventory: &[CredentialModels],
) -> Vec<ModelInfo> {
    let mut membership: BTreeMap<&str, Vec<(&CredentialModels, &RegisteredModel)>> =
        BTreeMap::new();
    for credential in inventory {
        for model in &credential.models {
            membership
                .entry(&model.id)
                .or_default()
                .push((credential, model));
        }
    }
    let mut groups: BTreeMap<String, Vec<(String, ModelInfo, bool)>> = BTreeMap::new();
    for original in models {
        let wire_slug = original.slug.clone();
        let mut normalized = original;
        let mut native = false;
        if let Some(entries) = membership.get(wire_slug.as_str())
            && let [(credential, member)] = entries.as_slice()
            && !credential.disabled
            && credential.provider.as_deref() == Some("codex")
            && member.provider.as_deref() == Some("openai")
            && member.owned_by.as_deref() == Some("openai")
        {
            if credential.source.is_some()
                && let Some(prefix) = &credential.prefix
                && let Some(slug) = wire_slug.strip_prefix(&format!("{prefix}/"))
                && !slug.is_empty()
            {
                normalized.slug = slug.into();
                native = true;
            }
            if legacy_gpt(&normalized.slug) {
                continue;
            }
        }
        groups
            .entry(normalized.slug.clone())
            .or_default()
            .push((wire_slug, normalized, native));
    }
    let mut result = Vec::new();
    for copies in groups.into_values() {
        // Different prompts/capabilities or a foreign collision keep the complete wire entries.
        // Those entries have no canonical native route until the conflict is resolved.
        if copies
            .iter()
            .all(|(_, model, native)| *native && model == &copies[0].1)
        {
            result.push(copies.into_iter().next().expect("nonempty model group").1);
        } else {
            result.extend(copies.into_iter().map(|(wire_slug, mut model, _)| {
                model.slug = wire_slug;
                model
            }));
        }
    }
    result
}

fn legacy_gpt(slug: &str) -> bool {
    let Some(version) = slug
        .strip_prefix("gpt-")
        .and_then(|name| name.split('-').next())
    else {
        return false;
    };
    if version == "4o" {
        return true;
    }
    let mut parts = version.split('.');
    let Some(major) = parts.next().and_then(|part| part.parse::<u32>().ok()) else {
        return false;
    };
    let minor = match parts.next() {
        Some(part) => match part.parse::<u32>() {
            Ok(minor) => minor,
            Err(_) => return false,
        },
        None => 0,
    };
    parts.next().is_none() && (major < 5 || (major == 5 && minor <= 4))
}

#[cfg(test)]
#[path = "cli_proxy_inventory_tests.rs"]
mod tests;
