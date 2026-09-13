use super::*;
use crate::legacy_core::config::ConfigBuilder;
use color_eyre::Result;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::fs;
use tempfile::TempDir;
const UPDATED_VERSION: &str = "0.6.9-codexpp.2";

#[test]
fn release_metadata_accepts_only_eligible_owned_channel() {
    let releases = [
        json!({"tag_name": "v0.7.0", "draft": false, "prerelease": false}),
        json!({"tag_name": "v0.6.10-codexpp.1", "draft": true, "prerelease": false}),
        json!({"tag_name": "v0.6.10-codexpp.1", "draft": false, "prerelease": true}),
        json!({"tag_name": "v0.6.9-codexpp.1", "draft": false, "prerelease": false}),
    ];
    let expected = DcgTarget::from_tag("v0.6.9-codexpp.1");
    assert_eq!(DcgTarget::from_releases(&releases), expected);
}

#[tokio::test]
async fn managed_install_lifecycle_is_transactional_and_next_session_only() -> Result<()> {
    if !PLATFORM_SUPPORTED {
        return Ok(());
    }
    let (home, source) = (TempDir::new()?, TempDir::new()?);
    fs::write(
        home.path().join("config.toml"),
        "[features]\nplugins = true\n",
    )?;
    write_marketplace(source.path(), "0.6.8-codexpp.1")?;
    compile_fake_dcg(source.path())?;
    write_installer(source.path(), /*fail*/ true)?;
    let config = ConfigBuilder::default()
        .codex_home(home.path().to_path_buf())
        .build()
        .await?;
    let app_server = crate::start_embedded_app_server_for_picker(&config).await?;
    let mut manager = DcgManager::new(&app_server, &config).unwrap();
    manager.marketplace_source = fs::canonicalize(source.path())?.display().to_string();
    manager.target_override = DcgTarget::from_tag("v0.6.8-codexpp.1");
    manager.local_marketplace_target = manager.target_override.clone();
    assert!(manager.install_and_enable().await.is_err());
    assert!(fs::read_to_string(home.path().join("config.toml"))?.contains("enabled = false"));
    write_installer(source.path(), /*fail*/ false)?;
    let installed = manager.install_and_enable().await.unwrap();
    assert!(!installed.takes_effect_in_current_session);
    manager.disable().await.unwrap();
    manager.target_override = DcgTarget::from_tag("v0.6.9-codexpp.2");
    write_installer(source.path(), /*fail*/ true)?;
    assert!(manager.update().await.is_err());
    assert!(fs::read_to_string(home.path().join("config.toml"))?.contains("enabled = false"));
    write_marketplace(source.path(), UPDATED_VERSION)?;
    write_installer(source.path(), /*fail*/ false)?;
    let updated = manager.update().await.unwrap();
    let installed_tag =
        fs::read_to_string(manager.binary_path().with_file_name("installed-version"))?;
    let expected = DcgChange {
        status: DcgStatus::Disabled(UPDATED_VERSION.to_string()),
        takes_effect_in_current_session: false,
    };
    assert_eq!(
        (updated, installed_tag),
        (expected, format!("v{UPDATED_VERSION}"))
    );
    let config_before_remote_attempt = fs::read_to_string(home.path().join("config.toml"))?;
    manager.remote_hook_host = true;
    assert!(manager.disable().await.is_err());
    let config_after_remote_attempt = fs::read_to_string(home.path().join("config.toml"))?;
    assert_eq!(config_after_remote_attempt, config_before_remote_attempt);
    app_server.shutdown().await?;
    Ok(())
}

fn write_marketplace(root: &Path, version: &str) -> Result<()> {
    let plugin = root.join("plugins").join(PLUGIN_NAME);
    fs::create_dir_all(root.join(".agents/plugins"))?;
    fs::create_dir_all(plugin.join(".codex-plugin"))?;
    fs::create_dir_all(plugin.join("hooks"))?;
    fs::write(
        root.join(".agents/plugins/marketplace.json"),
        format!(
            r#"{{"name":"{MARKETPLACE_NAME}","plugins":[{{"name":"{PLUGIN_NAME}","source":{{"source":"local","path":"./plugins/{PLUGIN_NAME}"}}}}]}}"#
        ),
    )?;
    fs::write(
        plugin.join(".codex-plugin/plugin.json"),
        format!(r#"{{"name":"{PLUGIN_NAME}","version":"{version}","description":"test"}}"#),
    )?;
    fs::write(
        plugin.join("hooks/hooks.json"),
        r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"\"${PLUGIN_DATA}/dcg\"","commandWindows":"\"${PLUGIN_DATA}\\dcg.exe\""}]}]}}"#,
    )?;
    Ok(())
}

fn compile_fake_dcg(root: &Path) -> Result<()> {
    let source = root.join("fake-dcg.rs");
    fs::write(
        &source,
        r#"fn main() { let path = std::env::current_exe().unwrap().with_file_name("installed-version"); println!("dcg {}", std::fs::read_to_string(path).unwrap().trim_start_matches('v')); }"#,
    )?;
    let status = std::process::Command::new("rustc")
        .arg(source)
        .args(["-o"])
        .arg(root.join(format!("fake-dcg{}", std::env::consts::EXE_SUFFIX)))
        .status()?;
    assert!(status.success());
    Ok(())
}

#[cfg(target_os = "windows")]
fn write_installer(root: &Path, fail: bool) -> Result<()> {
    let body = if fail {
        r#"Param([string]$Owner,[string]$Repo,[string]$Version,[string]$Dest,[switch]$NoConfigure,[switch]$Verify); [IO.File]::WriteAllText((Join-Path $Dest 'dcg.exe'), 'broken'); exit 7"#
    } else {
        r#"Param([string]$Owner,[string]$Repo,[string]$Version,[string]$Dest,[switch]$NoConfigure,[switch]$Verify); if($Owner -ne 'JJLiebig' -or $Repo -ne 'destructive_command_guard' -or -not $NoConfigure -or -not $Verify){exit 8}; [IO.File]::WriteAllText((Join-Path $Dest 'installed-version'), $Version); Copy-Item (Join-Path $PSScriptRoot 'fake-dcg.exe') (Join-Path $Dest 'dcg.exe') -Force"#
    };
    fs::write(root.join("install.ps1"), body)?;
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn write_installer(root: &Path, fail: bool) -> Result<()> {
    let body = if fail {
        r#"#!/usr/bin/env bash
set -eu; while (($#)); do if [[ $1 == --dest ]]; then dest=$2; shift; fi; shift; done; printf broken > "$dest/dcg"; exit 7"#
    } else {
        r#"#!/usr/bin/env bash
set -eu; no_configure=0; verify=0; while (($#)); do case $1 in --version) version=$2; shift;; --dest) dest=$2; shift;; --no-configure) no_configure=1;; --verify) verify=1;; esac; shift; done; [[ $OWNER == JJLiebig && $REPO == destructive_command_guard && $no_configure == 1 && $verify == 1 ]]; printf %s "$version" > "$dest/installed-version"; cp "$(dirname "$0")/fake-dcg" "$dest/dcg"; chmod +x "$dest/dcg""#
    };
    fs::write(root.join("install.sh"), body)?;
    Ok(())
}
