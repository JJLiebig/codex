use crate::legacy_core::config::Config;
use crate::version::codex_cli_version;
use codex_install_context::codex_plus_plus::ForkReleaseStatus;
use std::path::Path;
use std::path::PathBuf;

pub(crate) fn version_filepath(config: &Config) -> PathBuf {
    crate::codex_plus_plus::release_status_filepath(config.codex_home.as_path())
}

pub(crate) fn read_version_info(version_file: &Path) -> anyhow::Result<ForkReleaseStatus> {
    crate::codex_plus_plus::read_release_status(version_file)
}

/// Persist a dismissal for the current latest version so we don't show
/// the update popup again for this version.
pub(crate) async fn dismiss_version(config: &Config, version: &str) -> anyhow::Result<()> {
    let version_file = version_filepath(config);
    crate::codex_plus_plus::dismiss_version(&version_file, codex_cli_version(), version).await
}

#[cfg(test)]
#[path = "updates_cache_tests.rs"]
mod tests;
