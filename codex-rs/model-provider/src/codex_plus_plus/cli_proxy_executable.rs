//! Pinned, verified CLIProxyAPI acquisition, called only with the owned runtime lock held.

use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientBuilder;
use codex_http_client::HttpClientFactory;
use codex_http_client::HttpClientTlsConfig;
use sha2::Digest;
use sha2::Sha256;

pub(super) const TESTED_VERSION: &str = "7.3.14";
// Official archives are 20–23 MiB; the Windows executable is 67 MiB.
const ARCHIVE_LIMIT: usize = 64 * 1024 * 1024;
const BINARY_LIMIT: u64 = 128 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 120);

struct Release<'a> {
    platform: &'static str,
    sha256: &'a str,
}

impl Release<'_> {
    fn archive_name(&self) -> String {
        let extension = if self.platform.starts_with("windows_") {
            "zip"
        } else {
            "tar.gz"
        };
        format!("CLIProxyAPI_{TESTED_VERSION}_{}.{extension}", self.platform)
    }

    fn binary_name(&self) -> &'static str {
        if self.platform.starts_with("windows_") {
            "cli-proxy-api.exe"
        } else {
            "cli-proxy-api"
        }
    }
}

// Source: official v7.3.14 checksums.txt, whose GitHub asset SHA256 is
// 0feb5bb9da66b34e37c6597d991bc4b42af2efe538579484a70894d67bcb5d2c.
fn platform_release() -> io::Result<Release<'static>> {
    let (platform, sha256) = match (
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "musl"),
    ) {
        ("windows", "x86_64", _) => (
            "windows_amd64",
            "2f05772c994f39e7835d0208f3857b48d14a95603eb9160364ff4f3ab6296f7c",
        ),
        ("windows", "aarch64", _) => (
            "windows_aarch64",
            "1de8d6b70ff511f476abe9c9d44b0bcb3f35bd01c90890c7a24a4869fe1ef721",
        ),
        ("macos", "x86_64", _) => (
            "darwin_amd64",
            "b26eab8b7877a10e8a24e4ddcc136a3ea842914da7822985b2d36a9639a39e8b",
        ),
        ("macos", "aarch64", _) => (
            "darwin_aarch64",
            "ec45cffb882ef94f2c80b897767b376fea57e025ff28cef118453d58d4c83964",
        ),
        ("linux", "x86_64", false) => (
            "linux_amd64",
            "a93cdfb2e8b673eb362dedb0f0317ad3baf4d5eb2256da297239badb73c1d23d",
        ),
        ("linux", "aarch64", false) => (
            "linux_aarch64",
            "741eb9296410e7b67df76aa8d33bd06297dd694c822983a1a72ee54b06e7f64d",
        ),
        ("linux", "x86_64", true) => (
            "linux_amd64_no-plugin",
            "2b9003bd1f5bc71f68c7c21c9f4c049aae6e6583ae3cbaf3acedb55b5a8854f0",
        ),
        ("linux", "aarch64", true) => (
            "linux_aarch64_no-plugin",
            "41c84a72303e31ff012f0e486013b506177ee4477a498fb5206f20e481e5f89d",
        ),
        _ => {
            return Err(io::Error::other(format!(
                "CLIProxyAPI managed download is unavailable for {}/{}; supply a compatible absolute CODEX_CLI_PROXY_EXECUTABLE or use model_provider = \"openai\"",
                std::env::consts::OS,
                std::env::consts::ARCH
            )));
        }
    };
    Ok(Release { platform, sha256 })
}

pub(super) async fn resolve(
    dir: &Path,
    explicit: Option<&Path>,
    saved: &Path,
    factory: &HttpClientFactory,
) -> io::Result<PathBuf> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let candidates = std::env::split_paths(&search_path)
        .map(|dir| {
            dir.join(if cfg!(windows) {
                "cli-proxy-api.exe"
            } else {
                "cli-proxy-api"
            })
        })
        .chain(saved.is_absolute().then(|| saved.to_path_buf()));
    if let Some(path) = installed_executable(explicit, candidates)? {
        return Ok(path);
    }
    let release = platform_release()?;
    let managed = dir.join(format!("managed-{TESTED_VERSION}"));
    codex_uds::prepare_private_socket_directory(&managed).await?;
    let executable = managed.join(release.binary_name());
    if compatible(&executable) {
        return executable.canonicalize();
    }
    let url = format!(
        "https://github.com/router-for-me/CLIProxyAPI/releases/download/v{TESTED_VERSION}/{}",
        release.archive_name()
    );
    provision(&managed, &release, &url, factory).await.map_err(|error| {
        io::Error::new(error.kind(), format!(
            "Provision CLIProxyAPI {TESTED_VERSION}: {error}. Retry proxy startup, supply CODEX_CLI_PROXY_EXECUTABLE, or use model_provider = \"openai\""
        ))
    })
}

pub(super) fn installed_executable(
    explicit: Option<&Path>,
    candidates: impl IntoIterator<Item = PathBuf>,
) -> io::Result<Option<PathBuf>> {
    if let Some(path) = explicit {
        if !path.is_absolute() {
            return Err(io::Error::other(
                "CODEX_CLI_PROXY_EXECUTABLE must be an absolute path",
            ));
        }
        if compatible(path) {
            return path.canonicalize().map(Some);
        }
        return Err(io::Error::other(format!(
            "CODEX_CLI_PROXY_EXECUTABLE must point to CLIProxyAPI {TESTED_VERSION}; correct or unset it to use the managed download. The direct backend remains available with model_provider = \"openai\""
        )));
    }
    for path in candidates {
        if path.is_absolute() && compatible(&path) {
            return path.canonicalize().map(Some);
        }
    }
    Ok(None)
}

fn compatible(path: &Path) -> bool {
    let mut command = Command::new(path);
    command.arg("-help");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command.output().is_ok_and(|output| {
        let expected = format!("CLIProxyAPI Version: {TESTED_VERSION},");
        String::from_utf8_lossy(&output.stdout).starts_with(&expected)
            || String::from_utf8_lossy(&output.stderr).starts_with(&expected)
    })
}

async fn provision(
    managed: &Path,
    release: &Release<'_>,
    url: &str,
    factory: &HttpClientFactory,
) -> io::Result<PathBuf> {
    let executable = managed.join(release.binary_name());
    let staged = managed.join(format!("{}.pending", release.binary_name()));
    if staged.exists() {
        fs::remove_file(&staged)?;
    }
    // The pool checks the supplied policy and proxy routing again on every GitHub redirect.
    let client = HttpClientBuilder::new()
        .without_request_logging()
        .build_with_tls(
            factory,
            ClientRouteClass::Other,
            HttpClientTlsConfig::default(),
        );
    let mut response = client
        .get(url)
        .timeout(DOWNLOAD_TIMEOUT)
        .send()
        .await
        .map_err(io::Error::other)?
        .error_for_status()
        .map_err(io::Error::other)?;
    if response
        .content_length()
        .is_some_and(|length| length > ARCHIVE_LIMIT as u64)
    {
        return Err(io::Error::other(
            "official archive exceeds the 64 MiB download limit",
        ));
    }
    let mut archive = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(io::Error::other)? {
        if chunk.len() > ARCHIVE_LIMIT - archive.len() {
            return Err(io::Error::other(
                "official archive exceeds the 64 MiB download limit",
            ));
        }
        archive.extend_from_slice(&chunk);
    }
    let binary = extract_binary(&archive, release)?;
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o700);
        }
        let mut file = options.open(&staged)?;
        file.write_all(&binary)?;
        file.sync_all()?;
        drop(file);
        if !compatible(&staged) {
            return Err(io::Error::other(
                "verified archive executable has an incompatible version banner",
            ));
        }
        fs::rename(&staged, &executable)?;
        executable.canonicalize()
    })();
    if result.is_err() {
        let _ = fs::remove_file(staged);
    }
    result
}

fn extract_binary(archive: &[u8], release: &Release<'_>) -> io::Result<Vec<u8>> {
    if format!("{:x}", Sha256::digest(archive)) != release.sha256 {
        return Err(io::Error::other(
            "official archive SHA256 verification failed",
        ));
    }
    let mut binary = Vec::new();
    if release.platform.starts_with("windows_") {
        let mut archive =
            zip::ZipArchive::new(io::Cursor::new(archive)).map_err(io::Error::other)?;
        let entry = archive
            .by_name(release.binary_name())
            .map_err(io::Error::other)?;
        if !entry.is_file() || entry.is_symlink() || entry.size() > BINARY_LIMIT {
            return Err(io::Error::other(
                "archive executable is not a bounded regular file",
            ));
        }
        entry.take(BINARY_LIMIT + 1).read_to_end(&mut binary)?;
    } else {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        for entry in archive.entries()? {
            let mut entry = entry?;
            if entry.path_bytes().as_ref() != release.binary_name().as_bytes() {
                continue;
            }
            if !entry.header().entry_type().is_file() || entry.size() > BINARY_LIMIT {
                return Err(io::Error::other(
                    "archive executable is not a bounded regular file",
                ));
            }
            entry
                .by_ref()
                .take(BINARY_LIMIT + 1)
                .read_to_end(&mut binary)?;
            break;
        }
    }
    if binary.is_empty() || binary.len() as u64 > BINARY_LIMIT {
        return Err(io::Error::other(
            "verified archive is missing a bounded CLIProxyAPI executable",
        ));
    }
    Ok(binary)
}

#[cfg(test)]
#[path = "cli_proxy_executable_tests.rs"]
mod tests;
