//! Isolated owned TLS endpoint using the same management contract as provider preparation.
use super::*;
use serde_json::Value;

pub(super) struct OwnedFixture {
    pub home: TempDir,
    pub server: MockServer,
    pub manager: Arc<AuthManager>,
    pub files: Arc<Mutex<BTreeMap<String, Value>>>,
    tls: tokio::task::JoinHandle<()>,
}

impl OwnedFixture {
    pub async fn new() -> anyhow::Result<Self> {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let home = tempfile::tempdir()?;
        let dir = home.path().join("cli-proxy");
        codex_uds::prepare_private_socket_directory(&dir).await?;
        let identity = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])?;
        std::fs::write(dir.join("certificate.pem"), identity.cert.pem())?;
        std::fs::write(
            dir.join("private-key.pem"),
            identity.signing_key.serialize_pem(),
        )?;
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![identity.cert.der().clone()],
                rustls_pki_types::PrivateKeyDer::from(identity.signing_key),
            )?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        std::fs::write(
            dir.join("runtime.json"),
            json!({
                "port": port, "pid": 1, "executable": "",
                "inference_key": "a".repeat(64), "management_key": "b".repeat(64),
            })
            .to_string(),
        )?;
        let server = MockServer::start().await;
        let address = *server.address();
        let tls = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut stream) = acceptor.accept(stream).await else {
                        return;
                    };
                    let Ok(mut upstream) = tokio::net::TcpStream::connect(address).await else {
                        return;
                    };
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
                });
            }
        });
        let files = Arc::new(Mutex::new(BTreeMap::from([(
            "claude.json".into(),
            json!({"type":"claude"}),
        )])));
        let state = files.clone();
        Mock::given(wiremock::matchers::header("authorization", format!("Bearer {}", "b".repeat(64))))
            .respond_with(move |request: &wiremock::Request| {
                let mut files = state.lock().unwrap();
                let name = request.url.query_pairs().find(|(key, _)| key == "name").map(|(_, value)| value.into_owned());
                match (request.method.as_str(), request.url.path()) {
                    ("GET", "/v0/management/auth-files") => ResponseTemplate::new(200)
                        .insert_header("X-CPA-VERSION", "7.3.14")
                        .set_body_json(json!({"files": files.iter().map(|(name, record)| {
                            json!({"name": name, "type": record["type"], "auth_index": if name == "claude.json" {"0000000000000002"} else if record["account_id"] == "upstream-b" {"0000000000000003"} else {"0000000000000001"}, "disabled": false})
                        }).collect::<Vec<_>>()})),
                    ("GET", "/v0/management/auth-files/download") => files.get(name.as_deref().unwrap()).map_or(ResponseTemplate::new(404), |record| ResponseTemplate::new(200).set_body_json(record)),
                    ("GET", "/v0/management/auth-files/models") => {
                        let models: Vec<_> = files.get(name.as_deref().unwrap()).map(|record| {
                            match record["prefix"].as_str() {
                                Some(prefix) => json!({"id":format!("{prefix}/future-9.7"),"type":"openai","owned_by":"openai"}),
                                None => json!({"id":"claude-new","type":"claude","owned_by":"anthropic"}),
                            }
                        }).into_iter().collect();
                        ResponseTemplate::new(200).set_body_json(json!({"models":models}))
                    }
                    ("POST", "/v0/management/auth-files") => {
                        files.insert(name.unwrap(), serde_json::from_slice(&request.body).unwrap());
                        ResponseTemplate::new(200).set_body_json(json!({"status":"ok"}))
                    }
                    ("DELETE", "/v0/management/auth-files") => {
                        files.remove(&name.unwrap());
                        ResponseTemplate::new(200).set_body_json(json!({"status":"ok"}))
                    }
                    _ => ResponseTemplate::new(404),
                }
            }).mount(&server).await;
        Mock::given(path("/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[]})))
            .mount(&server)
            .await;
        let jwt = |payload: Value| {
            format!(
                "e30.{}.sig",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string())
            )
        };
        codex_login::save_auth(
            home.path(),
            &serde_json::from_value(json!({
                "auth_mode":"chatgpt", "tokens":{
                    "id_token":jwt(json!({"https://api.openai.com/auth":{"chatgpt_account_id":"upstream-a","chatgpt_user_id":"user-a","chatgpt_plan_type":"plus"}})),
                    "access_token":jwt(json!({"exp":chrono::Utc::now().timestamp()+3600})),
                    "refresh_token":"synthetic-refresh-must-not-publish", "account_id":"upstream-a"
                }, "last_refresh":chrono::Utc::now()
            }))?,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )?;
        let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
        let manager = Arc::new(
            AuthManager::new(
                home.path().to_path_buf(),
                /*enable_codex_api_key_env*/ false,
                AuthCredentialsStoreMode::File,
                /*forced_chatgpt_workspace_id*/ None,
                /*chatgpt_base_url*/ None,
                AuthKeyringBackendKind::default(),
                codex_login::AuthRouteConfig::from_http_client_factory(
                    factory.clone().with_network_policy(
                        factory
                            .network_policy()
                            .clone()
                            .restrict_to_origin(url::Url::parse(&format!(
                                "https://127.0.0.1:{port}"
                            ))?),
                    ),
                ),
            )
            .await,
        );
        Ok(Self {
            home,
            server,
            manager,
            files,
            tls,
        })
    }
    pub async fn import_pair(&self) -> anyhow::Result<Vec<codex_login::AccountId>> {
        let store = AccountStore::new(self.home.path().to_path_buf());
        let mut auth: codex_login::auth::AuthDotJson =
            serde_json::from_slice(&std::fs::read(self.home.path().join("auth.json"))?)?;
        let mut ids = Vec::new();
        for id in ["upstream-a", "upstream-b"] {
            auth.tokens.as_mut().unwrap().account_id = Some(id.into());
            codex_login::save_auth(
                self.home.path(),
                &auth,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )?;
            ids.push(
                store
                    .import_current(
                        None,
                        AuthCredentialsStoreMode::File,
                        AuthKeyringBackendKind::default(),
                    )?
                    .id,
            );
        }
        self.manager.activate_imported_account(&ids[0]).await?;
        Ok(ids)
    }
}

impl Drop for OwnedFixture {
    fn drop(&mut self) {
        self.tls.abort();
    }
}
