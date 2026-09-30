//! Immutable request routing through an owned proxy publication snapshot.

use std::sync::Arc;

use codex_api::Provider;
use codex_http_client::HttpClient;
use codex_login::NativeCredentialSource;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result;

use super::cli_proxy_credentials::owned_filename;
use super::cli_proxy_inventory::CredentialModels;
use crate::ModelProvider;
use crate::ResolvedProviderAuth;

/// Endpoint, credentials and wire model resolved together, before body encoding.
pub struct PreparedModelRequest {
    pub provider: Provider,
    pub auth: ResolvedProviderAuth,
    pub http_client: Option<HttpClient>,
    pub model: String,
    pub route: Option<ProxyRequestRoute>,
}

#[cfg(test)]
#[path = "prepared_request_tests.rs"]
mod tests;

/// Non-secret membership retained from the same guarded publication as this request.
#[derive(Debug)]
pub struct ProxyRequestRoute {
    pub auth_revision: u64,
    pub native_display_label: Option<String>,
    pub(super) native_expectation: Option<codex_login::auth::NativeCredentialExpectation>,
    inventory: Arc<[CredentialModels]>,
    native_credential: Option<usize>,
}

impl ProxyRequestRoute {
    /// Identify only the provider advertised for this exact frozen wire model, not its account.
    pub fn is_claude_model(&self, wire_model: &str) -> bool {
        self.inventory.iter().any(|credential| {
            !credential.disabled
                && credential.source.is_none()
                && credential.provider.as_deref() == Some("claude")
                && credential.models.iter().any(|member| {
                    member.id == wire_model && member.provider.as_deref() == Some("claude")
                })
        })
    }

    pub fn is_suspended_auth_error(error: &codex_api::TransportError) -> bool {
        let codex_api::TransportError::Http {
            status,
            body: Some(body),
            ..
        } = error
        else {
            return false;
        };
        *status == http::StatusCode::SERVICE_UNAVAILABLE
            && serde_json::from_str::<serde_json::Value>(body).is_ok_and(|body| {
                body["error"]["type"] == "authentication_error"
                    && body["error"]["code"] == "upstream_authentication_required"
            })
    }

    /// Join only the bounded native trace shape emitted by the pinned owned executor.
    pub fn served_native_source(&self, trace: &str) -> Option<&NativeCredentialSource> {
        if trace.len() > 128 {
            return None;
        }
        let parts: Vec<_> = trace.split('-').collect();
        (parts.len() == 3
            && parts[0].len() == 14
            && parts[0].bytes().all(|byte| byte.is_ascii_digit())
            && parts[1].len() == 16
            && parts[1].bytes().all(|byte| byte.is_ascii_hexdigit())
            && parts[2].len() == 8
            && parts[2].bytes().all(|byte| byte.is_ascii_hexdigit())
            && self.native_auth_index() == Some(parts[1]))
        .then(|| self.native_source())
        .flatten()
    }

    /// Proved upstream 401 for an owned Codex executor without a proxy refresh owner.
    pub fn native_auth_failure(
        &self,
        error: &codex_api::TransportError,
    ) -> Option<&codex_login::auth::NativeCredentialExpectation> {
        let codex_api::TransportError::Http {
            status,
            headers,
            body,
            ..
        } = error
        else {
            return None;
        };
        if *status != http::StatusCode::UNAUTHORIZED {
            return None;
        }
        let trace = headers.as_ref()?.get("x-cpa-trace-id")?.to_str().ok()?;
        self.served_native_source(trace)?;
        let body: serde_json::Value = serde_json::from_str(body.as_deref()?).ok()?;
        let error = body.get("error")?;
        (error.get("type").and_then(serde_json::Value::as_str) == Some("authentication_error")
            && error.get("code").and_then(serde_json::Value::as_str) == Some("auth_unavailable"))
        .then(|| self.native_expectation())
        .flatten()
    }

    pub fn native_expectation(&self) -> Option<&codex_login::auth::NativeCredentialExpectation> {
        self.native_expectation.as_ref()
    }

    pub fn native_source(&self) -> Option<&NativeCredentialSource> {
        self.inventory.get(self.native_credential?)?.source.as_ref()
    }

    pub fn native_auth_index(&self) -> Option<&str> {
        self.inventory
            .get(self.native_credential?)?
            .auth_index
            .as_deref()
    }
}

/// Preserve the configured-provider path for tools without session-scoped auth.
pub async fn prepare_configured_request(
    provider: &dyn ModelProvider,
    model: &str,
) -> Result<PreparedModelRequest> {
    if let Some(prepared) = provider.prepare_request(model).await? {
        return Ok(prepared);
    }
    Ok(PreparedModelRequest {
        provider: provider.api_provider().await?,
        auth: ResolvedProviderAuth::new(provider.api_auth().await?),
        http_client: provider.api_http_client()?,
        model: model.to_owned(),
        route: None,
    })
}

pub(super) fn resolve_route(
    model: &str,
    selected: Option<&NativeCredentialSource>,
    inventory: Vec<CredentialModels>,
    auth_revision: u64,
) -> Result<(String, ProxyRequestRoute)> {
    let unavailable = || {
        CodexErr::UnsupportedOperation(format!(
            "Model {model:?} is unavailable for the selected account; refresh the model list or choose another model"
        ))
    };
    let mut wire_model = None;
    let mut native_credential = None;
    let mut ownership = None;
    for (index, credential) in inventory.iter().enumerate() {
        if credential.disabled {
            continue;
        }
        for member in &credential.models {
            let canonical = credential
                .prefix
                .as_ref()
                .and_then(|prefix| member.id.strip_prefix(prefix)?.strip_prefix('/'));
            if canonical.unwrap_or(&member.id) != model && member.id != model {
                continue;
            }
            let provider = credential
                .provider
                .as_deref()
                .filter(|value| !value.is_empty());
            let native = credential.source.is_some();
            let valid = (native || !owned_filename(&credential.name))
                && credential
                    .auth_index
                    .as_ref()
                    .is_some_and(|value| !value.is_empty())
                && match provider {
                    Some("codex") => {
                        member.provider.as_deref() == Some("openai")
                            && member.owned_by.as_deref() == Some("openai")
                    }
                    Some(provider) => member.provider.as_deref() == Some(provider),
                    None => false,
                };
            if !valid
                || (native && canonical.is_none())
                || ownership.is_some_and(|previous| previous != (native, provider))
            {
                return Err(unavailable());
            }
            ownership = Some((native, provider));
            if native {
                if credential.source.as_ref() != selected {
                    continue;
                }
                // A prefix must identify one credential and one exact advertised model.
                if wire_model.is_some()
                    || inventory.iter().enumerate().any(|(other, entry)| {
                        other != index
                            && !entry.disabled
                            && (entry.auth_index == credential.auth_index
                                || entry.models.iter().any(|model| model.id == member.id))
                    })
                {
                    return Err(unavailable());
                }
                native_credential = Some(index);
            }
            wire_model = Some(member.id.clone());
        }
    }
    Ok((
        wire_model.ok_or_else(unavailable)?,
        ProxyRequestRoute {
            auth_revision,
            native_display_label: None,
            native_expectation: None,
            inventory: inventory.into(),
            native_credential,
        },
    ))
}
