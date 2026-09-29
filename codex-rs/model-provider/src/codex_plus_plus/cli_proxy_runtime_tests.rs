use super::*;
use pretty_assertions::assert_eq;

#[cfg(windows)]
#[tokio::test]
async fn windows_owned_runtime_attaches_and_restarts() -> io::Result<()> {
    let Some(executable) = std::env::var_os("CODEX_TEST_CLI_PROXY_EXE") else {
        return Ok(());
    };
    let home = tempfile::tempdir()?;
    let runtime = CliProxyRuntime::new(home.path().to_path_buf(), Some(executable.into()));
    let (first, second) = tokio::join!(
        runtime.ensure(test_factory()),
        runtime.ensure(test_factory())
    );
    let first = first?;
    let second = second?;
    assert_eq!(first.base_url, second.base_url);
    assert_eq!(first.inference_key, second.inference_key);
    let state_path = home.path().join("cli-proxy/runtime.json");
    let before = read_state(&state_path)?.expect("owned state");
    assert!(before.pid > 0);
    stop_test_proxy(before.pid)?;
    // A later host can reuse the saved executable without the first host's override.
    let runtime = CliProxyRuntime::new(home.path().to_path_buf(), None);
    let after = runtime.ensure(test_factory()).await?;
    let restarted = read_state(&state_path)?.expect("restarted state");
    assert_eq!(first.base_url, after.base_url);
    assert_eq!(first.inference_key, after.inference_key);
    assert_ne!(before.pid, restarted.pid);
    assert_eq!(before.executable, restarted.executable);
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

fn test_factory() -> HttpClientFactory {
    HttpClientFactory::new(codex_http_client::OutboundProxyPolicy::ReqwestDefault)
}

#[tokio::test]
async fn foreign_listener_cannot_be_adopted_or_reused_after_reconnect() -> io::Result<()> {
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;
    use tokio::io::AsyncWriteExt;
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let home = tempfile::tempdir()?;
    let dir = home.path().join("cli-proxy");
    codex_uds::prepare_private_socket_directory(&dir).await?;
    let owned =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).map_err(io::Error::other)?;
    write_private(&dir.join("certificate.pem"), owned.cert.pem().as_bytes())?;
    write_private(
        &dir.join("private-key.pem"),
        owned.signing_key.serialize_pem().as_bytes(),
    )?;
    let config = |identity: rcgen::CertifiedKey<rcgen::KeyPair>| {
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![identity.cert.der().clone()],
                rustls_pki_types::PrivateKeyDer::from(identity.signing_key),
            )
            .map(Arc::new)
            .map_err(io::Error::other)
    };
    let foreign = config(
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).map_err(io::Error::other)?,
    )?;
    let owned = config(owned)?;
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let state = RuntimeState {
        port,
        pid: 123,
        executable: PathBuf::new(),
        inference_key: "a".repeat(64),
        management_key: "b".repeat(64),
    };
    write_state(&dir.join("runtime.json"), &state)?;
    let runtime = CliProxyRuntime::new(home.path().to_path_buf(), None);
    let client = runtime.http_client(&test_factory())?;
    let url = format!("{}/responses", state.endpoint().base_url);
    // Initial foreign attach, real endpoint, foreign takeover using the retained client.
    for (config, expected_owned) in [(foreign.clone(), false), (owned, true), (foreign, false)] {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
        let acceptor = tokio_rustls::TlsAcceptor::from(config);
        let server = tokio::spawn(async move {
            let mut received = Vec::new();
            for _ in 0..2 {
                let (stream, _) = listener.accept().await?;
                if let Ok(mut stream) = acceptor.accept(stream).await {
                    let mut request = [0_u8; 1024];
                    let len = stream.read(&mut request).await.unwrap_or(0);
                    if len == 0 {
                        continue;
                    }
                    received.extend_from_slice(&request[..len]);
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await?;
                }
            }
            Ok::<_, io::Error>(received)
        });
        assert_eq!(runtime.ensure(test_factory()).await.is_ok(), expected_owned);
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(&state.inference_key)
                .send()
                .await
                .is_ok(),
            expected_owned
        );
        let received = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .map_err(io::Error::other)?
            .map_err(io::Error::other)??;
        if expected_owned {
            assert!(
                received
                    .windows(64)
                    .any(|bytes| bytes == state.inference_key.as_bytes())
            );
            assert!(
                received
                    .windows(64)
                    .any(|bytes| bytes == state.management_key.as_bytes())
            );
        } else {
            assert_eq!(received, Vec::<u8>::new(), "foreign TLS received a bearer");
        }
    }
    Ok(())
}
