use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AccountStore;
use codex_login::AuthConfig;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthDotJson;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::AuthRouteConfig;
use codex_login::save_auth;
use codex_model_provider_info::ModelProviderInfo;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::Request;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;

const TEST_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln";
const OTHER_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6Im90aGVyQGV4YW1wbGUuY29tIiwiZW1haWxfdmVyaWZpZWQiOnRydWUsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X3VzZXJfaWQiOiJvdGhlci11c2VyIiwidXNlcl9pZCI6Im90aGVyLXVzZXIiLCJjaGF0Z3B0X3BsYW5fdHlwZSI6InBybyIsImNoYXRncHRfYWNjb3VudF9pZCI6ImFjY291bnQtMTIzIn19.c2ln";
static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);

struct TestAccount {
    root: PathBuf,
    account_home: PathBuf,
}

impl TestAccount {
    fn new() -> Self {
        let id = NEXT_HOME.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "codex-model-provider-weekly-window-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create test home");
        write_auth(&root, "account-123", "one", "refresh-one");
        let profile = AccountStore::new(root.clone())
            .import_current(
                Some("test account".to_string()),
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )
            .expect("import account");
        let account_home = root.join("accounts").join(profile.id.as_str());
        Self { root, account_home }
    }

    fn request(&self, server: &MockServer) -> WeeklyWindowPingRequest {
        let http_client_factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
        WeeklyWindowPingRequest {
            auth_config: AuthConfig {
                codex_home: self.account_home.clone(),
                auth_credentials_store_mode: AuthCredentialsStoreMode::File,
                keyring_backend_kind: AuthKeyringBackendKind::default(),
                automatic_account_selection: Default::default(),
                forced_login_method: None,
                chatgpt_base_url: Some(server.uri()),
                forced_chatgpt_workspace_id: None,
                managed_auth_policy: Default::default(),
                auth_route_config: AuthRouteConfig::from_http_client_factory(
                    http_client_factory.clone(),
                ),
            },
            model_provider_id: OPENAI_PROVIDER_ID.to_string(),
            model_provider: ModelProviderInfo::create_openai_provider(/*base_url*/ None),
            chatgpt_base_url: server.uri(),
            http_client_factory,
        }
    }
}

impl Drop for TestAccount {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write_auth(home: &std::path::Path, account_id: &str, token: &str, refresh: &str) {
    write_auth_with_id_token(home, account_id, token, refresh, TEST_ID_TOKEN);
}

fn write_auth_with_id_token(
    home: &std::path::Path,
    account_id: &str,
    token: &str,
    refresh: &str,
    id_token: &str,
) {
    let auth: AuthDotJson = serde_json::from_value(json!({
        "OPENAI_API_KEY": null,
        "tokens": {"id_token": id_token, "access_token": token,
            "refresh_token": refresh, "account_id": account_id},
        "last_refresh": "2099-01-01T00:00:00Z"
    }))
    .expect("valid test auth");
    save_auth(
        home,
        &auth,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )
    .expect("save test auth");
}

fn completed_response() -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(
            "event: response.completed\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp-1\"}}\n\n",
        )
}

async fn mount_models(server: &MockServer, slug: &str) {
    let mut response = codex_models_manager::bundled_models_response().expect("bundled models");
    response
        .models
        .retain(|model| model.visibility == codex_protocol::openai_models::ModelVisibility::List);
    response.models.truncate(1);
    response.models[0].slug = slug.to_string();
    response.models[0].priority = 0;
    Mock::given(method("GET"))
        .and(path("/codex/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(response))
        .mount(server)
        .await;
}

async fn activation_requests(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("received requests")
        .into_iter()
        .filter(|request| request.url.path() == "/codex/responses")
        .collect()
}

#[tokio::test]
async fn sends_exact_request_without_mutating_auth_or_exposing_it_to_custom_providers() {
    let server = MockServer::start().await;
    mount_models(&server, "account-default").await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(|request: &Request| {
            let body: serde_json::Value =
                serde_json::from_slice(&request.body).expect("JSON request");
            if body.get("max_output_tokens").is_none()
                && body["input"][0]["content"][0]["text"] == "Reply ACK only."
            {
                completed_response()
            } else {
                ResponseTemplate::new(400).set_body_string("secret backend rejection body")
            }
        })
        .mount(&server)
        .await;
    let account = TestAccount::new();
    let foreground = AuthManager::new(
        account.root.clone(),
        /*enable_codex_api_key_env*/ false,
        AuthCredentialsStoreMode::File,
        /*forced_chatgpt_workspace_id*/ None,
        Some(server.uri()),
        AuthKeyringBackendKind::default(),
        AuthRouteConfig::from_http_client_factory(HttpClientFactory::new(
            OutboundProxyPolicy::ReqwestDefault,
        )),
    )
    .await;
    let foreground_id = foreground.active_account_id();
    let root_auth = std::fs::read(account.root.join("auth.json")).expect("root auth");

    assert_eq!(
        ping_weekly_window_with_timeout(account.request(&server), PING_TIMEOUT).await,
        WeeklyWindowPingOutcome::Completed
    );
    let requests = activation_requests(&server).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&requests[0].body).expect("JSON body"),
        json!({"model": "account-default", "instructions": "", "input": [{"type": "message",
            "role": "user", "content": [{"type": "input_text", "text": "Reply ACK only."}]}],
            "store": false, "stream": true, "include": []})
    );
    assert_eq!(
        requests[0]
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Bearer one")
    );
    assert_eq!(
        std::fs::read(account.root.join("auth.json")).unwrap(),
        root_auth
    );
    assert_eq!(foreground.active_account_id(), foreground_id);

    let mut custom_request = account.request(&server);
    custom_request.model_provider_id = "custom".to_string();
    let mut override_request = account.request(&server);
    override_request.model_provider.base_url = Some(server.uri());
    let mut invalid_auth_request = account.request(&server);
    invalid_auth_request.auth_config.codex_home = account.root.join("invalid-auth");
    std::fs::create_dir_all(
        invalid_auth_request
            .auth_config
            .codex_home
            .join("auth.json"),
    )
    .unwrap();
    for request in [custom_request, override_request] {
        assert_eq!(
            ping_weekly_window(request).await,
            WeeklyWindowPingOutcome::LocalSetup
        );
    }
    assert_eq!(
        ping_weekly_window_with_timeout(invalid_auth_request, PING_TIMEOUT).await,
        WeeklyWindowPingOutcome::LocalSetup
    );

    server.reset().await;
    let mut configured_url_request = account.request(&server);
    configured_url_request.chatgpt_base_url = "https://attacker.example/backend-api".to_string();
    assert_eq!(
        preflight_weekly_window_ping(
            &configured_url_request.model_provider_id,
            &configured_url_request.model_provider,
            &configured_url_request.chatgpt_base_url,
            &configured_url_request.http_client_factory,
        ),
        Err(WeeklyWindowPingOutcome::UnsupportedConfiguration)
    );
    assert_eq!(
        ping_weekly_window(configured_url_request).await,
        WeeklyWindowPingOutcome::UnsupportedConfiguration
    );

    let mut routed_request = account.request(&server);
    routed_request.chatgpt_base_url = ChatGptEnvironment::default().chatgpt_base_url().to_string();
    routed_request.http_client_factory =
        HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy);
    assert_eq!(
        ping_weekly_window(routed_request).await,
        WeeklyWindowPingOutcome::UnsupportedRouting
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 0);
}

#[tokio::test]
async fn unauthorized_recovery_retries_only_identity_preserving_reloads() {
    let server = MockServer::start().await;
    mount_models(&server, "account-default").await;
    let account = TestAccount::new();
    let account_home = account.account_home.clone();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer one"))
        .respond_with(move |_request: &Request| {
            write_auth(&account_home, "account-123", "two", "refresh-two");
            ResponseTemplate::new(401)
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .and(header("authorization", "Bearer two"))
        .respond_with(completed_response())
        .mount(&server)
        .await;
    assert_eq!(
        ping_weekly_window_with_timeout(account.request(&server), PING_TIMEOUT).await,
        WeeklyWindowPingOutcome::Completed
    );
    assert_eq!(activation_requests(&server).await.len(), 2);

    server.reset().await;
    write_auth(&account.account_home, "account-123", "one", "refresh-one");
    let account_home = account.account_home.clone();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(move |_request: &Request| {
            write_auth_with_id_token(
                &account_home,
                "account-123",
                "other",
                "other-refresh",
                OTHER_ID_TOKEN,
            );
            ResponseTemplate::new(401)
        })
        .mount(&server)
        .await;
    assert_eq!(
        ping_weekly_window_with_timeout(account.request(&server), PING_TIMEOUT).await,
        WeeklyWindowPingOutcome::AuthenticationRecovery
    );
    assert!(!AccountStore::new(account.root.clone()).list().unwrap()[0].login_required);
    assert_eq!(activation_requests(&server).await.len(), 1);
}

#[tokio::test]
async fn ambiguous_outcomes_are_not_replayed_and_the_attempt_is_bounded() {
    assert_eq!(
        [
            ApiError::UsageNotIncluded,
            ApiError::RateLimitExceeded {
                message: "retry later".to_string(),
                retry_after: None,
            },
            ApiError::FlexUnavailable,
        ]
        .map(|error| classify_error(&error)),
        [WeeklyWindowPingOutcome::Rejected { status: None }; 3]
    );

    let server = MockServer::start().await;
    let account = TestAccount::new();
    Mock::given(method("POST"))
        .and(path("/codex/responses"))
        .respond_with(ResponseTemplate::new(422).set_body_string("secret backend rejection body"))
        .mount(&server)
        .await;
    assert_eq!(
        ping_weekly_window_with_timeout(account.request(&server), PING_TIMEOUT).await,
        WeeklyWindowPingOutcome::Rejected { status: Some(422) }
    );

    for status in [500, 408] {
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        assert_eq!(
            ping_weekly_window_with_timeout(account.request(&server), PING_TIMEOUT).await,
            WeeklyWindowPingOutcome::Ambiguous {
                status: Some(status),
            }
        );
        assert_eq!(activation_requests(&server).await.len(), 1);
    }

    for response in [
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string("event: response.created\ndata: {\"type\":\"response.created\"}\n\n"),
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string("event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{\"error\":{\"code\":\"server_is_overloaded\"}}}\n\n"),
        completed_response().set_delay(Duration::from_secs(1)),
    ] {
        server.reset().await;
        Mock::given(method("POST"))
            .and(path("/codex/responses"))
            .respond_with(response)
            .mount(&server)
            .await;
        assert_eq!(
            ping_weekly_window_with_timeout(account.request(&server), Duration::from_millis(10))
                .await,
            WeeklyWindowPingOutcome::Ambiguous { status: None }
        );
    }
}
