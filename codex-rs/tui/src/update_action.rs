use codex_install_context::InstallContext;
use codex_install_context::StandalonePlatform;
use codex_install_context::codex_plus_plus::PackageManager;
use codex_install_context::codex_plus_plus::UpdateChannel;
use codex_install_context::codex_plus_plus::UpdatePlan;
use codex_install_context::codex_plus_plus::UpdateTarget;

#[path = "codex_plus_plus/update_action.rs"]
mod codex_plus_plus;

/// Update action the CLI should perform after the TUI exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateAction {
    /// Replace the local daemon after restoring the terminal.
    Daemon(DaemonUpdateSource),
    /// Update via `npm install -g @openai/codex@latest`.
    NpmGlobalLatest,
    /// Update via `bun install -g @openai/codex@latest`.
    BunGlobalLatest,
    /// Update via `vp install -g @openai/codex@latest`.
    VitePlusGlobalLatest,
    /// Update via `pnpm add -g @openai/codex@latest`.
    PnpmGlobalLatest,
    /// Update via `brew upgrade codex`.
    BrewUpgrade,
    /// Update via `curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh`.
    StandaloneUnix,
    /// Update via `$env:CODEX_NON_INTERACTIVE=1; irm https://chatgpt.com/codex/install.ps1 | iex`.
    StandaloneWindows,
    CodexPlusPlusPackageManager(PackageManager),
    CodexPlusPlusStandalone(StandalonePlatform),
}

impl UpdateAction {
    #[cfg(any(not(debug_assertions), test))]
    pub(crate) fn from_install_context(context: &InstallContext) -> Option<Self> {
        Self::from_update_plan(UpdatePlan::for_install_context(
            context,
            UpdateChannel::CodexPlusPlus,
        ))
    }

    pub fn from_update_plan(plan: UpdatePlan) -> Option<Self> {
        if plan.channel() == UpdateChannel::CodexPlusPlus {
            let shim_dir =
                std::env::var_os("CODEX_PLUS_PLUS_SHIM_DIR").map(std::path::PathBuf::from);
            return codex_plus_plus::from_target(plan.target(), shim_dir.as_deref());
        }
        match plan.target() {
            UpdateTarget::PackageManager { manager, .. } => Some(match manager {
                PackageManager::Npm => Self::NpmGlobalLatest,
                PackageManager::Bun => Self::BunGlobalLatest,
                PackageManager::VitePlus => Self::VitePlusGlobalLatest,
                PackageManager::Pnpm => Self::PnpmGlobalLatest,
            }),
            UpdateTarget::Standalone { platform, .. } => Some(match platform {
                StandalonePlatform::Unix => Self::StandaloneUnix,
                StandalonePlatform::Windows => Self::StandaloneWindows,
            }),
            UpdateTarget::Homebrew => Some(Self::BrewUpgrade),
            UpdateTarget::Manual => None,
        }
    }

    pub fn current_plan() -> UpdatePlan {
        UpdatePlan::for_install_context(InstallContext::current(), UpdateChannel::CodexPlusPlus)
    }

    pub(crate) fn release_notes_url(self) -> &'static str {
        match self {
            Self::CodexPlusPlusPackageManager(_) | Self::CodexPlusPlusStandalone(_) => {
                UpdateChannel::CodexPlusPlus
            }
            _ => UpdateChannel::Upstream,
        }
        .release_notes_url()
    }

    /// Returns the list of command-line arguments for invoking the update.
    pub fn command_args(self) -> (&'static str, &'static [&'static str]) {
        match self {
            UpdateAction::Daemon(source) => ("codex", source.command_args()),
            UpdateAction::NpmGlobalLatest => ("npm", &["install", "-g", "@openai/codex"]),
            UpdateAction::BunGlobalLatest => ("bun", &["install", "-g", "@openai/codex"]),
            UpdateAction::VitePlusGlobalLatest => ("vp", &["install", "-g", "@openai/codex"]),
            UpdateAction::PnpmGlobalLatest => ("pnpm", &["add", "-g", "@openai/codex"]),
            UpdateAction::BrewUpgrade => ("brew", &["upgrade", "--cask", "codex"]),
            UpdateAction::StandaloneUnix => (
                "sh",
                &[
                    "-c",
                    "curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh",
                ],
            ),
            Self::StandaloneWindows => (
                "powershell",
                &[
                    "-ExecutionPolicy",
                    "Bypass",
                    "-c",
                    "$env:CODEX_NON_INTERACTIVE=1; irm https://chatgpt.com/codex/install.ps1 | iex",
                ],
            ),
            Self::CodexPlusPlusPackageManager(manager) => {
                codex_plus_plus::package_manager_command_args(manager)
            }
            Self::CodexPlusPlusStandalone(platform) => {
                codex_plus_plus::standalone_command_args(platform)
            }
        }
    }

    pub fn requires_direct_execution(self) -> bool {
        matches!(
            self,
            Self::StandaloneWindows | Self::CodexPlusPlusStandalone(StandalonePlatform::Windows)
        )
    }

    /// Returns string representation of the command-line arguments for invoking the update.
    pub fn command_str(self) -> String {
        let (command, args) = self.command_args();
        shlex::try_join(std::iter::once(command).chain(args.iter().copied()))
            .unwrap_or_else(|_| format!("{command} {}", args.join(" ")))
    }
}

#[cfg(not(debug_assertions))]
pub fn get_update_action() -> Option<UpdateAction> {
    UpdateAction::from_install_context(InstallContext::current())
}

#[cfg(test)]
mod tests {
    use super::codex_plus_plus::from_target;
    use super::*;
    use codex_install_context::InstallMethod;
    use codex_install_context::StandalonePlatform::Unix;
    use codex_install_context::StandalonePlatform::Windows;
    use codex_install_context::codex_plus_plus::PackageManager::Bun;
    use codex_install_context::codex_plus_plus::PackageManager::Npm;
    use codex_install_context::codex_plus_plus::PackageManager::Pnpm;
    use codex_install_context::codex_plus_plus::PackageManager::VitePlus;
    use codex_install_context::codex_plus_plus::UpdateTarget::Standalone;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use pretty_assertions::assert_eq;

    #[test]
    fn maps_install_context_to_update_action() {
        let native_release_dir =
            AbsolutePathBuf::from_absolute_path(std::env::temp_dir().join("native-release"))
                .expect("temp dir path should be absolute");
        let shim_dir = std::env::var_os("CODEX_PLUS_PLUS_SHIM_DIR").map(std::path::PathBuf::from);
        let available = shim_dir.is_some_and(|dir| dir.is_absolute() && dir.is_dir());

        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Other,
                package_layout: None,
            }),
            None
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Npm,
                package_layout: None,
            }),
            Some(UpdateAction::CodexPlusPlusPackageManager(Npm))
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Bun,
                package_layout: None,
            }),
            Some(UpdateAction::CodexPlusPlusPackageManager(Bun))
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::VitePlus,
                package_layout: None,
            }),
            Some(UpdateAction::CodexPlusPlusPackageManager(VitePlus))
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Pnpm,
                package_layout: None,
            }),
            Some(UpdateAction::CodexPlusPlusPackageManager(Pnpm))
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Brew,
                package_layout: None,
            }),
            None
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Standalone {
                    platform: StandalonePlatform::Unix,
                    release_dir: native_release_dir.clone(),
                    resources_dir: Some(native_release_dir.join("codex-resources")),
                },
                package_layout: None,
            }),
            available.then_some(UpdateAction::CodexPlusPlusStandalone(Unix))
        );
        assert_eq!(
            UpdateAction::from_install_context(&InstallContext {
                method: InstallMethod::Standalone {
                    platform: StandalonePlatform::Windows,
                    release_dir: native_release_dir.clone(),
                    resources_dir: Some(native_release_dir.join("codex-resources")),
                },
                package_layout: None,
            }),
            available.then_some(UpdateAction::CodexPlusPlusStandalone(Windows))
        );
        let shim_dir = tempfile::tempdir().expect("tempdir");
        for platform in [Unix, Windows] {
            let target = Standalone {
                platform,
                installer_url: "unused",
            };
            let expected = Some(UpdateAction::CodexPlusPlusStandalone(platform));
            assert_eq!(from_target(target, Some(shim_dir.path())), expected);
            assert!(from_target(target, Some(&shim_dir.path().join("missing"))).is_none());
        }
    }

    #[test]
    fn standalone_update_commands_rerun_latest_installer() {
        assert_eq!(
            UpdateAction::StandaloneUnix.command_args(),
            (
                "sh",
                &[
                    "-c",
                    "curl -fsSL https://chatgpt.com/codex/install.sh | CODEX_NON_INTERACTIVE=1 sh"
                ][..],
            )
        );
        assert_eq!(
            UpdateAction::StandaloneWindows.command_args(),
            (
                "powershell",
                &[
                    "-ExecutionPolicy",
                    "Bypass",
                    "-c",
                    "$env:CODEX_NON_INTERACTIVE=1; irm https://chatgpt.com/codex/install.ps1 | iex"
                ][..],
            )
        );
    }
}

/// Package source explicitly selected by the user in the daemon menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonUpdateSource {
    PublicStable,
    ThisCli,
}

impl DaemonUpdateSource {
    pub fn command_args(self) -> &'static [&'static str] {
        match self {
            Self::PublicStable => &["app-server", "daemon", "update"],
            Self::ThisCli => &["app-server", "daemon", "update", "--from-cli", "--yes"],
        }
    }
}
