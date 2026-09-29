use super::version_for_install;
use codex_install_context::InstallContext;
use codex_install_context::codex_plus_plus::is_newer;
use pretty_assertions::assert_eq;
use std::fs;

#[test]
fn packaged_version_preserves_fork_update_comparisons() {
    let package = tempfile::tempdir().expect("package directory");
    let bin = package.path().join("bin");
    fs::create_dir(&bin).expect("bin directory");
    let executable = bin.join(if cfg!(windows) { "codex.exe" } else { "codex" });
    fs::write(&executable, b"").expect("executable fixture");
    fs::write(
        package.path().join("codex-package.json"),
        r#"{"version":"0.153.3-fork.1"}"#,
    )
    .expect("package manifest");
    let context = InstallContext::from_exe(
        cfg!(target_os = "macos"),
        Some(&executable),
        /*method_override*/ None,
    );
    let version = version_for_install(&context);
    assert_eq!(version, "0.153.3-fork.1");
    assert_eq!(is_newer("0.153.3-fork.2", &version), Some(true));
    assert_eq!(is_newer("0.153.3-fork.1", &version), Some(false));

    let header = crate::history_cell::SessionHeaderHistoryCell::new(
        "test-model".to_string(),
        /*reasoning_effort*/ None,
        std::path::PathBuf::from("."),
        Box::leak(version.into_boxed_str()),
    );
    use crate::history_cell::HistoryCell;
    let lines = header.display_lines(80);
    let rendered = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("packaged_fork_version_header", rendered);
}

#[test]
fn unpackaged_build_keeps_the_crate_version() {
    let context = InstallContext::from_exe(
        cfg!(target_os = "macos"),
        /*current_exe*/ None,
        /*method_override*/ None,
    );
    assert_eq!(version_for_install(&context), env!("CARGO_PKG_VERSION"));
}
