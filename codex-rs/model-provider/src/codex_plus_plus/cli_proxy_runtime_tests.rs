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

#[cfg(windows)]
#[tokio::test]
async fn windows_real_proxy_acknowledges_publication_rotation_and_named_removal() -> io::Result<()>
{
    use base64::Engine;
    use codex_login::AuthCredentialsStoreMode;
    use codex_login::AuthKeyringBackendKind;
    use serde_json::json;
    let Some(executable) = std::env::var_os("CODEX_TEST_CLI_PROXY_PUBLICATION_EXE") else {
        return Ok(());
    };
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let home = tempfile::tempdir()?;
    let dir = home.path().join("cli-proxy");
    codex_uds::prepare_private_socket_directory(&dir).await?;
    codex_uds::prepare_private_socket_directory(dir.join("auth")).await?;
    let identity =
        rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).map_err(io::Error::other)?;
    write_private(&dir.join("certificate.pem"), identity.cert.pem().as_bytes())?;
    write_private(
        &dir.join("private-key.pem"),
        identity.signing_key.serialize_pem().as_bytes(),
    )?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    drop(listener);
    let state = RuntimeState {
        port,
        pid: 0,
        executable: executable.into(),
        inference_key: "a".repeat(64),
        management_key: "b".repeat(64),
    };
    write_state(&dir.join("runtime.json"), &state)?;
    write_config(&dir.join("config.yaml"), &dir, &state)?;
    let mut command = Command::new(&state.executable);
    command
        .arg("-config")
        .arg(dir.join("config.yaml"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _child = OwnedChild(command.spawn()?);
    let runtime = CliProxyRuntime::new(home.path().to_path_buf(), None);
    let client = runtime.http_client(&test_factory())?;
    let url = format!("https://127.0.0.1:{port}/v0/management/auth-files");
    let deadline = std::time::Instant::now() + STARTUP_TIMEOUT;
    loop {
        if client
            .get(&url)
            .bearer_auth(&state.management_key)
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::other("synthetic proxy startup timed out"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let write_auth = |label: &str| -> io::Result<()> {
        let jwt = |payload: serde_json::Value| {
            let payload =
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
            format!("e30.{payload}.sig")
        };
        let auth = serde_json::from_value(
            json!({"auth_mode": "chatgpt", "last_refresh": chrono::Utc::now(),
            "tokens": {"id_token": jwt(json!({"https://api.openai.com/auth": {
                "chatgpt_account_id": "synthetic-account", "chatgpt_plan_type": "plus"
            }})), "account_id": "synthetic-account", "refresh_token": "synthetic-do-not-publish",
                "access_token": jwt(json!({"exp": chrono::Utc::now().timestamp() + 3600, "label": label}))}}),
        )?;
        codex_login::save_auth(
            home.path(),
            &auth,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
    };
    write_auth("old")?;
    let manager = codex_login::AuthManager::new(
        home.path().to_path_buf(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        /*chatgpt_base_url*/ None,
        AuthKeyringBackendKind::default(),
        codex_login::AuthRouteConfig::from_http_client_factory(test_factory()),
    )
    .await;
    runtime.prepare(&manager, test_factory()).await?;
    let inventory: serde_json::Value = client
        .get(&url)
        .bearer_auth(&state.management_key)
        .send()
        .await
        .map_err(io::Error::other)?
        .json()
        .await
        .map_err(io::Error::other)?;
    let name = inventory["files"][0]["name"].as_str().unwrap();
    let download = || {
        client
            .get(format!("{url}/download"))
            .query(&[("name", name)])
            .bearer_auth(&state.management_key)
            .send()
    };
    let original: serde_json::Value = download()
        .await
        .map_err(io::Error::other)?
        .json()
        .await
        .map_err(io::Error::other)?;
    assert!(original.get("id_token").is_none() && original.get("refresh_token").is_none());
    client
        .request(http::Method::PATCH, format!("{url}/status"))
        .bearer_auth(&state.management_key)
        .json(&json!({"name": name, "disabled": true}))
        .send()
        .await
        .map_err(io::Error::other)?
        .error_for_status()
        .map_err(io::Error::other)?;
    runtime.prepare(&manager, test_factory()).await?;
    let unchanged: serde_json::Value = download()
        .await
        .map_err(io::Error::other)?
        .json()
        .await
        .map_err(io::Error::other)?;
    assert_eq!(unchanged["disabled"], json!(true));
    write_auth("rotated")?;
    runtime.prepare(&manager, test_factory()).await?;
    let rotated: serde_json::Value = download()
        .await
        .map_err(io::Error::other)?
        .json()
        .await
        .map_err(io::Error::other)?;
    assert_ne!(original["access_token"], rotated["access_token"]);
    assert_eq!(original["prefix"], rotated["prefix"]);
    assert!(rotated.get("id_token").is_none() && rotated.get("refresh_token").is_none());
    fs::remove_file(home.path().join("auth.json"))?;
    runtime.prepare(&manager, test_factory()).await?;
    let response = download().await.map_err(io::Error::other)?;
    assert_eq!(response.status(), http::StatusCode::NOT_FOUND);
    Ok(())
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
