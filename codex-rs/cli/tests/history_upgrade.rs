#![cfg(windows)]

use anyhow::Context;
use anyhow::Result;
use pretty_assertions::assert_eq;
use std::fs;
use std::path::PathBuf;

#[test]
fn upgrade_target_respects_configured_storage_without_opening_it() -> Result<()> {
    let scratch = tempfile::tempdir()?;
    let home = scratch.path().join("home");
    let state = home.join("state");
    fs::create_dir_all(&state)?;
    fs::write(home.join("config.toml"), "sqlite_home = 'state'\n")?;
    let output = assert_cmd::Command::new(codex_utils_cargo_bin::cargo_bin("codex")?)
        .env("CODEX_HOME", &home)
        .env("CODEX_SQLITE_HOME", scratch.path().join("unused-env"))
        .current_dir(scratch.path())
        .args(["--no-daemon", "debug", "history-upgrade-target"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = PathBuf::from(String::from_utf8(output.stdout)?.trim());
    assert_eq!(
        path.parent().context("history parent")?.canonicalize()?,
        state.canonicalize()?
    );
    assert_eq!(
        path.file_name().and_then(|name| name.to_str()),
        Some("thread_history_1.sqlite")
    );
    assert!(!path.exists());
    assert!(!home.join("auth.json").exists());
    Ok(())
}
