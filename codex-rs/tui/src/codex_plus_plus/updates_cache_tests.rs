use super::*;
use crate::legacy_core::config::ConfigBuilder;
use pretty_assertions::assert_eq;
use tempfile::tempdir;

#[tokio::test]
async fn dismiss_version_creates_cache_file_when_missing() {
    let codex_home = tempdir().expect("temp codex home");
    let config = ConfigBuilder::default()
        .codex_home(codex_home.path().to_path_buf())
        .build()
        .await
        .expect("load config");
    let version_file = version_filepath(&config);
    assert_eq!(
        version_file,
        codex_home
            .path()
            .join("codex-plus-plus")
            .join("release-status.json")
    );

    dismiss_version(&config, "999.0.0")
        .await
        .expect("dismiss version");

    let info = read_version_info(&version_file).expect("read version info");
    assert_eq!(info.checked_at, std::time::UNIX_EPOCH);
    assert_eq!(
        (
            info.latest_fork_version.as_deref(),
            info.dismissed_version.as_deref()
        ),
        (Some("999.0.0"), Some("999.0.0"))
    );
}
