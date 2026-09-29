//! Publish native access credentials without taking ownership of refresh or selection.

use std::collections::BTreeMap;
use std::io;
use std::time::Duration;

use codex_http_client::HttpClient;
use codex_http_client::HttpClientFactory;
use codex_http_client::HttpResponse;
use codex_login::AuthManager;
use codex_login::NativeCredential;
use codex_login::NativeCredentialSource;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;

use super::cli_proxy_runtime::CliProxyRuntime;
use super::cli_proxy_runtime::RuntimeEndpoint;

const NATIVE_PREFIX: &str = "codex-native-";
pub(super) const MANAGEMENT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
pub(super) struct Inventory {
    pub files: Vec<AuthFile>,
}

#[derive(Deserialize)]
pub(super) struct AuthFile {
    pub name: String,
    #[serde(rename = "type")]
    pub provider: String,
}

impl CliProxyRuntime {
    pub(super) async fn prepare(
        &self,
        manager: &AuthManager,
        factory: HttpClientFactory,
    ) -> io::Result<RuntimeEndpoint> {
        self.prepare_sources(manager, factory)
            .await
            .map(|(endpoint, _)| endpoint)
    }

    pub(super) async fn prepare_catalogue(
        &self,
        manager: &AuthManager,
        factory: HttpClientFactory,
    ) -> io::Result<(
        RuntimeEndpoint,
        Vec<super::cli_proxy_inventory::CredentialModels>,
    )> {
        let (endpoint, sources) = self.prepare_sources(manager, factory.clone()).await?;
        let inventory = super::cli_proxy_inventory::read_inventory(
            self.http_client(&factory)?,
            &endpoint,
            &sources,
        )
        .await?;
        Ok((endpoint, inventory))
    }

    async fn prepare_sources(
        &self,
        manager: &AuthManager,
        factory: HttpClientFactory,
    ) -> io::Result<(RuntimeEndpoint, Vec<NativeCredentialSource>)> {
        // Runtime startup/probing must finish before taking the native topology guards.
        let endpoint = self.ensure(factory.clone()).await?;
        // Reload the selected native source, including a login written after this host started.
        manager.reload().await;
        manager.auth().await;
        let client = self.http_client(&factory)?;
        let snapshot = manager.export_native_credentials().await?;
        reconcile(&client, &endpoint, snapshot.credentials()).await?;
        let sources = snapshot
            .credentials()
            .iter()
            .map(|credential| credential.source.clone())
            .collect();
        // The snapshot serializes publishers and native mutations until all writes acknowledge.
        drop(snapshot);
        Ok((endpoint, sources))
    }
}

pub(super) fn native_route(source: &NativeCredentialSource) -> (String, String) {
    let (source, account) = match source {
        NativeCredentialSource::Root(account) => ("root", account),
        NativeCredentialSource::Imported(account) => ("imported", account),
    };
    let prefix = format!("{NATIVE_PREFIX}{source}-{account}");
    (format!("{prefix}.json"), prefix)
}

pub(super) fn native_record(credential: &NativeCredential) -> (String, Value) {
    let (name, prefix) = native_route(&credential.source);
    let mut record = json!({
        "type": "codex",
        "access_token": credential.access_token,
        "account_id": credential.upstream_account_id,
        "expired": credential.expires_at.to_rfc3339(),
        "prefix": prefix,
    });
    if let Some(plan) = &credential.plan_type {
        record["plan_type"] = json!(plan);
    }
    (name, record)
}

pub(super) fn owned_filename(name: &str) -> bool {
    (name.starts_with("codex-native-root-") || name.starts_with("codex-native-imported-"))
        && name.ends_with(".json")
        && !name.contains(['/', '\\'])
}

pub(super) async fn delete_auth_file(
    client: &HttpClient,
    url: &url::Url,
    management_key: &str,
    name: &str,
) -> io::Result<()> {
    acknowledged(
        client
            .delete(url.clone())
            .query(&[("name", name)])
            .bearer_auth(management_key)
            .timeout(MANAGEMENT_TIMEOUT)
            .send()
            .await
            .map_err(io::Error::other)?,
    )
    .await
}

async fn reconcile(
    client: &HttpClient,
    endpoint: &RuntimeEndpoint,
    credentials: &[NativeCredential],
) -> io::Result<()> {
    let url = url::Url::parse(&endpoint.base_url)
        .and_then(|url| url.join("/v0/management/auth-files"))
        .map_err(io::Error::other)?;
    let inventory: Inventory = client
        .get(url.clone())
        .bearer_auth(&endpoint.management_key)
        .timeout(MANAGEMENT_TIMEOUT)
        .send()
        .await
        .map_err(io::Error::other)?
        .error_for_status()
        .map_err(io::Error::other)?
        .json()
        .await
        .map_err(io::Error::other)?;
    let desired: BTreeMap<_, _> = credentials.iter().map(native_record).collect();
    for file in &inventory.files {
        if owned_filename(&file.name)
            && file.provider == "codex"
            && !desired.contains_key(&file.name)
        {
            delete_auth_file(client, &url, &endpoint.management_key, &file.name).await?;
        }
    }
    for (name, record) in desired {
        if let Some(file) = inventory.files.iter().find(|file| file.name == name) {
            if file.provider != "codex" {
                return Err(io::Error::other(
                    "CLIProxyAPI native credential filename belongs to another provider",
                ));
            }
            let existing: Value = client
                .get(format!("{url}/download"))
                .query(&[("name", &name)])
                .bearer_auth(&endpoint.management_key)
                .timeout(MANAGEMENT_TIMEOUT)
                .send()
                .await
                .map_err(io::Error::other)?
                .error_for_status()
                .map_err(io::Error::other)?
                .json()
                .await
                .map_err(io::Error::other)?;
            if [
                "type",
                "access_token",
                "account_id",
                "expired",
                "plan_type",
                "prefix",
            ]
            .iter()
            .all(|field| existing.get(field) == record.get(field))
            {
                continue;
            }
        }
        acknowledged(
            client
                .post(url.clone())
                .query(&[("name", &name)])
                .bearer_auth(&endpoint.management_key)
                .timeout(MANAGEMENT_TIMEOUT)
                .json(&record)
                .send()
                .await
                .map_err(io::Error::other)?,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn acknowledged(response: HttpResponse) -> io::Result<()> {
    let response: Value = response
        .error_for_status()
        .map_err(io::Error::other)?
        .json()
        .await
        .map_err(io::Error::other)?;
    if response.get("status").and_then(Value::as_str) != Some("ok") {
        return Err(io::Error::other(
            "CLIProxyAPI did not acknowledge native credential publication",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "cli_proxy_credentials_tests.rs"]
mod tests;
