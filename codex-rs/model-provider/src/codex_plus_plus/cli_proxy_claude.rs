//! Claude login stays in the proxy; only an invocation and safe membership leave this module.

use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use codex_http_client::HttpClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::ReqwestTransport;
use serde::Deserialize;

use super::cli_proxy_inventory::read_json;
use super::cli_proxy_runtime::CliProxyRuntime;
use super::cli_proxy_runtime::RuntimeEndpoint;

/// Public management metadata only. No token/auth-file contents are read or returned.
#[derive(Debug, PartialEq, Eq)]
pub struct ClaudeAccount {
    pub name: String,
    pub email: Option<String>,
    pub disabled: bool,
    pub unavailable: bool,
}

/// Explicit sign-in may provision/start the owned server; this child only runs native login.
pub async fn prepare_cli_proxy_claude_login(
    codex_home: &Path,
    factory: HttpClientFactory,
) -> io::Result<Command> {
    let runtime = CliProxyRuntime::new(
        codex_home.to_path_buf(),
        std::env::var_os("CODEX_CLI_PROXY_EXECUTABLE").map(PathBuf::from),
    );
    runtime.ensure(factory.clone()).await?;
    let attached = runtime.attach_only(factory).await?;
    if attached
        .as_ref()
        .and_then(|runtime| runtime.endpoint.as_ref())
        .is_none()
    {
        return Err(io::Error::other(
            "The owned runtime stopped before sign-in; retry codex account claude add",
        ));
    }
    runtime.claude_login_command()
}

/// Attach-only: `None` means no verified running server, and listing never starts one.
pub async fn list_cli_proxy_claude_accounts(
    codex_home: &Path,
    factory: HttpClientFactory,
) -> io::Result<Option<Vec<ClaudeAccount>>> {
    let runtime = CliProxyRuntime::new(codex_home.to_path_buf(), /*executable*/ None);
    let Some(attached) = runtime.attach_only(factory.clone()).await? else {
        return Ok(None);
    };
    let Some(endpoint) = &attached.endpoint else {
        return Ok(None);
    };
    read_claude_accounts(runtime.http_client(&factory)?, endpoint)
        .await
        .map(Some)
}

async fn read_claude_accounts(
    client: HttpClient,
    endpoint: &RuntimeEndpoint,
) -> io::Result<Vec<ClaudeAccount>> {
    #[derive(Deserialize)]
    struct Files {
        files: Vec<File>,
    }
    #[derive(Deserialize)]
    struct File {
        name: String,
        provider: Option<String>,
        #[serde(rename = "type")]
        kind: Option<String>,
        email: Option<String>,
        disabled: Option<bool>,
        unavailable: Option<bool>,
    }
    let url = url::Url::parse(&endpoint.base_url)
        .and_then(|url| url.join("/v0/management/auth-files"))
        .map_err(io::Error::other)?;
    let transport = ReqwestTransport::from_http_client(client);
    let mut remaining = 1024 * 1024;
    let files: Files = read_json(&transport, url, endpoint, &mut remaining).await?;
    Ok(files
        .files
        .into_iter()
        .filter(|file| {
            file.provider.as_deref() == Some("claude")
                && file.kind.as_deref().is_none_or(|kind| kind == "claude")
        })
        .map(|file| ClaudeAccount {
            name: file.name,
            email: file.email,
            disabled: file.disabled.unwrap_or(true),
            unavailable: file.unavailable.unwrap_or(true),
        })
        .collect())
}

#[cfg(test)]
#[path = "cli_proxy_claude_tests.rs"]
mod tests;
