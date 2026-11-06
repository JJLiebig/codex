use codex_install_context::StandalonePlatform;
use std::process::ExitStatus;

/// Materialize the shipped downloader and activator so updates need no network bootstrap.
pub(crate) fn run(platform: StandalonePlatform) -> anyhow::Result<ExitStatus> {
    let temporary = tempfile::tempdir()?;
    let (suffix, latest, installer) = match platform {
        StandalonePlatform::Unix => (
            "sh",
            include_str!("../../../../scripts/install/install-codex-plus-plus-latest.sh"),
            include_str!("../../../../scripts/install/install-codex-plus-plus.sh"),
        ),
        StandalonePlatform::Windows => (
            "ps1",
            include_str!("../../../../scripts/install/install-codex-plus-plus-latest.ps1"),
            include_str!("../../../../scripts/install/install-codex-plus-plus.ps1"),
        ),
    };
    let downloader = temporary.path().join(format!("latest.{suffix}"));
    let activator = temporary.path().join(format!("install.{suffix}"));
    std::fs::write(&downloader, latest)?;
    std::fs::write(&activator, installer)?;
    let shim = std::env::var_os("CODEX_PLUS_PLUS_SHIM_DIR")
        .ok_or_else(|| anyhow::anyhow!("Codex++ shim provenance is missing"))?;
    let mut command = match platform {
        StandalonePlatform::Unix => {
            let mut command = std::process::Command::new("sh");
            command.arg(&downloader).arg("--shim-dir").arg(shim);
            command
        }
        StandalonePlatform::Windows => {
            #[cfg(windows)]
            let executable = crate::resolve_windows_update_command_from_path(
                "powershell.exe",
                &std::env::var_os("PATH").unwrap_or_default(),
            )?;
            #[cfg(not(windows))]
            let executable = "powershell.exe";
            let mut command = std::process::Command::new(executable);
            command
                .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
                .arg(&downloader)
                .arg("-ShimDir")
                .arg(shim);
            command
        }
    };
    if let Some(manifest) = codex_install_context::InstallContext::current().package_manifest() {
        command.env(
            "CODEX_PLUS_PLUS_CURRENT_VERSION",
            manifest.version.to_string(),
        );
    }
    Ok(command
        .env("CODEX_PLUS_PLUS_INSTALLER_PATH", activator)
        .current_dir(temporary.path())
        .status()?)
}
