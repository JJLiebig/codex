use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use codex_protocol::ThreadId;

use crate::Cli;
use crate::app_server_session::ResumeModelSettings;

static RECOVERING: OnceLock<()> = OnceLock::new();

pub fn apply_home() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new("--codex-resume-home")) {
        return Ok(());
    }
    let home = args
        .next()
        .filter(|home| Path::new(home).is_absolute())
        .ok_or_else(|| anyhow::anyhow!("Program resume requires an absolute Codex home"))?;
    anyhow::ensure!(
        args.next().as_deref() == Some(OsStr::new("resume"))
            && args
                .next()
                .and_then(|id| ThreadId::from_string(id.to_str()?).ok())
                .is_some(),
        "Program resume requires an exact thread ID"
    );
    // SAFETY: the CLI calls this before arg0 loads dotenv or starts Tokio.
    unsafe { std::env::set_var("CODEX_HOME", home) };
    RECOVERING.get_or_init(|| ());
    Ok(())
}

pub(crate) fn startup_model_settings(ordinary: ResumeModelSettings) -> ResumeModelSettings {
    if RECOVERING.get().is_some() {
        ResumeModelSettings::RestoreFromThread
    } else {
        ordinary
    }
}

pub(super) struct Launch {
    pub(super) sessions: PathBuf,
    pub(super) argv: Vec<String>,
    pub(super) options: Vec<String>,
}

impl Launch {
    pub(super) fn new(cli: &Cli) -> Option<Self> {
        if cli.bypass_hook_trust
            || codex_login::is_workload_identity_selected()
            || [
                codex_state::SQLITE_HOME_ENV,
                codex_exec_server::CODEX_EXEC_SERVER_URL_ENV_VAR,
            ]
            .iter()
            .any(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()))
        {
            return None;
        }
        let executable = cli.daemon_cli_executable.as_deref()?;
        let codex_home = codex_utils_home_dir::find_codex_home()
            .ok()?
            .into_path_buf();
        let managed_releases = codex_home.join("packages/codex-plus-plus/releases");
        let executable = if let Ok(releases) = managed_releases.canonicalize()
            && let Ok(relative) = executable.strip_prefix(releases)
        {
            // The current generation can be pruned. Resume through the same installation's shim.
            let dir = PathBuf::from(std::env::var_os("CODEX_PLUS_PLUS_SHIM_DIR")?);
            let generation = relative.components().nth(1)?.as_os_str();
            let shim = dir.join(if cfg!(windows) { "codex.cmd" } else { "codex" });
            if !dir.is_absolute()
                || !dir
                    .join(".codex-plus-plus-leases")
                    .join(generation)
                    .is_dir()
                || !shim.is_file()
            {
                return None;
            }
            shim
        } else {
            executable.to_path_buf()
        };
        let argv = vec![
            executable.to_str()?.to_owned(),
            "--codex-resume-home".to_owned(),
            codex_home.to_str()?.to_owned(),
            "resume".to_owned(),
        ];
        let mut options = Vec::new();
        // Keep the account selection mode so the same picker and eligibility rules apply.
        for (selected, flag) in [
            (cli.auto_account, "--auto-account"),
            (cli.no_daemon, "--no-daemon"),
            (cli.no_alt_screen, "--no-alt-screen"),
            (cli.strict_config, "--strict-config"),
            (cli.web_search, "--search"),
        ] {
            if selected {
                options.push(flag.to_owned());
            }
        }
        if let Some(profile) = &cli.config_profile_v2 {
            options.extend(["--profile".to_owned(), profile.to_string()]);
        }
        if cli.oss {
            // --oss also selects a default model at bootstrap, which can replace the saved one.
            return None;
        }
        for (key, value) in cli.config_overrides.parse_overrides().ok()? {
            match key.as_str() {
                // Resume restores these from the thread. Reapplying launch overrides could
                // revert later /model or /permissions changes.
                "model"
                | "model_reasoning_effort"
                | "approval_policy"
                | "approvals_reviewer"
                | "sandbox_mode"
                | "default_permissions"
                | "service_tier" => continue,
                "web_search" if matches!(value.as_str(), Some("disabled" | "cached" | "live")) => {}
                "tui.resume_cwd" if matches!(value.as_str(), Some("current" | "session")) => {}
                _ if value.is_bool()
                    && key
                        .strip_prefix("features.")
                        .is_some_and(codex_features::is_known_feature_key) => {}
                // Arbitrary CLI configuration can include credentials or runtime-only policy.
                _ => return None,
            }
            options.extend(["--config".to_owned(), format!("{key}={value}")]);
        }
        Some(Self {
            sessions: codex_home.join(codex_rollout::SESSIONS_SUBDIR),
            argv,
            options,
        })
    }
}
