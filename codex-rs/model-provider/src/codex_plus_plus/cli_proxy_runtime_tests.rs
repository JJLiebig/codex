use super::*;
use pretty_assertions::assert_eq;

#[test]
fn foreign_listener_cannot_be_adopted() -> io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    listener.set_nonblocking(true)?;
    let server = std::thread::spawn(move || -> io::Result<Vec<Vec<u8>>> {
        let mut requests = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let (mut stream, _) = match listener.accept() {
                Ok(accepted) => accepted,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => return Err(error),
            };
            let mut request = [0_u8; 512];
            let length = stream.read(&mut request)?;
            if length == 0 {
                return Err(io::Error::other("empty synthetic request"));
            }
            let valid = request[..length].windows(64).any(|window| {
                window == b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            });
            stream.write_all(if valid {
                b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n"
            } else {
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n"
            })?;
            requests.push(request[..length].to_vec());
        }
        Ok(requests)
    });
    let home = tempfile::tempdir()?;
    let dir = home.path().join("cli-proxy");
    private_dir(&dir)?;
    write_state(
        &dir.join("runtime.json"),
        &RuntimeState {
            port,
            pid: 123,
            executable: PathBuf::new(),
            inference_key: "a".repeat(64),
            management_key: "b".repeat(64),
        },
    )?;
    let runtime = CliProxyRuntime::new(home.path().to_path_buf(), None);
    let result = runtime.ensure_blocking();
    let requests = server.join().expect("server thread")?;
    assert!(result.is_err(), "foreign port must be rejected");
    assert!(
        requests
            .iter()
            .all(|request| !request.windows(64).any(|window| {
                window == b"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            })),
        "foreign listener received the management secret"
    );
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn windows_owned_runtime_attaches_and_restarts() -> io::Result<()> {
    let Some(executable) = std::env::var_os("CODEX_TEST_CLI_PROXY_EXE") else {
        return Ok(());
    };
    let home = tempfile::tempdir()?;
    let runtime = CliProxyRuntime::new(home.path().to_path_buf(), Some(executable.into()));
    let (first, second) = tokio::join!(runtime.ensure(), runtime.ensure());
    let first = first?;
    let second = second?;
    assert_eq!(first.base_url, second.base_url);
    assert_eq!(first.inference_key, second.inference_key);
    let state_path = home.path().join("cli-proxy/runtime.json");
    let before = read_state(&state_path)?.expect("owned state");
    assert!(before.pid > 0);
    stop_test_proxy(before.pid)?;
    let after = runtime.ensure().await?;
    let restarted = read_state(&state_path)?.expect("restarted state");
    assert_eq!(first.base_url, after.base_url);
    assert_eq!(first.inference_key, after.inference_key);
    assert_ne!(before.pid, restarted.pid);
    stop_test_proxy(restarted.pid)?;
    Ok(())
}

#[cfg(windows)]
fn stop_test_proxy(pid: u32) -> io::Result<()> {
    let status = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(
            "could not stop synthetic CLIProxyAPI test process",
        ))
    }
}
