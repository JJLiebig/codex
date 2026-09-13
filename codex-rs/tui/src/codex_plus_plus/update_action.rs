use super::UpdateAction;
use codex_install_context::StandalonePlatform;
use codex_install_context::codex_plus_plus as planner;
use codex_install_context::codex_plus_plus::UpdateTarget;
use std::path::Path;

type CommandArgs = (&'static str, &'static [&'static str]);

pub(super) fn from_target(target: UpdateTarget, dir: Option<&Path>) -> Option<UpdateAction> {
    match target {
        UpdateTarget::PackageManager { manager, .. } => {
            Some(UpdateAction::CodexPlusPlusPackageManager(manager))
        }
        UpdateTarget::Standalone { platform, .. } => {
            dir.and_then(|dir| standalone_action(platform, dir))
        }
        UpdateTarget::Homebrew | UpdateTarget::Manual => None,
    }
}

pub(super) fn standalone_action(platform: StandalonePlatform, dir: &Path) -> Option<UpdateAction> {
    (dir.is_absolute() && dir.is_dir()).then_some(UpdateAction::CodexPlusPlusStandalone(platform))
}
pub(super) fn package_manager_command_args(manager: planner::PackageManager) -> CommandArgs {
    match manager {
        planner::PackageManager::Npm => ("npm", &["install", "-g", "@jjliebig/codex-plus-plus"]),
        planner::PackageManager::Bun => ("bun", &["install", "-g", "@jjliebig/codex-plus-plus"]),
        planner::PackageManager::VitePlus => {
            ("vp", &["install", "-g", "@jjliebig/codex-plus-plus"])
        }
        planner::PackageManager::Pnpm => ("pnpm", &["add", "-g", "@jjliebig/codex-plus-plus"]),
    }
}

pub(super) fn standalone_command_args(platform: StandalonePlatform) -> CommandArgs {
    match platform {
        StandalonePlatform::Unix => (
            "sh",
            &[
                "-c",
                "shim=$CODEX_PLUS_PLUS_SHIM_DIR && installer=$(mktemp \"${TMPDIR:-/tmp}/codex-plus-plus-update.XXXXXX\") && trap 'rm -f \"$installer\"' 0 && curl -fsSL https://raw.githubusercontent.com/JJLiebig/codex/main/scripts/install/install-codex-plus-plus-latest.sh -o \"$installer\" && [ -s \"$installer\" ] && sh \"$installer\" --shim-dir \"$shim\"",
            ],
        ),
        StandalonePlatform::Windows => (
            "powershell",
            &[
                "-ExecutionPolicy",
                "Bypass",
                "-c",
                "$shim=$env:CODEX_PLUS_PLUS_SHIM_DIR; & ([scriptblock]::Create((irm https://raw.githubusercontent.com/JJLiebig/codex/main/scripts/install/install-codex-plus-plus-latest.ps1))) -ShimDir $shim",
            ],
        ),
    }
}
