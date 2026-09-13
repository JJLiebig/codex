use crate::InstallContext;
use crate::InstallMethod;
use crate::StandalonePlatform;

const CODEX_PLUS_PLUS_GITHUB_API_URL: &str =
    "https://api.github.com/repos/JJLiebig/codex/releases/latest";
const CODEX_PLUS_PLUS_INSTALL_URL: &str = "https://github.com/JJLiebig/codex#install-a-release";
const CODEX_PLUS_PLUS_NPM_PACKAGE: &str = "@jjliebig/codex-plus-plus";
const CODEX_PLUS_PLUS_NPM_REGISTRY_URL: &str =
    "https://registry.npmjs.org/@jjliebig%2fcodex-plus-plus";
const CODEX_PLUS_PLUS_RELEASE_NOTES_URL: &str = "https://github.com/JJLiebig/codex/releases/latest";
const CODEX_PLUS_PLUS_TAG_PREFIX: &str = "codex-plus-plus-v";
const CODEX_PLUS_PLUS_UNIX_INSTALLER_URL: &str = "https://raw.githubusercontent.com/JJLiebig/codex/main/scripts/install/install-codex-plus-plus-latest.sh";
const CODEX_PLUS_PLUS_WINDOWS_INSTALLER_URL: &str = "https://raw.githubusercontent.com/JJLiebig/codex/main/scripts/install/install-codex-plus-plus-latest.ps1";
const HOMEBREW_CASK_API_URL: &str = "https://formulae.brew.sh/api/cask/codex.json";
const UPSTREAM_GITHUB_API_URL: &str = "https://api.github.com/repos/openai/codex/releases/latest";
const UPSTREAM_INSTALL_URL: &str = "https://developers.openai.com/codex/cli/";
const UPSTREAM_NPM_PACKAGE: &str = "@openai/codex";
const UPSTREAM_NPM_REGISTRY_URL: &str = "https://registry.npmjs.org/@openai%2fcodex";
const UPSTREAM_RELEASE_NOTES_URL: &str = "https://github.com/openai/codex/releases/latest";
const UPSTREAM_TAG_PREFIX: &str = "rust-v";
const UPSTREAM_UNIX_INSTALLER_URL: &str = "https://chatgpt.com/codex/install.sh";
const UPSTREAM_WINDOWS_INSTALLER_URL: &str = "https://chatgpt.com/codex/install.ps1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateChannel {
    CodexPlusPlus,
    Upstream,
}

impl UpdateChannel {
    pub fn release_notes_url(self) -> &'static str {
        match self {
            Self::CodexPlusPlus => CODEX_PLUS_PLUS_RELEASE_NOTES_URL,
            Self::Upstream => UPSTREAM_RELEASE_NOTES_URL,
        }
    }

    pub fn install_url(self) -> &'static str {
        match self {
            Self::CodexPlusPlus => CODEX_PLUS_PLUS_INSTALL_URL,
            Self::Upstream => UPSTREAM_INSTALL_URL,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageManager {
    Npm,
    Bun,
    VitePlus,
    Pnpm,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UpdateTarget {
    PackageManager {
        manager: PackageManager,
        package: &'static str,
    },
    Standalone {
        platform: StandalonePlatform,
        installer_url: &'static str,
    },
    Homebrew,
    Manual,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LatestVersionSource {
    GitHub {
        api_url: &'static str,
        tag_prefix: &'static str,
        npm_registry_url: Option<&'static str>,
    },
    Homebrew {
        api_url: &'static str,
    },
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UpdatePlan {
    channel: UpdateChannel,
    target: UpdateTarget,
}

impl UpdatePlan {
    pub fn for_install_context(context: &InstallContext, channel: UpdateChannel) -> Self {
        let target = match &context.method {
            InstallMethod::Npm => package_manager_target(PackageManager::Npm, channel),
            InstallMethod::Bun => package_manager_target(PackageManager::Bun, channel),
            InstallMethod::VitePlus => package_manager_target(PackageManager::VitePlus, channel),
            InstallMethod::Pnpm => package_manager_target(PackageManager::Pnpm, channel),
            InstallMethod::Standalone { platform, .. } => standalone_target(*platform, channel),
            InstallMethod::Brew if channel == UpdateChannel::Upstream => UpdateTarget::Homebrew,
            InstallMethod::Brew | InstallMethod::Other => UpdateTarget::Manual,
        };
        Self { channel, target }
    }

    pub fn channel(self) -> UpdateChannel {
        self.channel
    }

    pub fn target(self) -> UpdateTarget {
        self.target
    }

    pub fn package_manager_package(self) -> Option<&'static str> {
        match self.target {
            UpdateTarget::PackageManager { package, .. } => Some(package),
            UpdateTarget::Standalone { .. } | UpdateTarget::Homebrew | UpdateTarget::Manual => None,
        }
    }

    pub fn latest_version_source(self) -> LatestVersionSource {
        if self.target == UpdateTarget::Homebrew {
            return LatestVersionSource::Homebrew {
                api_url: HOMEBREW_CASK_API_URL,
            };
        }
        let (api_url, tag_prefix, npm_registry_url) = match self.channel {
            UpdateChannel::CodexPlusPlus => (
                CODEX_PLUS_PLUS_GITHUB_API_URL,
                CODEX_PLUS_PLUS_TAG_PREFIX,
                CODEX_PLUS_PLUS_NPM_REGISTRY_URL,
            ),
            UpdateChannel::Upstream => (
                UPSTREAM_GITHUB_API_URL,
                UPSTREAM_TAG_PREFIX,
                UPSTREAM_NPM_REGISTRY_URL,
            ),
        };
        LatestVersionSource::GitHub {
            api_url,
            tag_prefix,
            npm_registry_url: matches!(self.target, UpdateTarget::PackageManager { .. })
                .then_some(npm_registry_url),
        }
    }
}

pub fn is_newer(latest: &str, current: &str) -> Option<bool> {
    let (latest_base, latest_revision) = parse_version(latest)?;
    let (current_base, current_revision) = parse_version(current)?;
    match (latest_revision, current_revision) {
        (Some(latest_revision), Some(current_revision)) => {
            Some((latest_base, latest_revision) > (current_base, current_revision))
        }
        (None, None) => Some(latest_base > current_base),
        (Some(_), None) | (None, Some(_)) => None,
    }
}

pub(super) fn parse_version(value: &str) -> Option<((u64, u64, u64), Option<u64>)> {
    let value = value.trim();
    let (base, fork_revision) = match value.split_once("-fork.") {
        Some((base, revision)) => (base, Some(parse_number(revision)?)),
        None if value.contains('-') => return None,
        None => (value, None),
    };
    let mut parts = base.split('.');
    let upstream_base = (
        parse_number(parts.next()?)?,
        parse_number(parts.next()?)?,
        parse_number(parts.next()?)?,
    );
    (parts.next().is_none()).then_some((upstream_base, fork_revision))
}

fn parse_number(value: &str) -> Option<u64> {
    value.parse().ok().filter(|_| !value.starts_with('+'))
}

fn package_manager_target(manager: PackageManager, channel: UpdateChannel) -> UpdateTarget {
    let package = match channel {
        UpdateChannel::CodexPlusPlus => CODEX_PLUS_PLUS_NPM_PACKAGE,
        UpdateChannel::Upstream => UPSTREAM_NPM_PACKAGE,
    };
    UpdateTarget::PackageManager { manager, package }
}

fn standalone_target(platform: StandalonePlatform, channel: UpdateChannel) -> UpdateTarget {
    use StandalonePlatform as S;
    use UpdateChannel as C;
    UpdateTarget::Standalone {
        platform,
        installer_url: match (channel, platform) {
            (C::CodexPlusPlus, S::Unix) => CODEX_PLUS_PLUS_UNIX_INSTALLER_URL,
            (C::CodexPlusPlus, S::Windows) => CODEX_PLUS_PLUS_WINDOWS_INSTALLER_URL,
            (C::Upstream, S::Unix) => UPSTREAM_UNIX_INSTALLER_URL,
            (C::Upstream, S::Windows) => UPSTREAM_WINDOWS_INSTALLER_URL,
        },
    }
}

#[cfg(test)]
#[path = "update_plan_tests.rs"]
mod tests;
