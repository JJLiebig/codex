use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::io::Read;
use std::io::Write;
use std::net::SocketAddr;
use std::net::TcpListener;
use std::net::TcpStream;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;

use rand::RngCore;
use serde::Deserialize;
use serde::Serialize;

const TESTED_VERSION: &str = "7.3.12";
const STARTUP_ATTEMPTS: usize = 100;
const STATE_LIMIT: u64 = 16 * 1024;

#[derive(Clone, Debug)]
pub(super) struct CliProxyRuntime {
    home: PathBuf,
    executable: Option<PathBuf>,
}

#[derive(Clone)]
pub(super) struct RuntimeEndpoint {
    pub base_url: String,
    pub inference_key: String,
}

impl std::fmt::Debug for RuntimeEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeEndpoint")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
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
            base_url: format!("http://127.0.0.1:{}/v1", self.port),
            inference_key: self.inference_key.clone(),
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

    pub(super) async fn ensure(&self) -> io::Result<RuntimeEndpoint> {
        let runtime = self.clone();
        tokio::task::spawn_blocking(move || runtime.ensure_blocking())
            .await
            .map_err(io::Error::other)?
    }

    fn ensure_blocking(&self) -> io::Result<RuntimeEndpoint> {
        let dir = self.home.join("cli-proxy");
        private_dir(&dir)?;
        let lock_path = dir.join("runtime.lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock()?;

        let state_path = dir.join("runtime.json");
        let config_path = dir.join("config.yaml");
        let mut state = match read_state(&state_path)? {
            Some(state) => {
                match probe_owned(state.port, &state.management_key)? {
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
                let state = RuntimeState {
                    port,
                    pid: 0,
                    executable: PathBuf::new(),
                    inference_key: random_key(),
                    management_key: random_key(),
                };
                state
            }
        };
        private_dir(&dir.join("auth"))?;
        if config_path.exists() {
            fs::remove_file(&config_path)?;
        }
        write_config(&config_path, &dir.join("auth"), &state)?;
        let executable = self.compatible_executable()?;
        if state.pid == 0 {
            // Persist the keys before launch so another host can attach after a crash.
            write_state(&state_path, &state)?;
        }
        state.executable = executable.clone();
        let mut command = Command::new(&executable);
        command
            .arg("-config")
            .arg(&config_path)
            .current_dir(&dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
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
            for _ in 0..STARTUP_ATTEMPTS {
                if let Some(status) = child.try_wait()? {
                    return Err(io::Error::other(format!(
                        "CLIProxyAPI exited during startup ({status}); check the executable and owned configuration"
                    )));
                }
                if probe_owned(state.port, &state.management_key)? == Some(true) {
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

    fn compatible_executable(&self) -> io::Result<PathBuf> {
        let explicit = self.executable.as_ref();
        let candidates: Vec<PathBuf> = match explicit {
            Some(path) if path.is_absolute() => vec![path.clone()],
            Some(_) => {
                return Err(io::Error::other(
                    "CODEX_CLI_PROXY_EXECUTABLE must be an absolute path",
                ));
            }
            None => std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|dir| {
                    dir.join(if cfg!(windows) {
                        "cli-proxy-api.exe"
                    } else {
                        "cli-proxy-api"
                    })
                })
                .filter(|path| path.is_file())
                .collect(),
        };
        for path in candidates {
            if let Ok(output) = Command::new(&path).arg("-help").output() {
                let banner = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let expected = format!("CLIProxyAPI Version: {TESTED_VERSION},");
                if banner.starts_with(&expected) || stderr.starts_with(&expected) {
                    return path.canonicalize();
                }
            }
        }
        Err(io::Error::other(format!(
            "CLIProxyAPI {TESTED_VERSION} is required. Set CODEX_CLI_PROXY_EXECUTABLE to the absolute path of that release, or install it on PATH. The direct OpenAI backend remains available with model_provider = \"openai\"."
        )))
    }
}

fn random_key() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700).create(path)?;
        fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::other(
            "CLIProxyAPI state path must be a directory",
        ));
    }
    Ok(())
}

fn write_config(path: &Path, auth_dir: &Path, state: &RuntimeState) -> io::Result<()> {
    let auth_dir = auth_dir
        .to_str()
        .ok_or_else(|| io::Error::other("CLIProxyAPI auth path is not valid Unicode"))?;
    let auth_dir = serde_json::to_string(auth_dir)?;
    let text = format!(
        "host: \"127.0.0.1\"\nport: {}\nauth-dir: {}\napi-keys: [\"{}\"]\nremote-management:\n  allow-remote: false\n  secret-key: \"{}\"\n  disable-control-panel: true\nforce-model-prefix: true\ndisable-claude-cloak-mode: true\ndisable-image-generation: \"passthrough\"\nrequest-retry: 0\nmax-retry-interval: 0\nrequest-log: false\nlogging-to-file: false\nrouting:\n  session-affinity: true\nstreaming:\n  keepalive-seconds: 0\n  bootstrap-retries: 0\ncodex:\n  optimize-multi-agent-v2: true\n  identity-confuse: false\n",
        state.port, auth_dir, state.inference_key, state.management_key
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

fn probe(port: u16, key: &str) -> io::Result<Option<bool>> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = match TcpStream::connect_timeout(&address, Duration::from_millis(200)) {
        Ok(stream) => stream,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    stream.set_read_timeout(Some(Duration::from_secs(1)))?;
    stream.set_write_timeout(Some(Duration::from_secs(1)))?;
    write!(
        stream,
        "GET /v0/management/auth-files HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {key}\r\nConnection: close\r\n\r\n"
    )?;
    let mut response = [0_u8; 64];
    let mut length = 0;
    while length < response.len() && !response[..length].contains(&b'\n') {
        let read = stream.read(&mut response[length..])?;
        if read == 0 {
            break;
        }
        length += read;
    }
    Ok(Some(
        response[..length].starts_with(b"HTTP/1.1 200")
            || response[..length].starts_with(b"HTTP/1.0 200"),
    ))
}

fn probe_owned(port: u16, key: &str) -> io::Result<Option<bool>> {
    match probe(port, key)? {
        Some(true) => Ok(Some(probe(port, "invalid-codex-probe")? == Some(false))),
        other => Ok(other),
    }
}

#[cfg(test)]
#[path = "cli_proxy_runtime_tests.rs"]
mod tests;
