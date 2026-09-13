//! Diagnoses whether Codex update paths target the running installation.
//!
//! Update diagnostics combine cached fork release status and install-channel hints.
//! It never executes package managers or other helpers selected by PATH.

use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::time::Duration;
use std::time::SystemTime;

use codex_core::config::Config;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use codex_http_client::ClientRouteClass;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use codex_http_client::RouteAwareClientPool;
use codex_install_context::InstallContext;
use codex_install_context::codex_plus_plus::FORK_RELEASE_STATUS_MAX_AGE;
use codex_install_context::codex_plus_plus::UpdateChannel;
use codex_install_context::codex_plus_plus::UpdatePlan;
use codex_install_context::codex_plus_plus::is_newer;
use codex_tui::UpdateAction;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use http::Method;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use serde::Deserialize;
#[cfg(target_os = "macos")]
use url::Url;

use super::CheckStatus;
use super::DoctorCheck;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::DoctorIssue;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::desktop::platform::InstalledApp;
use super::doctor_install_context;
use super::doctor_managed_by_npm;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use super::network;

#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const DESKTOP_UPDATE_URL: &str = "https://persistent.oaistatic.com/codex-app-prod/appcast-x64.xml";
#[cfg(all(target_os = "macos", not(target_arch = "x86_64")))]
const DESKTOP_UPDATE_URL: &str = "https://persistent.oaistatic.com/codex-app-prod/appcast.xml";
#[cfg(target_os = "macos")]
const BACKEND_DESKTOP_UPDATE_URL: &str = "https://chatgpt.com/backend-api/wham/app/appcast";
#[cfg(target_os = "windows")]
const DESKTOP_UPDATE_URL: &str =
    "https://persistent.oaistatic.com/codex-app-prod/windows-store-update.json";

/// Builds the update-health row for the current installation.
///
/// Missing or stale release status degrades the row to a warning instead of
/// masking more direct install or configuration failures.
pub(super) async fn updates_check(config: &Config) -> DoctorCheck {
    let current_exe = std::env::current_exe().ok();
    let install_context = doctor_install_context(current_exe.as_deref());
    let mut details = vec![
        format!(
            "check for update on startup: {}",
            config.check_for_update_on_startup
        ),
        format!("update action: {}", update_action_label(&install_context)),
    ];
    let mut status = CheckStatus::Ok;
    let mut summary = "update configuration is locally consistent".to_string();
    if push_cached_version_details(&mut details, config.codex_home.as_path()) {
        status = CheckStatus::Warning;
        summary = "fork release status cache is stale or unavailable".to_string();
    }

    if doctor_managed_by_npm(current_exe.as_deref()) {
        details
            .push("npm update target: not inspected (PATH helpers are not executed)".to_string());
    }

    DoctorCheck::new("updates.status", "updates", status, summary).details(details)
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(super) async fn append_desktop_update(
    checks: &mut [DoctorCheck],
    config: Option<&Config>,
    application: &InstalledApp,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    #[cfg(target_os = "macos")]
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from)
        && let Some(build) = latest_macos_staged_build(
            &home
                .join("Library/Caches")
                .join(application.identity)
                .join("org.sparkle-project.Sparkle/Installation"),
            application.build,
        )
        .await
        && let Some(update) = checks.iter_mut().find(|check| check.id == "updates.status")
    {
        update.details.extend([
            "desktop update status: ready to install".to_string(),
            format!("desktop latest build: {build}"),
            format!("desktop application: {}", application.identity),
        ]);
    }

    let Some(config) = config else {
        return;
    };
    let Some(reachability_index) = checks
        .iter()
        .position(|check| check.id == "network.provider_reachability")
    else {
        return;
    };
    #[cfg(target_os = "macos")]
    let desktop_update_url = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| {
            macos_desktop_update_url(&home, application, &os_info::get().version().to_string())
        })
        .unwrap_or_else(|| DESKTOP_UPDATE_URL.to_string());
    #[cfg(target_os = "windows")]
    let desktop_update_url = DESKTOP_UPDATE_URL;
    #[cfg(target_os = "macos")]
    let desktop_update_url = desktop_update_url.as_str();
    let desktop_update_display_url = desktop_update_url
        .split_once('?')
        .map_or(desktop_update_url, |(endpoint, _)| endpoint);
    let client = RouteAwareClientPool::new_without_request_logging(
        config.http_client_factory(),
        ClientRouteClass::Other,
    );
    let outcome = match client
        .request(Method::GET, desktop_update_url)
        .timeout(deadline.saturating_duration_since(tokio::time::Instant::now()))
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status().as_u16();
            #[cfg(target_os = "windows")]
            if status == 404 && response.url().scheme() == "https" {
                checks[reachability_index].details.push(format!(
                    "desktop assets CDN: {desktop_update_display_url} reachable (HTTP 404; no update available)"
                ));
                return;
            }
            if cfg!(target_os = "windows") && response.url().scheme() != "https" {
                Err("update manifest redirected to a non-HTTPS URL".to_string())
            } else if status == 407 {
                Err("proxy authentication required (HTTP 407)".to_string())
            } else if !(200..=299).contains(&status) {
                Err(format!("HTTP {status}"))
            } else {
                checks[reachability_index].details.push(format!(
                    "desktop assets CDN: {desktop_update_display_url} reachable (HTTP {status})"
                ));
                #[cfg(target_os = "windows")]
                if let Some(update) = checks.iter_mut().find(|check| check.id == "updates.status") {
                    match response.bytes().await {
                        Ok(body) => match windows_store_update(&body, &application.version) {
                            Ok(Some(build)) => update.details.extend([
                                "desktop update status: available".to_string(),
                                format!("desktop latest build: {build}"),
                                format!("desktop application: {}", application.identity),
                            ]),
                            Ok(None) => {}
                            Err(error) => {
                                update.status = update.status.max(CheckStatus::Warning);
                                update
                                    .details
                                    .push(format!("desktop update manifest: {error}"));
                            }
                        },
                        Err(_) => {
                            update.status = update.status.max(CheckStatus::Warning);
                            update
                                .details
                                .push("desktop update manifest: response could not be read".into());
                        }
                    }
                }
                Ok(())
            }
        }
        Err(error) => Err(network::request_error(error)),
    };

    if let Err(error) = outcome {
        let reachability = &mut checks[reachability_index];
        reachability.details.push(format!(
            "desktop assets CDN: {desktop_update_display_url} {error} (optional)"
        ));
        if reachability.status == CheckStatus::Ok {
            reachability.status = CheckStatus::Warning;
            reachability.summary = "desktop update and runtime CDN is unreachable".to_string();
        }
        reachability.issues.push(
            DoctorIssue::new(
                CheckStatus::Warning,
                "desktop update and runtime CDN is unreachable",
            )
            .measured(format!("{desktop_update_display_url} {error}"))
            .expected("desktop update and runtime CDN reachable over HTTPS")
            .remedy(
                if desktop_update_display_url.starts_with("https://chatgpt.com/") {
                    "check proxy, firewall, DNS, and certificate access to chatgpt.com"
                } else {
                    "check proxy, firewall, DNS, and certificate access to persistent.oaistatic.com"
                },
            )
            .field("desktop assets CDN"),
        );
    }
}

#[cfg(target_os = "macos")]
fn macos_desktop_update_url(home: &Path, application: &InstalledApp, os_version: &str) -> String {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct ProductionAppcastState {
        #[serde(default)]
        backend_appcast_enabled: bool,
        installation_id: Option<String>,
    }

    let state_path = home
        .join("Library/Application Support")
        .join(application.identity)
        .join("production-appcast-bootstrap.json");
    let Some(state) = std::fs::read(state_path)
        .ok()
        .and_then(|contents| serde_json::from_slice::<ProductionAppcastState>(&contents).ok())
    else {
        return DESKTOP_UPDATE_URL.to_string();
    };
    let Some(installation_id) = state
        .backend_appcast_enabled
        .then_some(state.installation_id)
        .flatten()
    else {
        return DESKTOP_UPDATE_URL.to_string();
    };

    let Ok(mut url) = Url::parse(BACKEND_DESKTOP_UPDATE_URL) else {
        return DESKTOP_UPDATE_URL.to_string();
    };
    url.query_pairs_mut().extend_pairs([
        ("installation_id", installation_id.as_str()),
        (
            "arch",
            if cfg!(target_arch = "x86_64") {
                "x64"
            } else {
                "arm64"
            },
        ),
        ("app_version", application.version.as_str()),
        ("beta", "false"),
        ("os-version", os_version),
        ("plan_type", "unknown"),
    ]);
    url.to_string()
}

#[cfg(any(target_os = "windows", test))]
fn windows_store_update(
    manifest: &[u8],
    installed_version: &str,
) -> Result<Option<String>, &'static str> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct StoreManifest {
        schema_version: u64,
        build_version: String,
        store_product_id: String,
        package_identity: String,
    }

    let manifest: StoreManifest =
        serde_json::from_slice(manifest).map_err(|_| "invalid Windows Store update manifest")?;
    if manifest.schema_version == 0
        || manifest.store_product_id != "9PLM9XGG6VKS"
        || manifest.package_identity != "OpenAI.Codex"
    {
        return Err("Windows Store update manifest does not target the production application");
    }
    let version = |value: &str| -> Option<[u64; 4]> {
        value
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?
            .try_into()
            .ok()
    };
    let latest = version(&manifest.build_version)
        .ok_or("Windows Store update manifest contains an invalid build version")?;
    let installed =
        version(installed_version).ok_or("installed Windows application has an invalid version")?;
    Ok((latest > installed).then_some(manifest.build_version))
}

#[cfg(target_os = "macos")]
async fn latest_macos_staged_build(root: &Path, installed_build: u64) -> Option<u64> {
    const MAX_STAGED_BUNDLES: usize = 64;

    if !std::fs::symlink_metadata(root).ok()?.is_dir() {
        return None;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    let mut inspected = 0;
    let mut latest = None;
    for entry in std::fs::read_dir(root).ok()? {
        if inspected == MAX_STAGED_BUNDLES || tokio::time::Instant::now() >= deadline {
            break;
        }
        let Ok(entry) = entry else {
            continue;
        };
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let extracted = entry.path().join("extracted");
        if !std::fs::symlink_metadata(&extracted).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        let bundle = extracted.join("ChatGPT.app");
        if !std::fs::symlink_metadata(&bundle).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        inspected += 1;
        let Ok(result) = tokio::time::timeout_at(
            deadline,
            super::desktop::platform::inspect_macos_bundle(&bundle),
        )
        .await
        else {
            break;
        };
        if let Ok(Some(application)) = result
            && application.build > installed_build
        {
            latest = Some(latest.map_or(application.build, |latest: u64| {
                latest.max(application.build)
            }));
        }
    }
    latest
}

fn push_cached_version_details(details: &mut Vec<String>, codex_home: &Path) -> bool {
    let info = UpdateAction::read_cached_fork_release_status(codex_home, env!("CARGO_PKG_VERSION"));
    if let Some(latest) = info.latest_fork_version.as_deref() {
        details.push(format!("cached latest version: {latest}"));
        let availability = if is_newer(latest, env!("CARGO_PKG_VERSION")) == Some(true) {
            "newer version is available"
        } else {
            "current version is not older"
        };
        details.push(format!("latest version status: {availability}"));
    }
    if let Some(dismissed) = info.dismissed_version.as_deref() {
        details.push(format!("dismissed version: {dismissed}"));
    }
    info.is_stale(SystemTime::now(), FORK_RELEASE_STATUS_MAX_AGE)
}

fn update_action_label(context: &InstallContext) -> String {
    let plan = UpdatePlan::for_install_context(context, UpdateChannel::CodexPlusPlus);
    UpdateAction::from_update_plan(plan)
        .map(UpdateAction::command_str)
        .unwrap_or_else(|| format!("manual: {}", plan.channel().install_url()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_install_context::InstallMethod;
    use pretty_assertions::assert_eq;

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_update_probe_uses_the_persisted_production_appcast_feed() {
        let home = tempfile::tempdir().expect("temporary home should be created");
        let application = InstalledApp {
            identity: "com.openai.codex",
            version: "26.623.10000".to_string(),
            bundle: PathBuf::new(),
            build: 6139,
        };
        assert_eq!(
            macos_desktop_update_url(home.path(), &application, "26.6.0"),
            DESKTOP_UPDATE_URL
        );

        let state_directory = home
            .path()
            .join("Library/Application Support/com.openai.codex");
        std::fs::create_dir_all(&state_directory)
            .expect("production appcast state directory should be created");
        std::fs::write(
            state_directory.join("production-appcast-bootstrap.json"),
            r#"{"backendAppcastEnabled":true,"installationId":"028e90f8-5f2a-47db-a05c-6a48f548d728"}"#,
        )
        .expect("production appcast state should be created");

        let arch = if cfg!(target_arch = "x86_64") {
            "x64"
        } else {
            "arm64"
        };
        assert_eq!(
            macos_desktop_update_url(home.path(), &application, "26.6.0"),
            format!(
                "{BACKEND_DESKTOP_UPDATE_URL}?installation_id=028e90f8-5f2a-47db-a05c-6a48f548d728&arch={arch}&app_version=26.623.10000&beta=false&os-version=26.6.0&plan_type=unknown"
            )
        );
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn macos_staged_updates_require_a_newer_matching_extracted_bundle() {
        let root = tempfile::tempdir().expect("temporary Sparkle cache should be created");
        for index in 0..320 {
            std::fs::create_dir(root.path().join(format!("unrelated-{index}")))
                .expect("unrelated Sparkle cache directory should be created");
        }
        for (name, identity, build) in [
            ("newest", "com.openai.codex", "6268"),
            ("newer", "com.openai.codex", "6168"),
            ("older", "com.openai.codex", "6138"),
            ("different", "com.example.other", "9999"),
            ("invalid", "com.openai.codex", "invalid"),
        ] {
            let bundle = root.path().join(name).join("extracted/ChatGPT.app");
            write_macos_bundle(&bundle, identity, build);
        }
        let outside = tempfile::tempdir().expect("external fixture should be created");
        let linked = outside.path().join("ChatGPT.app");
        write_macos_bundle(&linked, "com.openai.codex", "9999");
        std::os::unix::fs::symlink(&linked, root.path().join("ChatGPT.app"))
            .expect("symlinked staged app fixture should be created");

        assert_eq!(
            latest_macos_staged_build(root.path(), /*installed_build*/ 6139).await,
            Some(6268)
        );
        assert_eq!(
            latest_macos_staged_build(root.path(), /*installed_build*/ 6268).await,
            None
        );
    }

    #[test]
    fn windows_store_updates_compare_all_four_production_build_components() {
        let mut manifest = serde_json::json!({
            "schemaVersion": 1,
            "buildVersion": "26.803.5235.1",
            "storeProductId": "9PLM9XGG6VKS",
            "packageIdentity": "OpenAI.Codex",
        });
        assert_eq!(
            windows_store_update(&serde_json::to_vec(&manifest).unwrap(), "26.803.5235.0"),
            Ok(Some("26.803.5235.1".to_string()))
        );
        assert_eq!(
            windows_store_update(&serde_json::to_vec(&manifest).unwrap(), "26.803.5235.1"),
            Ok(None)
        );
        manifest["storeProductId"] = "other".into();
        assert!(
            windows_store_update(&serde_json::to_vec(&manifest).unwrap(), "26.803.5235.0").is_err()
        );
    }

    #[cfg(target_os = "macos")]
    fn write_macos_bundle(path: &Path, identity: &str, build: &str) {
        let contents = path.join("Contents");
        std::fs::create_dir_all(&contents).expect("staged app fixture should be created");
        std::fs::write(
            contents.join("Info.plist"),
            format!(
                "<?xml version=\"1.0\"?><plist version=\"1.0\"><dict>\
                 <key>CFBundleIdentifier</key><string>{identity}</string>\
                 <key>CFBundleVersion</key><string>{build}</string>\
                 </dict></plist>"
            ),
        )
        .expect("staged app metadata should be created");
    }

    #[test]
    fn update_action_labels_use_the_planned_distribution() {
        assert_eq!(
            update_action_label(&InstallContext {
                method: InstallMethod::Npm,
                package_layout: None,
            }),
            "npm install -g @jjliebig/codex-plus-plus"
        );
        assert_eq!(
            update_action_label(&InstallContext {
                method: InstallMethod::Pnpm,
                package_layout: None,
            }),
            "pnpm add -g @jjliebig/codex-plus-plus"
        );
        assert_eq!(
            update_action_label(&InstallContext {
                method: InstallMethod::Other,
                package_layout: None,
            }),
            "manual: https://github.com/JJLiebig/codex#install-a-release"
        );
    }

    #[test]
    fn update_details_use_the_shared_fork_release_cache() {
        let codex_home = tempfile::tempdir().expect("temp codex home");
        let cache_dir = codex_home.path().join("codex-plus-plus");
        std::fs::create_dir_all(&cache_dir).expect("create cache directory");
        let checked_at = SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_secs();
        std::fs::write(
            cache_dir.join("release-status.json"),
            serde_json::json!({
                "installed_fork_version": "0.144.4-fork.1",
                "latest_fork_version": "0.144.4-fork.2",
                "latest_stable_upstream_version": "0.147.0",
                "checked_at": {
                    "secs_since_epoch": checked_at,
                    "nanos_since_epoch": 0,
                },
                "dismissed_version": null,
            })
            .to_string(),
        )
        .expect("write release cache");
        let mut details = Vec::new();

        assert!(!push_cached_version_details(
            &mut details,
            codex_home.path()
        ));
        assert_eq!(
            details,
            vec![
                "cached latest version: 0.144.4-fork.2".to_string(),
                "latest version status: current version is not older".to_string(),
            ]
        );
    }
}
