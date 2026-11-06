use crate::UpdateAction;
use codex_install_context::codex_plus_plus::ForkReleaseStatus;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

const CACHE_PATH: &str = "codex-plus-plus/release-status.json";
const MAX_CACHE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize, Serialize)]
struct CachedReleaseStatus {
    installed_fork_version: String,
    latest_fork_version: Option<String>,
    latest_stable_upstream_version: Option<String>,
    checked_at: SystemTime,
    dismissed_version: Option<String>,
}

pub(crate) fn release_status_filepath(codex_home: &Path) -> PathBuf {
    codex_home.join(CACHE_PATH)
}

pub(crate) fn read_release_status(path: &Path) -> anyhow::Result<ForkReleaseStatus> {
    if std::fs::metadata(path)?.len() > MAX_CACHE_BYTES {
        anyhow::bail!("release status cache exceeds {MAX_CACHE_BYTES} bytes");
    }
    let cached: CachedReleaseStatus = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    let mut status = ForkReleaseStatus::new(
        cached.installed_fork_version,
        cached.latest_fork_version,
        cached.latest_stable_upstream_version,
        cached.checked_at,
    );
    status.dismissed_version = cached.dismissed_version;
    Ok(status)
}

#[cfg(any(not(debug_assertions), test))]
pub(crate) async fn dismiss_version(
    path: &Path,
    installed_version: &str,
    version: &str,
) -> anyhow::Result<()> {
    let mut status = read_release_status(path).unwrap_or_else(|_| {
        ForkReleaseStatus::new(
            installed_version.to_string(),
            Some(version.to_string()),
            None,
            std::time::UNIX_EPOCH,
        )
    });
    status.dismissed_version = Some(version.to_string());
    write_release_status(path, &status).await
}

#[cfg(any(not(debug_assertions), test))]
async fn write_release_status(path: &Path, status: &ForkReleaseStatus) -> anyhow::Result<()> {
    let cached = CachedReleaseStatus {
        installed_fork_version: status.installed_fork_version.clone(),
        latest_fork_version: status.latest_fork_version.clone(),
        latest_stable_upstream_version: status.latest_stable_upstream_version.clone(),
        checked_at: status.checked_at,
        dismissed_version: status.dismissed_version.clone(),
    };
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, format!("{}\n", serde_json::to_string(&cached)?)).await?;
    Ok(())
}

impl UpdateAction {
    pub fn read_cached_fork_release_status(
        codex_home: &Path,
        installed_version: &str,
    ) -> ForkReleaseStatus {
        read_release_status(&release_status_filepath(codex_home))
            .map(|status| status.with_installed_fork_version(installed_version.to_string()))
            .unwrap_or_else(|_| ForkReleaseStatus::unavailable(installed_version.to_string()))
    }
}

#[cfg(any(not(debug_assertions), test))]
#[cfg_attr(test, allow(dead_code))]
mod probe {
    use super::*;
    use crate::update_versions::extract_version_from_latest_tag;
    use codex_install_context::codex_plus_plus::LatestVersionSource;
    use codex_install_context::codex_plus_plus::UpdatePlan;
    use codex_login::default_client::create_client;
    use serde::de::DeserializeOwned;
    use std::future::Future;
    use std::time::Duration;

    const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;
    const TIMEOUT: Duration = Duration::from_secs(5);

    enum Kind {
        Fork,
        StableUpstream,
    }

    #[derive(Deserialize)]
    struct ReleaseInfo {
        tag_name: String,
        #[serde(default)]
        assets: Vec<ReleaseAsset>,
    }

    #[derive(Deserialize)]
    struct ReleaseAsset {
        name: String,
    }

    #[derive(Deserialize)]
    struct NpmVersion {
        version: String,
        dist: Option<NpmDist>,
    }

    #[derive(Deserialize)]
    struct NpmDist {
        tarball: Option<String>,
        integrity: Option<String>,
    }

    #[derive(Deserialize)]
    struct HomebrewCaskInfo {
        version: String,
    }

    pub(crate) async fn refresh_release_status(
        path: &Path,
        installed_version: &str,
        fork_plan: UpdatePlan,
        upstream_plan: UpdatePlan,
    ) -> anyhow::Result<()> {
        refresh_with_probes(
            path,
            installed_version,
            fetch_latest_version(fork_plan, Kind::Fork),
            fetch_latest_version(upstream_plan, Kind::StableUpstream),
        )
        .await
    }

    pub(super) async fn refresh_with_probes(
        path: &Path,
        installed_version: &str,
        fork_probe: impl Future<Output = anyhow::Result<String>>,
        upstream_probe: impl Future<Output = anyhow::Result<String>>,
    ) -> anyhow::Result<()> {
        let (latest_fork, latest_upstream) = tokio::join!(fork_probe, upstream_probe);
        let previous = read_release_status(path).ok();
        let latest_upstream = latest_upstream
            .ok()
            .filter(|version| ForkReleaseStatus::is_stable_upstream(version))
            .or_else(|| previous.as_ref()?.latest_stable_upstream_version.clone());
        let dismissed_version = previous.and_then(|status| status.dismissed_version);
        let mut status = ForkReleaseStatus::new(
            installed_version.to_string(),
            Some(latest_fork?),
            latest_upstream,
            SystemTime::now(),
        );
        status.dismissed_version = dismissed_version;
        write_release_status(path, &status).await
    }

    async fn fetch_latest_version(plan: UpdatePlan, kind: Kind) -> anyhow::Result<String> {
        if matches!(kind, Kind::Fork)
            && let Some(url) = plan.fork_npm_version_url()
        {
            let LatestVersionSource::GitHub { api_url, .. } = plan.latest_version_source() else {
                unreachable!("fork npm releases use GitHub fallback");
            };
            return fetch_published_version(plan, url, api_url).await;
        }
        match plan.latest_version_source() {
            LatestVersionSource::Homebrew { api_url } => {
                Ok(fetch_json::<HomebrewCaskInfo>(api_url).await?.version)
            }
            LatestVersionSource::GitHub {
                api_url,
                tag_prefix,
                npm_registry_url: _,
            } => {
                let release = fetch_json::<ReleaseInfo>(api_url).await?;
                let version = extract_version_from_latest_tag(tag_prefix, &release.tag_name)?;
                Ok(version)
            }
        }
    }

    pub(super) async fn fetch_published_version(
        plan: UpdatePlan,
        url: &str,
        github_url: &str,
    ) -> anyhow::Result<String> {
        let npm = fetch_json::<NpmVersion>(url).await.ok().and_then(|info| {
            let dist = info.dist?;
            if dist.tarball.as_deref().is_none_or(str::is_empty)
                || dist.integrity.as_deref().is_none_or(str::is_empty)
            {
                return None;
            }
            let version = if plan.standalone_release_target().is_some() {
                info.version
                    .strip_suffix(&format!("-{}", url.rsplit('/').next()?))?
                    .to_string()
            } else {
                info.version
            };
            codex_install_context::codex_plus_plus::is_newer(&version, "0.0.0-fork.0")?;
            Some(version)
        });
        let Some(target) = plan.standalone_release_target() else {
            return npm.ok_or_else(|| anyhow::anyhow!("npm release unavailable"));
        };
        let LatestVersionSource::GitHub { tag_prefix, .. } = plan.latest_version_source() else {
            unreachable!("standalone releases use GitHub fallback");
        };
        let github = fetch_json::<ReleaseInfo>(github_url)
            .await
            .ok()
            .and_then(|release| {
                let version =
                    extract_version_from_latest_tag(tag_prefix, &release.tag_name).ok()?;
                let suffix = if target.contains("windows") {
                    "zip"
                } else {
                    "tar.gz"
                };
                let archive = format!("codex-plus-plus-{version}-{target}.{suffix}");
                [archive.clone(), format!("{archive}.sha256")]
                    .iter()
                    .all(|name| release.assets.iter().any(|asset| &asset.name == name))
                    .then_some(version)
            });
        let newest = match (npm, github) {
            (Some(npm), Some(github)) => {
                if codex_install_context::codex_plus_plus::is_newer(&github, &npm) == Some(true) {
                    Some(github)
                } else {
                    Some(npm)
                }
            }
            (npm, github) => npm.or(github),
        };
        newest.ok_or_else(|| anyhow::anyhow!("published release unavailable"))
    }

    async fn fetch_json<T: DeserializeOwned>(url: &str) -> anyhow::Result<T> {
        let mut response = create_client()
            .get(url)
            .timeout(TIMEOUT)
            .send()
            .await?
            .error_for_status()?;
        if response.content_length().unwrap_or_default() > MAX_BODY_BYTES {
            anyhow::bail!("release probe from {url} exceeds {MAX_BODY_BYTES} bytes");
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if body.len() as u64 + chunk.len() as u64 > MAX_BODY_BYTES {
                anyhow::bail!("release probe from {url} exceeds {MAX_BODY_BYTES} bytes");
            }
            body.extend_from_slice(&chunk);
        }
        Ok(serde_json::from_slice(&body)?)
    }
}

#[cfg(not(debug_assertions))]
pub(crate) use probe::refresh_release_status;

#[cfg(test)]
#[path = "release_status_tests.rs"]
mod tests;
