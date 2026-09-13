use anyhow::Context;
use anyhow::Result;
use codex_login::default_client::create_client;
use std::time::Duration;
use tokio::process::Command;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct DcgTarget {
    pub(super) tag: String,
    pub(super) version: String,
    pub(super) precedence: [u64; 4],
    pub(super) commit: String,
}
impl DcgTarget {
    pub(super) fn from_tag(tag: &str) -> Option<Self> {
        let version = tag.strip_prefix('v')?;
        let (base, fork) = version.split_once("-codexpp.")?;
        let mut parts = base.split('.').chain(std::iter::once(fork));
        let mut next = || parts.next()?.parse::<u64>().ok();
        let precedence = [next()?, next()?, next()?, next()?];
        parts.next().is_none().then_some(Self {
            tag: tag.to_string(),
            version: version.to_string(),
            precedence,
            commit: String::new(),
        })
    }
    pub(super) fn from_release(release: &serde_json::Value) -> Option<Self> {
        (!release["draft"].as_bool()? && !release["prerelease"].as_bool()?)
            .then(|| release["tag_name"].as_str())
            .flatten()
            .and_then(Self::from_tag)
    }
    pub(super) fn from_releases(xs: &[serde_json::Value]) -> Option<Self> {
        let targets = xs.iter().filter_map(Self::from_release);
        targets.max_by_key(|x| x.precedence)
    }
}
pub(super) async fn resolve(source: &str) -> Result<DcgTarget> {
    let mut response = create_client()
        .get(
            "https://api.github.com/repos/JJLiebig/destructive_command_guard/releases?per_page=100",
        )
        .timeout(Duration::from_secs(5))
        .send()
        .await?
        .error_for_status()?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(body.len() + chunk.len() <= 2 << 20);
        body.extend_from_slice(&chunk);
    }
    let releases: Vec<serde_json::Value> = serde_json::from_slice(&body)?;
    let mut target = DcgTarget::from_releases(&releases)
        .context("no eligible vX.Y.Z-codexpp.N DCG release was found")?;
    let mut command = Command::new("git");
    command
        .args(["ls-remote", source])
        .args([
            format!("refs/tags/{}", target.tag),
            format!("refs/tags/{}^{{}}", target.tag),
        ])
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .context("timed out resolving the DCG release tag")??;
    target.commit = String::from_utf8(output.stdout)?
        .lines()
        .last()
        .and_then(|line| line.split_once('\t'))
        .map(|(sha, _)| sha.to_string())
        .filter(|sha| output.status.success() && sha.len() == 40)
        .context("resolved DCG release tag did not identify an immutable commit")?;
    Ok(target)
}
