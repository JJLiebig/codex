use super::*;
use clap::Parser;
use pretty_assertions::assert_eq;

#[test]
fn recovery_retains_options_without_replaying_launch_input() {
    let mut cli = Cli::parse_from([
        "codex",
        "--no-daemon",
        "-m",
        "test-model",
        "--cd",
        "/old",
        "--worktree",
        "--image",
        "old.png",
        "--",
        "secret prompt",
    ]);
    cli.config_overrides
        .raw_overrides
        .push("model_reasoning_effort=high".into());
    let args = launch_arguments(&cli, Path::new("codex-native"));
    assert_eq!(
        args,
        vec![
            "codex-native",
            "--no-daemon",
            "--model",
            "test-model",
            "--config",
            "model_reasoning_effort=high"
        ]
    );
}

#[tokio::test]
async fn rpc_accepts_extra_fields_and_rejects_core_errors() {
    for (reply, success) in [
        (
            json!({"jsonrpc":"2.0", "id":1, "result":{"keep":true}, "extra":true}),
            true,
        ),
        (
            json!({"jsonrpc":"2.0", "id":1, "error":{"code":-32601}}),
            false,
        ),
        (json!({"jsonrpc":"2.0", "id":2, "result":{}}), false),
    ] {
        let (client, server) = tokio::io::duplex(4096);
        let peer = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            let mut request = String::new();
            server.read_line(&mut request).await.unwrap();
            assert_eq!(
                serde_json::from_str::<Value>(&request).unwrap()["method"],
                "terminal.get"
            );
            server
                .write_all(format!("{reply}\n").as_bytes())
                .await
                .unwrap();
        });
        assert_eq!(
            exchange(client, "terminal.get", json!({})).await.is_ok(),
            success
        );
        peer.await.unwrap();
    }
}

// Use a real local control socket: exercise keep gating, latest-session replacement and cleanup.
#[tokio::test]
async fn kept_terminal_tracks_main_session_and_disarms_only_on_request() {
    exercise_worker(true, true, true).await;
    exercise_worker(true, true, false).await;
    exercise_worker(true, false, true).await;
    exercise_worker(false, true, true).await;
}

async fn exercise_worker(kept: bool, deliberate: bool, supported: bool) {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    let socket = root.path().join("core.sock").to_string_lossy().into_owned();
    #[cfg(windows)]
    let socket = format!(r"\\.\pipe\codex-herdr-test-{}", uuid::Uuid::new_v4());
    #[cfg(unix)]
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    #[cfg(windows)]
    let mut listener = tokio::net::windows::named_pipe::ServerOptions::new()
        .create(&socket)
        .unwrap();
    let (requests_tx, mut requests_rx) = tokio::sync::mpsc::unbounded_channel();
    #[cfg(windows)]
    let server_socket = socket.clone();
    let peer = tokio::spawn(async move {
        loop {
            #[cfg(unix)]
            let (stream, _) = listener.accept().await.unwrap();
            #[cfg(windows)]
            let stream = {
                listener.connect().await.unwrap();
                let next = tokio::net::windows::named_pipe::ServerOptions::new()
                    .create(&server_socket)
                    .unwrap();
                std::mem::replace(&mut listener, next)
            };
            let mut stream = BufReader::new(stream);
            let mut line = String::new();
            stream.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let result = match request["method"].as_str().unwrap() {
                "terminal.get" => json!({"keep":kept, "state":"live"}),
                "rpc.discover" => {
                    json!({"methods":[{"name":"terminal.keep", "params":[{"name":"recovery"},{"name":"expected_scope"}]}]})
                }
                "rpc.discover" => json!({"methods":[]}),
                "terminal.keep" => json!({}),
                other => panic!("unexpected method: {other}"),
            };
            stream
                .write_all(format!("{}\n", json!({"id":1, "result":result})).as_bytes())
                .await
                .unwrap();
            requests_tx.send(request).unwrap();
        }
    });
    let (tx, rx) = watch::channel(Update::Pending);
    let task = tokio::spawn(run(
        socket,
        "t1".into(),
        "scope".into(),
        vec!["codex.exe".into(), "--no-daemon".into()],
        rx,
    ));
    let second_cwd = root.path().join("second");
    std::fs::create_dir(&second_cwd).unwrap();
    for (id, cwd) in [
        ("main-a", root.path()),
        ("main-b", root.path()),
        ("main-b", second_cwd.as_path()),
    ] {
        tx.send_replace(Update::Session {
            id: id.into(),
            cwd: cwd.to_string_lossy().into_owned(),
        });
        let request = tokio::time::timeout(Duration::from_secs(2), requests_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request["method"], "terminal.get");
        if kept {
            assert_eq!(requests_rx.recv().await.unwrap()["method"], "rpc.discover");
            if !supported {
                continue;
            }
            let registration = requests_rx.recv().await.unwrap();
            assert_eq!(
                registration["params"]["recovery"],
                json!({
                    "argv":["codex.exe", "--no-daemon", "resume", id, "--cd", cwd], "cwd":cwd
                })
            );
            // Losing the main attachment must disarm a deleted/reset thread and permit rearming.
            tx.send_replace(Update::Pending);
            assert_eq!(requests_rx.recv().await.unwrap()["method"], "terminal.get");
            assert_eq!(requests_rx.recv().await.unwrap()["params"]["keep"], false);
            assert_eq!(requests_rx.recv().await.unwrap()["params"]["keep"], true);
            tx.send_replace(Update::Session {
                id: id.into(),
                cwd: cwd.to_string_lossy().into_owned(),
            });
            assert_eq!(requests_rx.recv().await.unwrap()["method"], "terminal.get");
            assert_eq!(requests_rx.recv().await.unwrap()["method"], "rpc.discover");
            assert_eq!(requests_rx.recv().await.unwrap()["method"], "terminal.keep");
        }
    }
    if deliberate {
        tx.send_replace(Update::Quit);
    }
    drop(tx);
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    if kept && deliberate && supported {
        assert_eq!(requests_rx.recv().await.unwrap()["method"], "terminal.get");
        assert_eq!(requests_rx.recv().await.unwrap()["params"]["keep"], false);
        assert_eq!(requests_rx.recv().await.unwrap()["params"]["keep"], true);
    }
    assert!(requests_rx.try_recv().is_err());
    peer.abort();
}
