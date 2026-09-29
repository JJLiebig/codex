use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use base64::Engine;
use chrono::Utc;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AccountStore;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthRouteConfig;
use codex_login::save_auth;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::RefreshStrategy;
use pretty_assertions::assert_eq;
use tokio::sync::Notify;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;

use super::*;
use crate::provider::create_model_provider;

struct Fixture {
    home: tempfile::TempDir,
    server: MockServer,
    files: Arc<Mutex<BTreeMap<String, Value>>>,
    reject: Arc<AtomicBool>,
    uploaded: Arc<Notify>,
    tls: tokio::task::JoinHandle<()>,
    origin: url::Url,
}

impl Fixture {
    async fn new(delay: Duration) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("cli-proxy");
        codex_uds::prepare_private_socket_directory(&dir)
            .await
            .unwrap();
        let identity = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        std::fs::write(dir.join("certificate.pem"), identity.cert.pem()).unwrap();
        std::fs::write(
            dir.join("private-key.pem"),
            identity.signing_key.serialize_pem(),
        )
        .unwrap();
        let tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![identity.cert.der().clone()],
                rustls_pki_types::PrivateKeyDer::from(identity.signing_key),
            )
            .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            dir.join("runtime.json"),
            json!({
                "port": port, "pid": 1, "executable": "",
                "inference_key": "a".repeat(64), "management_key": "b".repeat(64),
            })
            .to_string(),
        )
        .unwrap();
        let server = MockServer::start().await;
        let address = *server.address();
        let tls = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let mut stream = acceptor.accept(stream).await.unwrap();
                    let mut upstream = tokio::net::TcpStream::connect(address).await.unwrap();
                    let _ = tokio::io::copy_bidirectional(&mut stream, &mut upstream).await;
                });
            }
        });
        let files = Arc::new(Mutex::new(BTreeMap::<String, Value>::new()));
        let reject = Arc::new(AtomicBool::new(false));
        let uploaded = Arc::new(Notify::new());
        let state = files.clone();
        let failure = reject.clone();
        let notification = uploaded.clone();
        Mock::given(header(
            "authorization",
            format!("Bearer {}", "b".repeat(64)),
        ))
        .respond_with(move |request: &Request| {
            let mut files = state.lock().unwrap();
            let name = request
                .url
                .query_pairs()
                .find(|(key, _)| key == "name")
                .map(|(_, value)| value.into_owned());
            match (request.method.as_str(), request.url.path()) {
                ("GET", "/v0/management/auth-files") => ResponseTemplate::new(200)
                    .insert_header("X-CPA-VERSION", "7.3.14")
                    .set_body_json(json!({"files": files.iter().map(|(name, record)| {
                            json!({"name": name, "type": record["type"], "extra": true})
                        }).collect::<Vec<_>>(), "observed_at": "ignored"})),
                ("GET", "/v0/management/auth-files/download") => {
                    match files.get(name.as_deref().unwrap()) {
                        Some(record) => ResponseTemplate::new(200).set_body_json(record),
                        None => ResponseTemplate::new(404),
                    }
                }
                ("POST", "/v0/management/auth-files") => {
                    notification.notify_one();
                    if failure.load(Ordering::SeqCst) {
                        return ResponseTemplate::new(200)
                            .set_body_json(json!({"status": "failed"}));
                    }
                    files.insert(
                        name.unwrap(),
                        serde_json::from_slice(&request.body).unwrap(),
                    );
                    ResponseTemplate::new(200)
                        .set_body_json(json!({"status": "ok"}))
                        .set_delay(delay)
                }
                ("DELETE", "/v0/management/auth-files") => {
                    files.remove(&name.unwrap());
                    ResponseTemplate::new(200).set_body_json(json!({"status": "ok"}))
                }
                _ => ResponseTemplate::new(404),
            }
        })
        .mount(&server)
        .await;
        Mock::given(header(
            "authorization",
            format!("Bearer {}", "a".repeat(64)),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": []})))
        .mount(&server)
        .await;
        Self {
            home,
            server,
            files,
            reject,
            uploaded,
            tls,
            origin: url::Url::parse(&format!("https://127.0.0.1:{port}")).unwrap(),
        }
    }

    fn runtime(&self) -> CliProxyRuntime {
        CliProxyRuntime::new(self.home.path().to_path_buf(), None)
    }

    async fn manager(&self) -> Arc<AuthManager> {
        Arc::new(
            AuthManager::new(
                self.home.path().to_path_buf(),
                /*enable_codex_api_key_env*/ false,
                AuthCredentialsStoreMode::File,
                /*forced_chatgpt_workspace_id*/ None,
                /*chatgpt_base_url*/ None,
                AuthKeyringBackendKind::default(),
                AuthRouteConfig::from_http_client_factory(
                    factory().with_network_policy(
                        factory()
                            .network_policy()
                            .clone()
                            .restrict_to_origin(self.origin.clone()),
                    ),
                ),
            )
            .await,
        )
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.tls.abort();
    }
}

fn factory() -> HttpClientFactory {
    HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
}

fn write_auth(home: &Path, label: &str) {
    let jwt = |payload: Value| {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        format!("e30.{payload}.sig")
    };
    let auth: AuthDotJson = serde_json::from_value(json!({
        "auth_mode": "chatgpt",
        "tokens": {
            "id_token": jwt(json!({"https://api.openai.com/auth": {
                "chatgpt_account_id": "upstream-a", "chatgpt_user_id": "user-a", "chatgpt_plan_type": "plus"
            }})),
            "access_token": jwt(json!({"exp": Utc::now().timestamp() + 3600, "label": label})),
            "refresh_token": "synthetic-refresh-must-not-publish", "account_id": "upstream-a"
        },
        "last_refresh": Utc::now()
    }))
    .unwrap();
    save_auth(
        home,
        &auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .unwrap();
}

#[tokio::test]
async fn setup_and_catalogue_reconcile_disk_without_resetting_unchanged_cooldown() {
    let fixture = Fixture::new(Duration::ZERO).await;
    write_auth(fixture.home.path(), "old");
    let store = AccountStore::new(fixture.home.path().to_path_buf());
    let imported = store
        .import_current(
            /*label*/ None,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
        )
        .unwrap();
    store.set_automation_enabled(&imported.id, false).unwrap();
    let manager = fixture.manager().await;
    manager
        .activate_imported_account(&imported.id)
        .await
        .unwrap();
    let provider = create_model_provider(
        ModelProviderInfo::create_cli_proxy_provider(),
        Some(manager.clone()),
    );
    let claude = json!({"type": "claude", "access_token": "synthetic-claude", "cooldown": 123});
    let unowned = json!({"type": "codex", "access_token": "synthetic-unowned"});
    fixture.files.lock().unwrap().extend([
        ("claude.json".into(), claude.clone()),
        ("manual-codex.json".into(), unowned.clone()),
    ]);
    let generation = *manager.auth_change_receiver().borrow();
    provider.api_provider().await.unwrap();
    let name = format!("codex-native-imported-{}.json", imported.id);
    let first = fixture.files.lock().unwrap()[&name].clone();
    assert_eq!(first.as_object().unwrap().len(), 6);
    assert!(first.get("refresh_token").is_none() && first.get("id_token").is_none());
    fixture.files.lock().unwrap().get_mut(&name).unwrap()["cooldown"] = json!(456);
    provider.api_auth().await.unwrap();
    assert_eq!(fixture.files.lock().unwrap()[&name]["cooldown"], json!(456));
    write_auth(
        &fixture
            .home
            .path()
            .join("accounts")
            .join(imported.id.as_str()),
        "rotated",
    );
    assert_eq!(*manager.auth_change_receiver().borrow(), generation);
    provider.runtime_base_url().await.unwrap();
    let rotated = fixture.files.lock().unwrap()[&name].clone();
    assert_ne!(rotated["access_token"], first["access_token"]);
    assert_eq!(rotated["prefix"], first["prefix"]);
    assert!(*manager.auth_change_receiver().borrow() > generation);
    assert_eq!(manager.active_account_id(), Some(imported.id.clone()));
    let index_path = fixture.home.path().join("accounts/index.json");
    let mut index: Value = serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
    index["accounts"][0]["enabled"] = json!(false);
    std::fs::write(index_path, index.to_string()).unwrap();
    let models = provider.models_manager_without_cache(/*config_model_catalog*/ None);
    models
        .raw_model_catalog(RefreshStrategy::Online, factory())
        .await;
    assert_eq!(
        *fixture.files.lock().unwrap(),
        BTreeMap::from([
            ("claude.json".into(), claude),
            ("manual-codex.json".into(), unowned),
        ])
    );
    let requests = fixture.server.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method.as_str() == "POST")
            .count(),
        2
    );
    assert!(
        requests
            .iter()
            .any(|request| request.url.path() == "/v1/models")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn publication_guards_block_native_mutation_and_stale_publishers_use_new_disk_state() {
    let fixture = Fixture::new(Duration::from_millis(300)).await;
    write_auth(fixture.home.path(), "old");
    let first_manager = fixture.manager().await;
    let stale_manager = fixture.manager().await;
    let runtime = fixture.runtime();
    let first = tokio::spawn(async move { runtime.prepare(&first_manager, factory()).await });
    tokio::time::timeout(Duration::from_secs(5), fixture.uploaded.notified())
        .await
        .unwrap();
    let home = fixture.home.path().to_path_buf();
    let mut mutation = tokio::task::spawn_blocking(move || write_auth(&home, "new"));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut mutation)
            .await
            .is_err()
    );
    first.await.unwrap().unwrap();
    mutation.await.unwrap();
    let fresh_manager = fixture.manager().await;
    let runtime = fixture.runtime();
    let (fresh, stale) = tokio::join!(
        runtime.prepare(&fresh_manager, factory()),
        runtime.prepare(&stale_manager, factory())
    );
    fresh.unwrap();
    stale.unwrap();
    let snapshot = fresh_manager.export_native_credentials().await.unwrap();
    let (_, expected) = native_record(&snapshot.credentials()[0]);
    assert_eq!(
        fixture
            .files
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>(),
        vec![expected]
    );
    drop(snapshot);
    std::fs::remove_file(fixture.home.path().join("auth.json")).unwrap();
    runtime.prepare(&stale_manager, factory()).await.unwrap();
    assert!(fixture.files.lock().unwrap().is_empty());
}

#[tokio::test]
async fn empty_native_pool_preserves_claude_and_unacknowledged_publication_rejects_setup() {
    let fixture = Fixture::new(Duration::ZERO).await;
    let claude = json!({"type": "claude", "access_token": "synthetic-claude"});
    fixture
        .files
        .lock()
        .unwrap()
        .insert("claude.json".into(), claude.clone());
    let manager = fixture.manager().await;
    let provider = create_model_provider(
        ModelProviderInfo::create_cli_proxy_provider(),
        Some(manager.clone()),
    );
    provider.api_provider().await.unwrap();
    write_auth(fixture.home.path(), "new-login");
    fixture.reject.store(true, Ordering::SeqCst);
    assert!(provider.api_provider().await.is_err());
    assert!(provider.api_auth().await.is_err());
    assert_eq!(
        *fixture.files.lock().unwrap(),
        BTreeMap::from([("claude.json".into(), claude.clone())])
    );
    fixture.reject.store(false, Ordering::SeqCst);
    provider.api_auth().await.unwrap();
    let auth_path = fixture.home.path().join("auth.json");
    let mut expired: Value = serde_json::from_slice(&std::fs::read(&auth_path).unwrap()).unwrap();
    expired["tokens"]["access_token"] = json!("e30.eyJleHAiOjF9.sig");
    std::fs::write(auth_path, expired.to_string()).unwrap();
    // The native refresh handoff sees expiry; its OAuth authority is denied by this fixture's
    // loopback-only policy. No request can leave the synthetic management listener.
    provider.api_auth().await.unwrap();
    assert_eq!(
        manager
            .auth_cached()
            .unwrap()
            .get_token_data()
            .unwrap()
            .access_token,
        "e30.eyJleHAiOjF9.sig"
    );
    assert_eq!(
        *fixture.files.lock().unwrap(),
        BTreeMap::from([("claude.json".into(), claude)])
    );
}
