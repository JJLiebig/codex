use std::path::Path;

use anyhow::Result;
use app_test_support::ChatGptAuthFixture;
use app_test_support::TestAppServer;
use app_test_support::write_chatgpt_auth;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditOutcome;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditParams;
use codex_app_server_protocol::ConsumeAccountRateLimitResetCreditResponse;
use codex_app_server_protocol::GetAccountParams;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::LoginAccountResponse;
use codex_app_server_protocol::RequestId;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AccountStore;
use codex_login::AuthKeyringBackendKind;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_json;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(/*secs*/ 10);
const RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR: &str =
    "CODEX_TEST_RATE_LIMIT_RESET_REQUEST_TIMEOUT_MS";
const SERVER_TIMEOUT_READ_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(/*secs*/ 15);
const INVALID_REQUEST_ERROR_CODE: i64 = -32600;
const INTERNAL_ERROR_CODE: i64 = -32603;

#[tokio::test]
async fn consume_rate_limit_reset_credit_requires_chatgpt_auth() -> Result<()> {
    let codex_home = TempDir::new()?;
    let mut mcp = initialized_app_server(codex_home.path()).await?;

    let consume_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                idempotency_key: "request-1".to_string(),
                credit_id: None,
            },
        )
        .await?;
    let consume_error = read_error_response(&mut mcp, consume_id).await?;
    assert_eq!(consume_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "codex account authentication required for rate limit reset credits"
    );

    login_with_api_key(&mut mcp, "sk-test-key").await?;
    let consume_id = send_consume_reset_credit(&mut mcp, "request-2").await?;
    let consume_error = read_error_response(&mut mcp, consume_id).await?;
    assert_eq!(consume_error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "chatgpt authentication required for rate limit reset credits"
    );
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_maps_backend_outcomes() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    let cases = [
        (
            "request-reset",
            "reset",
            ConsumeAccountRateLimitResetCreditOutcome::Reset,
            2,
        ),
        (
            "request-nothing",
            "nothing_to_reset",
            ConsumeAccountRateLimitResetCreditOutcome::NothingToReset,
            0,
        ),
        (
            "request-no-credit",
            "no_credit",
            ConsumeAccountRateLimitResetCreditOutcome::NoCredit,
            0,
        ),
        (
            "request-retry",
            "already_redeemed",
            ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed,
            0,
        ),
    ];
    for (idempotency_key, backend_code, _, windows_reset) in cases {
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .and(header("authorization", "Bearer chatgpt-token"))
            .and(header("chatgpt-account-id", "account-123"))
            .and(body_json(json!({ "redeem_request_id": idempotency_key })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "code": backend_code,
                "windows_reset": windows_reset
            })))
            .mount(&server)
            .await;
    }

    let mut mcp = initialized_app_server(codex_home.path()).await?;
    for (idempotency_key, _, expected_outcome, _) in cases {
        assert_eq!(
            consume_reset_credit(&mut mcp, idempotency_key).await?,
            ConsumeAccountRateLimitResetCreditResponse {
                outcome: expected_outcome,
            }
        );
    }
    Ok(())
}

#[tokio::test]
async fn chatgpt_auth_without_local_identity_can_still_consume() -> Result<()> {
    let codex_home = TempDir::new()?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token"),
        AuthCredentialsStoreMode::File,
    )?;
    let server = MockServer::start().await;
    write_chatgpt_base_url(codex_home.path(), &server.uri())?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("authorization", "Bearer chatgpt-token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": "reset", "windows_reset": 2 })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut mcp = initialized_app_server(codex_home.path()).await?;
    assert_eq!(
        consume_reset_credit(&mut mcp, "request-no-local-identity").await?,
        ConsumeAccountRateLimitResetCreditResponse {
            outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
        }
    );
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_forwards_selected_credit_id() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(header("authorization", "Bearer chatgpt-token"))
        .and(header("chatgpt-account-id", "account-123"))
        .and(body_json(json!({
            "redeem_request_id": "request-selected",
            "credit_id": "credit-123",
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": "reset", "windows_reset": 2 })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut mcp = initialized_app_server(codex_home.path()).await?;
    let request_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                idempotency_key: "request-selected".to_string(),
                credit_id: Some("credit-123".to_string()),
            },
        )
        .await?;

    assert_eq!(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_response::<ConsumeAccountRateLimitResetCreditResponse>(request_id),
        )
        .await??,
        ConsumeAccountRateLimitResetCreditResponse {
            outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
        }
    );
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_rejects_empty_idempotency_key() -> Result<()> {
    let (codex_home, _server) = chatgpt_test_context().await?;
    let mut mcp = initialized_app_server(codex_home.path()).await?;

    let request_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                idempotency_key: String::new(),
                credit_id: None,
            },
        )
        .await?;
    let error = read_error_response(&mut mcp, request_id).await?;

    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "idempotencyKey must not be empty");
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_rejects_empty_credit_id() -> Result<()> {
    let (codex_home, _server) = chatgpt_test_context().await?;
    let mut mcp = initialized_app_server(codex_home.path()).await?;

    let request_id = mcp
        .send_consume_account_rate_limit_reset_credit_request(
            ConsumeAccountRateLimitResetCreditParams {
                idempotency_key: "request-1".to_string(),
                credit_id: Some(String::new()),
            },
        )
        .await?;
    let error = read_error_response(&mut mcp, request_id).await?;

    assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
    assert_eq!(error.error.message, "creditId must not be empty");
    Ok(())
}

#[tokio::test]
async fn consume_account_rate_limit_reset_credit_surfaces_backend_failure() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;

    let mut mcp = initialized_app_server(codex_home.path()).await?;
    let request_id = send_consume_reset_credit(&mut mcp, "request-1").await?;
    let error = read_error_response(&mut mcp, request_id).await?;

    assert_eq!(error.error.code, INTERNAL_ERROR_CODE);
    assert!(
        error
            .error
            .message
            .contains("failed to consume rate limit reset"),
        "unexpected error message: {}",
        error.error.message
    );
    Ok(())
}

#[tokio::test]
async fn consume_timeout_releases_account_auth_queue() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    Mock::given(method("GET"))
        .and(path("/api/codex/accounts/check"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"accounts": [{
                "id": "account-123", "workspace_backend_origin": "https://chatgpt.com",
                "account_routing_override": "NO_CONSTRAINT"
            }]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(/*secs*/ 1))
                .set_body_json(json!({ "code": "reset", "windows_reset": 2 })),
        )
        .mount(&server)
        .await;

    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            (RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR, Some("100")),
        ])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await?;
    let consume_id = send_consume_reset_credit(&mut mcp, "request-timeout").await?;
    let account_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;

    let consume_error: JSONRPCError = timeout(
        SERVER_TIMEOUT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(consume_id)),
    )
    .await??;
    assert_eq!(consume_error.error.code, INTERNAL_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "rate limit reset consume timed out"
    );

    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(account_id)),
    )
    .await??;
    Ok(())
}

#[tokio::test]
async fn reset_lease_contention_times_out_without_blocking_server() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    app_test_support::mount_workspace_routing(&server).await;
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let account = store.import_current(
        /*label*/ None,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let _reset_lease = store.acquire_reset_mutation_lease(&account.id)?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .without_auto_env()
        .with_env_overrides(&[
            ("OPENAI_API_KEY", None),
            (RATE_LIMIT_RESET_REQUEST_TIMEOUT_ENV_VAR, Some("100")),
        ])
        .build()
        .await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize()).await??;
    let consume_id = send_consume_reset_credit(&mut mcp, "request-lock-timeout").await?;
    let account_id = mcp
        .send_get_account_request(GetAccountParams {
            refresh_token: false,
        })
        .await?;

    let consume_error: JSONRPCError = timeout(
        SERVER_TIMEOUT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(consume_id)),
    )
    .await??;
    assert_eq!(consume_error.error.code, INTERNAL_ERROR_CODE);
    assert_eq!(
        consume_error.error.message,
        "rate limit reset consume timed out"
    );
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(account_id)),
    )
    .await??;
    Ok(())
}

#[tokio::test]
async fn imported_account_consume_waits_for_shared_reset_lease() -> Result<()> {
    let (codex_home, server) = chatgpt_test_context().await?;
    let store = AccountStore::new(codex_home.path().to_path_buf());
    let account = store.import_current(
        /*label*/ None,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let reset_lease = store.acquire_reset_mutation_lease(&account.id)?;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "code": "reset", "windows_reset": 2 })),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut mcp = initialized_app_server(codex_home.path()).await?;
    let request_id = send_consume_reset_credit(&mut mcp, "request-locked").await?;
    assert!(
        timeout(
            std::time::Duration::from_millis(/*millis*/ 100),
            mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
        )
        .await
        .is_err()
    );
    drop(reset_lease);
    assert_eq!(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_response::<ConsumeAccountRateLimitResetCreditResponse>(request_id),
        )
        .await??,
        ConsumeAccountRateLimitResetCreditResponse {
            outcome: ConsumeAccountRateLimitResetCreditOutcome::Reset,
        }
    );
    Ok(())
}

async fn chatgpt_test_context() -> Result<(TempDir, MockServer)> {
    let codex_home = TempDir::new()?;
    write_chatgpt_auth(
        codex_home.path(),
        ChatGptAuthFixture::new("chatgpt-token")
            .account_id("account-123")
            .plan_type("pro"),
        AuthCredentialsStoreMode::File,
    )?;
    let server = MockServer::start().await;
    write_chatgpt_base_url(codex_home.path(), &server.uri())?;
    Ok((codex_home, server))
}

async fn initialized_app_server(codex_home: &Path) -> Result<TestAppServer> {
    TestAppServer::builder()
        .with_codex_home(codex_home)
        .without_auto_env()
        .with_env_overrides(&[("OPENAI_API_KEY", None)])
        .build_initialized_with_timeout(DEFAULT_READ_TIMEOUT)
        .await
}

async fn consume_reset_credit(
    mcp: &mut TestAppServer,
    idempotency_key: &str,
) -> Result<ConsumeAccountRateLimitResetCreditResponse> {
    let request_id = send_consume_reset_credit(mcp, idempotency_key).await?;
    timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(request_id)).await?
}

async fn send_consume_reset_credit(mcp: &mut TestAppServer, idempotency_key: &str) -> Result<i64> {
    mcp.send_consume_account_rate_limit_reset_credit_request(
        ConsumeAccountRateLimitResetCreditParams {
            idempotency_key: idempotency_key.to_string(),
            credit_id: None,
        },
    )
    .await
}

async fn read_error_response(mcp: &mut TestAppServer, request_id: i64) -> Result<JSONRPCError> {
    let error = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(request_id)),
    )
    .await??;
    Ok(error)
}

async fn login_with_api_key(mcp: &mut TestAppServer, api_key: &str) -> Result<()> {
    let request_id = mcp.send_login_account_api_key_request(api_key).await?;
    assert_eq!(
        timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_response::<LoginAccountResponse>(request_id),
        )
        .await??,
        LoginAccountResponse::ApiKey {}
    );
    Ok(())
}

fn write_chatgpt_base_url(codex_home: &Path, base_url: &str) -> std::io::Result<()> {
    std::fs::write(
        codex_home.join("config.toml"),
        format!("chatgpt_base_url = \"{base_url}\"\n"),
    )
}

#[tokio::test]
async fn owned_manual_completion_requires_a_new_reset_or_known_pending_retry() -> Result<()> {
    const TOKEN: &str = "e30.eyJleHAiOjQxMDI0NDQ4MDB9.c2ln";
    for (source, retry_key, retry_credit) in [
        (
            codex_login::ResetCredentialSource::Root,
            "reopened",
            "different",
        ),
        (
            codex_login::ResetCredentialSource::Imported,
            "unknown",
            "original",
        ),
    ] {
        let home = TempDir::new()?;
        let server = MockServer::start().await;
        write_chatgpt_auth(
            home.path(),
            ChatGptAuthFixture::new(TOKEN).account_id("account-123"),
            AuthCredentialsStoreMode::File,
        )?;
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "model_provider = \"cli-proxy\"\nchatgpt_base_url = \"{}\"\n",
                server.uri()
            ),
        )?;
        Mock::given(method("GET"))
            .and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(503))
            .expect(4)
            .mount(&server)
            .await;
        let failed = std::sync::atomic::AtomicBool::new(false);
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .and(header("authorization", format!("Bearer {TOKEN}")))
            .and(header("chatgpt-account-id", "account-123"))
            .respond_with(move |request: &wiremock::Request| {
                let body: serde_json::Value = request.body_json().unwrap();
                let key = body["redeem_request_id"].as_str().unwrap();
                if key == "unknown" && !failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return ResponseTemplate::new(500);
                }
                let code = if key == "new" {
                    "reset"
                } else {
                    "already_redeemed"
                };
                ResponseTemplate::new(200).set_body_json(json!({"code": code, "windows_reset": 2}))
            })
            .expect(6)
            .mount(&server)
            .await;
        let store = AccountStore::new(home.path().into());
        if source == codex_login::ResetCredentialSource::Imported {
            store.import_current(
                /*label*/ None,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )?;
        }
        let id = serde_json::from_value(json!("acct_ed3ee2fed195b138"))?;
        let mut app = initialized_app_server(home.path()).await?;
        assert_eq!(
            consume_reset_credit(&mut app, "new").await?.outcome,
            ConsumeAccountRateLimitResetCreditOutcome::Reset
        );
        let first = store.acquire_reset_mutation_lease(&id)?.state()?;
        let completion = first.completion.as_ref().unwrap();
        assert_eq!(
            first,
            codex_login::ResetState {
                phase: None,
                completion: Some(codex_login::ResetCompletion {
                    id: "new".into(),
                    completed_at: completion.completed_at,
                    manual: true,
                    source: Some(source),
                    reconciliation: codex_login::ResetReconciliation::Pending,
                }),
            }
        );
        // Replayed Reset and an untracked old AlreadyRedeemed cannot freshen completion evidence.
        for key in ["new", "old"] {
            consume_reset_credit(&mut app, key).await?;
            assert_eq!(store.acquire_reset_mutation_lease(&id)?.state()?, first);
        }
        let request = app
            .send_consume_account_rate_limit_reset_credit_request(
                ConsumeAccountRateLimitResetCreditParams {
                    idempotency_key: "unknown".into(),
                    credit_id: Some("original".into()),
                },
            )
            .await?;
        assert_eq!(
            read_error_response(&mut app, request).await?.error.code,
            INTERNAL_ERROR_CODE
        );
        assert_eq!(
            store.acquire_reset_mutation_lease(&id)?.state()?,
            codex_login::ResetState {
                phase: Some(codex_login::ResetAttemptPhase::ManualRedeeming {
                    redeem_request_id: "unknown".into(),
                    credit_id: Some("original".into()),
                    source,
                }),
                completion: first.completion,
            }
        );
        drop(app);
        let mut app = initialized_app_server(home.path()).await?;
        let retry = app
            .send_consume_account_rate_limit_reset_credit_request(
                ConsumeAccountRateLimitResetCreditParams {
                    idempotency_key: retry_key.into(),
                    credit_id: Some(retry_credit.into()),
                },
            )
            .await?;
        if retry_key == "unknown" {
            let response: ConsumeAccountRateLimitResetCreditResponse =
                timeout(DEFAULT_READ_TIMEOUT, app.read_response(retry)).await??;
            assert_eq!(
                response.outcome,
                ConsumeAccountRateLimitResetCreditOutcome::AlreadyRedeemed
            );
        } else {
            let error = read_error_response(&mut app, retry).await?;
            assert_eq!(error.error.code, INVALID_REQUEST_ERROR_CODE);
            assert_eq!(
                error.error.message,
                "Previous reset attempt resolved. No new reset was used; refresh usage before trying again."
            );
        }
        let requests = server.received_requests().await.unwrap();
        let resets: Vec<_> = requests
            .iter()
            .filter(|request| {
                request.method == "POST"
                    && request.url.path() == "/api/codex/rate-limit-reset-credits/consume"
            })
            .collect();
        assert_eq!(
            resets[4].body_json::<serde_json::Value>()?,
            json!({
                "redeem_request_id": "unknown", "credit_id": "original",
            })
        );
        let recovered = store.acquire_reset_mutation_lease(&id)?.state()?;
        let completion = recovered.completion.as_ref().unwrap();
        assert_eq!(
            recovered,
            codex_login::ResetState {
                phase: None,
                completion: Some(codex_login::ResetCompletion {
                    id: "unknown".into(),
                    completed_at: completion.completed_at,
                    manual: true,
                    source: Some(source),
                    reconciliation: codex_login::ResetReconciliation::Pending,
                }),
            }
        );
        consume_reset_credit(&mut app, "unknown").await?;
        assert_eq!(store.acquire_reset_mutation_lease(&id)?.state()?, recovered);
    }
    Ok(())
}

#[tokio::test]
async fn reset_admission_reads_imported_a_without_switching_selected_b() -> Result<()> {
    use codex_app_server_protocol::GetAccountRateLimitsResponse;
    use codex_app_server_protocol::UsageResetTargetParams;
    use codex_login::ResetCredentialSource;
    use codex_login::ResetReconciliation;
    use codex_protocol::inference_attribution::InferenceNativeSource;
    const TOKEN: &str = "e30.eyJleHAiOjQxMDI0NDQ4MDB9.c2ln";
    let home = TempDir::new()?;
    let backend = MockServer::start().await;
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new(TOKEN).account_id("account-a"),
        AuthCredentialsStoreMode::File,
    )?;
    let store = AccountStore::new(home.path().into());
    let profile = store.import_current(
        /*label*/ None,
        AuthCredentialsStoreMode::File,
        AuthKeyringBackendKind::default(),
    )?;
    let mut lease = store.acquire_reset_mutation_lease(&profile.id)?;
    lease.begin_manual(
        "completed-a",
        ResetCredentialSource::Imported,
        /*credit_id*/ None,
    )?;
    lease.confirm_manual("completed-a", /*completed_at*/ 10_000_000_000)?;
    let completion = lease.state()?.completion.unwrap();
    lease.reconcile_proxy(&completion, ResetReconciliation::ObservedClear)?;
    drop(lease);
    write_chatgpt_auth(
        home.path(),
        ChatGptAuthFixture::new(TOKEN).account_id("account-b"),
        AuthCredentialsStoreMode::File,
    )?;
    let selected = std::fs::read(home.path().join("auth.json"))?;
    std::fs::write(
        home.path().join("config.toml"),
        format!(
            "automatic_account_selection = \"disabled\"\nchatgpt_base_url = {:?}\n",
            backend.uri()
        ),
    )?;
    for account in ["account-a", "account-b"] {
        Mock::given(method("GET")).and(path("/api/codex/usage"))
            .and(header("chatgpt-account-id", account))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "account_id": account, "plan_type": "pro", "rate_limit": {"allowed": true, "limit_reached": false,
                    "secondary_window": {"used_percent": 1, "limit_window_seconds": 604800, "reset_after_seconds": 3600, "reset_at": 2000000000}}
            }))).expect(1).mount(&backend).await;
    }
    let mut app = initialized_app_server(home.path()).await?;
    let target = UsageResetTargetParams {
        thread_id: "waiting-thread".into(),
        turn_id: "waiting-turn".into(),
        source: InferenceNativeSource::Imported,
        account_id: profile.id.to_string(),
        failed_at: 9,
        completion_id: Some("completed-a".into()),
    };
    for mismatch in ["kind", "completion", "old", "matching"] {
        let mut request = target.clone();
        match mismatch {
            "kind" => request.source = InferenceNativeSource::Root,
            "completion" => request.completion_id = Some("other-reset".into()),
            "old" => request.failed_at = 11,
            _ => {}
        }
        let id = app
            .send_request(
                "account/rateLimits/read",
                Some(json!({"resetAdmission": request})),
            )
            .await?;
        if mismatch != "matching" {
            assert_eq!(
                read_error_response(&mut app, id).await?.error.code,
                INVALID_REQUEST_ERROR_CODE
            );
        } else {
            let response: GetAccountRateLimitsResponse =
                timeout(DEFAULT_READ_TIMEOUT, app.read_response(id)).await??;
            assert_eq!(
                (response.account_id.as_deref(), response.reset_admission),
                (Some("account-a"), None)
            );
        }
    }
    let id = app
        .send_request(
            "account/rateLimits/read",
            Some(json!({"excludeResetCreditDetails": true})),
        )
        .await?;
    let response: GetAccountRateLimitsResponse =
        timeout(DEFAULT_READ_TIMEOUT, app.read_response(id)).await??;
    assert_eq!(response.account_id.as_deref(), Some("account-b"));
    assert_eq!(std::fs::read(home.path().join("auth.json"))?, selected);
    assert!(!home.path().join("cli-proxy").exists());
    Ok(())
}
