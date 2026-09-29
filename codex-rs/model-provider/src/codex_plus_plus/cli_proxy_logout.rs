//! Remove stale native copies after logout without starting or changing the owned runtime.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use codex_http_client::HttpClientFactory;
use codex_login::AuthManager;
use serde_json::Value;

use super::cli_proxy_credentials::Inventory;
use super::cli_proxy_credentials::MANAGEMENT_TIMEOUT;
use super::cli_proxy_credentials::delete_auth_file;
use super::cli_proxy_credentials::native_record;
use super::cli_proxy_credentials::owned_filename;
use super::cli_proxy_runtime::CliProxyRuntime;

/// Call only after native logout releases its locks. A new disk login is retained.
pub async fn cleanup_cli_proxy_credentials(
    codex_home: &Path,
    manager: &AuthManager,
    factory: HttpClientFactory,
) -> io::Result<()> {
    let runtime = CliProxyRuntime::new(codex_home.to_path_buf(), None);
    // All paths take runtime before native; startup/publication never takes them in reverse.
    let Some(attached) = runtime.attach_only(factory.clone()).await? else {
        return Ok(());
    };
    let snapshot = manager.native_credential_cleanup_snapshot().await?;
    let retained: BTreeSet<_> = snapshot
        .credentials()
        .iter()
        .map(|c| native_record(c).0)
        .collect();
    if let Some(endpoint) = &attached.endpoint {
        let client = runtime.http_client(&factory)?;
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
        for file in inventory.files {
            if owned_filename(&file.name)
                && file.provider == "codex"
                && !retained.contains(&file.name)
            {
                delete_auth_file(&client, &url, &endpoint.management_key, &file.name).await?;
            }
        }
    } else {
        let directory = codex_home.join("cli-proxy/auth");
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Err(io::Error::other("CLIProxyAPI auth directory is invalid")),
            Err(error) => return Err(error),
        }
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !owned_filename(name) || retained.contains(name) {
                continue;
            }
            if !entry.file_type()?.is_file() {
                return Err(io::Error::other(
                    "CLIProxyAPI native auth file is not regular",
                ));
            }
            let record: Value = serde_json::from_slice(&fs::read(entry.path())?)?;
            if record.get("type").and_then(Value::as_str) == Some("codex") {
                fs::remove_file(entry.path())?;
            }
        }
    }
    // Acknowledged deletion and stopped disk removal finish before allowing native mutation.
    drop(snapshot);
    drop(attached);
    Ok(())
}
