use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClient;
use codex_http_client::HttpClientBuilder;
use codex_http_client::HttpClientFactory;
use codex_http_client::HttpClientTlsConfig;
use rand::RngCore;
use serde::Deserialize;
use serde::Serialize;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
// Windows can take over a second to report refusal on a stopped loopback listener.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const STATE_LIMIT: u64 = 16 * 1024;

#[derive(Clone, Debug)]
pub(super) struct CliProxyRuntime {
    home: PathBuf,
    executable: Option<PathBuf>,
}

pub(super) struct RuntimeEndpoint {
    pub base_url: String,
    pub inference_key: String,
    pub management_key: String,
}

/// Retains the existing runtime lock so startup cannot race stopped-file cleanup.
pub(super) struct AttachedRuntime {
    pub endpoint: Option<RuntimeEndpoint>,
    _lock: File,
}

#[derive(Deserialize, Serialize)]
struct RuntimeState {
    port: u16,
    pid: u32,
    executable: PathBuf,
    inference_key: String,
    management_key: String,
}

impl RuntimeState {
    fn endpoint(&self) -> RuntimeEndpoint {
        RuntimeEndpoint {
            base_url: format!("https://127.0.0.1:{}/v1", self.port),
            inference_key: self.inference_key.clone(),
            management_key: self.management_key.clone(),
        }
    }
}

impl CliProxyRuntime {
    pub(super) fn new(home: PathBuf, executable: Option<PathBuf>) -> Self {
        Self { home, executable }
    }

    pub(super) fn home(&self) -> &Path {
        &self.home
    }

    pub(super) async fn attach_only(
        &self,
        factory: HttpClientFactory,
    ) -> io::Result<Option<AttachedRuntime>> {
        let runtime = self.clone();
        let handle = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let dir = runtime.home.join("cli-proxy");
            match fs::symlink_metadata(&dir) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => return Err(io::Error::other("CLIProxyAPI directory is invalid")),
                Err(error) => return Err(error),
            }
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(dir.join("runtime.lock"))?;
            lock.lock()?;
            let endpoint = match read_state(&dir.join("runtime.json"))? {
                Some(state) => match handle.block_on(probe_owned(
                    &state,
                    &factory,
                    runtime.tls_config()?,
                    PROBE_TIMEOUT,
                ))? {
                    Some(true) => Some(state.endpoint()),
                    None => None,
                    Some(false) => {
                        return Err(io::Error::other(
                            "CLIProxyAPI runtime could not be verified",
                        ));
                    }
                },
                None => None,
            };
            Ok(Some(AttachedRuntime {
                endpoint,
                _lock: lock,
            }))
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(super) async fn ensure(&self, factory: HttpClientFactory) -> io::Result<RuntimeEndpoint> {
        let runtime = self.clone();
        let handle = tokio::runtime::Handle::current();
        codex_uds::prepare_private_socket_directory(self.home.join("cli-proxy")).await?;
        tokio::task::spawn_blocking(move || runtime.ensure_blocking(&handle, &factory))
            .await
            .map_err(io::Error::other)?
    }

    fn ensure_blocking(
        &self,
        handle: &tokio::runtime::Handle,
        factory: &HttpClientFactory,
    ) -> io::Result<RuntimeEndpoint> {
        let dir = self.home.join("cli-proxy");
        let lock_path = dir.join("runtime.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock()?;
        if let Some(executable) = &self.executable {
            super::cli_proxy_executable::installed_executable(Some(executable), [])?;
        }

        let cert_path = dir.join("certificate.pem");
        let key_path = dir.join("private-key.pem");
        if !cert_path.exists() && !key_path.exists() {
            let rcgen::CertifiedKey { cert, signing_key } =
                rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])
                    .map_err(io::Error::other)?;
            write_private(&key_path, signing_key.serialize_pem().as_bytes())?;
            write_private(&cert_path, cert.pem().as_bytes())?;
        }
        // A partial identity fails closed rather than rotating a pin used by other sessions.
        let tls = self.tls_config()?;
        let state_path = dir.join("runtime.json");
        let config_path = dir.join("config.yaml");
        let mut state = match read_state(&state_path)? {
            Some(state) => {
                match handle.block_on(probe_owned(&state, factory, tls.clone(), PROBE_TIMEOUT))? {
                    Some(true) => return Ok(state.endpoint()),
                    Some(false) => {
                        return Err(io::Error::other(
                            "CLIProxyAPI's saved loopback port is occupied by an unauthenticated process; stop that process or remove the stale owned runtime after verifying it is stopped",
                        ));
                    }
                    None => {}
                }
                state
            }
            None => {
                let listener = TcpListener::bind(("127.0.0.1", 0))?;
                let port = listener.local_addr()?.port();
                drop(listener);
                RuntimeState {
                    port,
                    pid: 0,
                    executable: PathBuf::new(),
                    inference_key: random_key(),
                    management_key: random_key(),
                }
            }
        };
        handle.block_on(codex_uds::prepare_private_socket_directory(
            dir.join("auth"),
        ))?;
        if config_path.exists() {
            fs::remove_file(&config_path)?;
        }
        write_config(&config_path, &dir, &state)?;
        let executable = handle.block_on(super::cli_proxy_executable::resolve(
            &dir,
            self.executable.as_deref(),
            &state.executable,
            factory,
        ))?;
        if state.pid == 0 {
            // Persist the keys before launch so another host can attach after a crash.
            write_state(&state_path, &state)?;
        }
        state.executable = executable.clone();
        let mut command = Command::new(&executable);
        configure_owned_command(&mut command, &dir);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .map_err(|error| io::Error::new(error.kind(), format!("start CLIProxyAPI: {error}")))?;
        state.pid = child.id();
        if let Err(error) = write_state(&state_path, &state) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        let started = (|| -> io::Result<RuntimeEndpoint> {
            let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
            while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
                if let Some(status) = child.try_wait()? {
                    return Err(io::Error::other(format!(
                        "CLIProxyAPI exited during startup ({status}); check the executable and owned configuration"
                    )));
                }
                if handle.block_on(probe_owned(
                    &state,
                    factory,
                    tls.clone(),
                    remaining.min(PROBE_TIMEOUT),
                ))? == Some(true)
                {
                    return Ok(state.endpoint());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(io::Error::other(
                "CLIProxyAPI did not start within 10 seconds; check the executable and owned configuration",
            ))
        })();
        if started.is_err() {
            let _ = child.kill();
            let _ = child.wait();
        }
        started
    }

    pub(super) fn http_client(&self, factory: &HttpClientFactory) -> io::Result<HttpClient> {
        let state = read_state(&self.home.join("cli-proxy/runtime.json"))?
            .ok_or_else(|| io::Error::other("CLIProxyAPI runtime has not started"))?;
        pinned_client(factory, self.tls_config()?, &state.endpoint().base_url)
    }

    pub(super) fn claude_login_command(&self) -> io::Result<Command> {
        let dir = self.home.join("cli-proxy");
        let state = read_state(&dir.join("runtime.json"))?
            .ok_or_else(|| io::Error::other("Claude sign-in needs the owned runtime"))?;
        let executable = super::cli_proxy_executable::installed_executable(
            /*explicit*/ None,
            [state.executable],
        )?
        .ok_or_else(|| io::Error::other("The saved sign-in executable is incompatible"))?;
        let mut command = Command::new(&executable);
        configure_owned_command(&mut command, &dir);
        command.arg("-claude-login");
        Ok(command)
    }

    fn tls_config(&self) -> io::Result<HttpClientTlsConfig> {
        let dir = self.home.join("cli-proxy");
        fs::metadata(dir.join("private-key.pem"))?;
        HttpClientTlsConfig::default()
            .with_root_certificate_pem(&fs::read(dir.join("certificate.pem"))?)
            .map_err(io::Error::other)
    }
}

fn configure_owned_command(command: &mut Command, dir: &Path) {
    command
        .arg("-config")
        .arg(dir.join("config.yaml"))
        .current_dir(dir);
    // v7.3.14 reads these selectors before -config; never redirect owned credentials/config.
    for selector in [
        "PGSTORE_DSN",
        "GITSTORE_GIT_URL",
        "OBJECTSTORE_ENDPOINT",
        "HOME_JWT",
    ] {
        command
            .env_remove(selector)
            .env_remove(selector.to_ascii_lowercase());
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
}

fn random_key() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_config(path: &Path, dir: &Path, state: &RuntimeState) -> io::Result<()> {
    let yaml_path = |name: &str| -> io::Result<String> {
        let path = dir.join(name);
        serde_json::to_string(
            path.to_str()
                .ok_or_else(|| io::Error::other("CLIProxyAPI path is not valid Unicode"))?,
        )
        .map_err(io::Error::other)
    };
    let auth_dir = yaml_path("auth")?;
    let cert = yaml_path("certificate.pem")?;
    let key = yaml_path("private-key.pem")?;
    let text = format!(
        "host: \"127.0.0.1\"\nport: {}\ntls:\n  enable: true\n  cert: {cert}\n  key: {key}\nauth-dir: {auth_dir}\napi-keys: [\"{}\"]\nremote-management:\n  allow-remote: false\n  secret-key: \"{}\"\n  disable-control-panel: true\nforce-model-prefix: true\ndisable-claude-cloak-mode: true\ndisable-image-generation: \"passthrough\"\nrequest-retry: 0\nmax-retry-interval: 0\nrequest-log: false\nlogging-to-file: false\nrouting:\n  session-affinity: true\nstreaming:\n  keepalive-seconds: 0\n  bootstrap-retries: 0\ncodex:\n  optimize-multi-agent-v2: true\n  identity-confuse: false\n",
        state.port, state.inference_key, state.management_key
    );
    write_private(path, text.as_bytes())
}

fn read_state(path: &Path) -> io::Result<Option<RuntimeState>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > STATE_LIMIT {
        return Err(io::Error::other(
            "CLIProxyAPI runtime state is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(STATE_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    let state: RuntimeState = serde_json::from_slice(&bytes)?;
    if state.port == 0 || state.inference_key.len() != 64 || state.management_key.len() != 64 {
        return Err(io::Error::other("CLIProxyAPI runtime state is invalid"));
    }
    Ok(Some(state))
}

fn write_state(path: &Path, state: &RuntimeState) -> io::Result<()> {
    let pending = path.with_extension("pending");
    if pending.exists() {
        fs::remove_file(&pending)?;
    }
    write_private(&pending, &serde_json::to_vec(state)?)?;
    fs::rename(pending, path)
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn pinned_client(
    factory: &HttpClientFactory,
    tls: HttpClientTlsConfig,
    base_url: &str,
) -> io::Result<HttpClient> {
    let origin = url::Url::parse(base_url).map_err(io::Error::other)?;
    let factory = factory
        .clone()
        .with_network_policy(factory.network_policy().clone().restrict_to_origin(origin));
    Ok(HttpClientBuilder::new()
        .default_headers(codex_login::default_client::default_headers())
        .without_redirects()
        .without_request_logging()
        .build_with_tls(&factory, ClientRouteClass::Api, tls))
}

async fn probe_owned(
    state: &RuntimeState,
    factory: &HttpClientFactory,
    tls: HttpClientTlsConfig,
    timeout: Duration,
) -> io::Result<Option<bool>> {
    let url = format!("https://127.0.0.1:{}/v0/management/auth-files", state.port);
    let response = pinned_client(factory, tls, &state.endpoint().base_url)?
        .get(url)
        .bearer_auth(&state.management_key)
        .timeout(timeout)
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => {
            let version = response
                .headers()
                .get("x-cpa-version")
                .and_then(|value| value.to_str().ok());
            if version != Some(super::cli_proxy_executable::TESTED_VERSION) {
                return Err(io::Error::other(format!(
                    "The owned CLIProxyAPI process reports version {}; {} is required. Stop the owned proxy after its active sessions finish, then retry startup; Codex++ will not replace a running process",
                    version.unwrap_or("unknown"),
                    super::cli_proxy_executable::TESTED_VERSION
                )));
            }
            Ok(Some(true))
        }
        Ok(_) => Ok(Some(false)),
        Err(error) => {
            // TLS/policy failures are occupied, untrusted endpoints. Only refusal permits restart.
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&error);
            while let Some(error) = source {
                if error
                    .downcast_ref::<io::Error>()
                    .is_some_and(|error| error.kind() == io::ErrorKind::ConnectionRefused)
                {
                    return Ok(None);
                }
                source = error.source();
            }
            Ok(Some(false))
        }
    }
}

#[cfg(test)]
#[path = "cli_proxy_runtime_tests.rs"]
mod tests;
