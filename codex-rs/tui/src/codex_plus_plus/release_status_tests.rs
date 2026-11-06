use super::*;
use codex_install_context::codex_plus_plus::FORK_RELEASE_STATUS_MAX_AGE;
use pretty_assertions::assert_eq;
use std::time::Duration;
use std::time::UNIX_EPOCH;
use tempfile::tempdir;

#[tokio::test]
async fn published_probe_tracks_the_installable_channel() {
    use codex_install_context::InstallContext;
    use codex_install_context::InstallMethod;
    use codex_install_context::StandalonePlatform;
    use codex_install_context::codex_plus_plus::UpdateChannel;
    use codex_install_context::codex_plus_plus::UpdatePlan;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::path;

    let server = MockServer::start().await;
    let directory = tempdir().unwrap();
    let standalone = UpdatePlan::for_install_context(
        &InstallContext {
            method: InstallMethod::Standalone {
                release_dir: AbsolutePathBuf::from_absolute_path(directory.path()).unwrap(),
                resources_dir: None,
                platform: if cfg!(windows) {
                    StandalonePlatform::Windows
                } else {
                    StandalonePlatform::Unix
                },
            },
            package_layout: None,
        },
        UpdateChannel::CodexPlusPlus,
    );
    let Some(target) = standalone.standalone_release_target() else {
        return;
    };
    let tag = standalone
        .fork_npm_version_url()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap();
    let npm = UpdatePlan::for_install_context(
        &InstallContext {
            method: InstallMethod::Npm,
            package_layout: None,
        },
        UpdateChannel::CodexPlusPlus,
    );
    for (plan, patch, github_available, assets_available, expected) in [
        (standalone, 3, false, false, "1.2.3-fork.1"),
        (standalone, 3, true, false, "1.2.3-fork.1"),
        (standalone, 3, true, true, "1.2.4-fork.1"),
        (standalone, 5, true, true, "1.2.5-fork.1"),
        (npm, 2, true, true, "1.2.2-fork.1"),
    ] {
        let mut npm_version = format!("1.2.{patch}-fork.1");
        if plan == standalone {
            npm_version.push_str(&format!("-{tag}"));
        }
        server.reset().await;
        Mock::given(path(format!("/{tag}"))).respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "version": npm_version, "dist": {"tarball": "https://example.test/pkg.tgz", "integrity": "sha512-value"}
        }))).mount(&server).await;
        let suffix = if target.contains("windows") {
            "zip"
        } else {
            "tar.gz"
        };
        let archive = format!("codex-plus-plus-1.2.4-fork.1-{target}.{suffix}");
        let assets = if assets_available {
            vec![
                serde_json::json!({"name": archive}),
                serde_json::json!({"name": format!("{archive}.sha256")}),
            ]
        } else {
            vec![]
        };
        Mock::given(path("/github")).respond_with(ResponseTemplate::new(if github_available {200} else {503}).set_body_json(serde_json::json!({"tag_name": "codex-plus-plus-v1.2.4-fork.1", "assets": assets}))).mount(&server).await;
        assert_eq!(
            probe::fetch_published_version(
                plan,
                &format!("{}/{tag}", server.uri()),
                &format!("{}/github", server.uri())
            )
            .await
            .unwrap(),
            expected
        );
    }
}

#[tokio::test]
async fn cache_round_trips_fresh_and_stale_status() {
    let codex_home = tempdir().expect("temp codex home");
    let path = release_status_filepath(codex_home.path());
    let checked_at = UNIX_EPOCH + Duration::from_secs(10_000);
    let mut expected = ForkReleaseStatus::new(
        "0.144.4-fork.2".to_string(),
        Some("0.144.4-fork.2".to_string()),
        Some("0.147.1".to_string()),
        checked_at,
    );
    expected.dismissed_version = Some("0.144.4-fork.1".to_string());

    write_release_status(&path, &expected)
        .await
        .expect("write release status");
    let actual = read_release_status(&path).expect("read release status");

    assert_eq!(actual, expected);
    assert!(!actual.is_stale(
        checked_at + FORK_RELEASE_STATUS_MAX_AGE,
        FORK_RELEASE_STATUS_MAX_AGE
    ));
    assert!(actual.is_stale(
        checked_at + FORK_RELEASE_STATUS_MAX_AGE + Duration::from_secs(1),
        FORK_RELEASE_STATUS_MAX_AGE
    ));
}

#[tokio::test]
async fn refresh_recovers_corrupt_cache_and_offline_preserves_it() {
    let codex_home = tempdir().expect("temp codex home");
    let path = release_status_filepath(codex_home.path());
    tokio::fs::create_dir_all(path.parent().expect("cache parent"))
        .await
        .expect("create cache parent");
    tokio::fs::write(&path, "not json")
        .await
        .expect("write corrupt cache");

    probe::refresh_with_probes(
        &path,
        "0.144.4-fork.1",
        async { Ok("0.144.4-fork.2".to_string()) },
        async { Ok("0.147.0".to_string()) },
    )
    .await
    .expect("repair cache");
    read_release_status(&path).expect("read repaired cache");

    probe::refresh_with_probes(
        &path,
        "0.144.4-fork.1",
        async { Ok("0.144.4-fork.3".to_string()) },
        async { Err(anyhow::anyhow!("upstream offline")) },
    )
    .await
    .expect("fork refresh should survive upstream failure");
    let partial = read_release_status(&path).expect("read partial refresh");
    assert_eq!(
        (
            partial.latest_fork_version.as_deref(),
            partial.latest_stable_upstream_version.as_deref()
        ),
        (Some("0.144.4-fork.3"), Some("0.147.0"))
    );

    probe::refresh_with_probes(
        &path,
        "0.144.4-fork.1",
        async { Ok("0.144.4-fork.4".to_string()) },
        async { Ok("0.148.0-beta.1".to_string()) },
    )
    .await
    .expect("fork refresh should ignore upstream prerelease");
    let prerelease = read_release_status(&path).expect("read prerelease refresh");
    assert_eq!(
        (
            prerelease.latest_fork_version.as_deref(),
            prerelease.latest_stable_upstream_version.as_deref()
        ),
        (Some("0.144.4-fork.4"), Some("0.147.0"))
    );

    let error = probe::refresh_with_probes(
        &path,
        "0.144.4-fork.1",
        async { Err(anyhow::anyhow!("fork offline")) },
        async { Ok("0.148.0".to_string()) },
    )
    .await
    .expect_err("fork failure should leave cache unchanged");
    assert_eq!(error.to_string(), "fork offline");
    assert_eq!(
        read_release_status(&path).expect("read stale cache"),
        prerelease
    );
}
