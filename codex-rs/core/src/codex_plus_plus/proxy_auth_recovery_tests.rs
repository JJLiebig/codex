use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn owned_auth_recovery_is_guarded_and_bounded() -> anyhow::Result<()> {
    let rotated = format!(
        "e30.{}.sig",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(json!({"exp":chrono::Utc::now().timestamp()+7200,"rotated":true}).to_string())
    );
    let endpoint = match std::env::var("CODEX_TEST_NATIVE_RECOVERY") {
        Ok(endpoint) => endpoint,
        Err(_) => {
            // Scope the existing OAuth URL override to a child process, never this test process.
            let oauth = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/oauth/token"))
                .respond_with(
                    ResponseTemplate::new(200).set_body_json(json!({"access_token":rotated})),
                )
                .expect(3)
                .mount(&oauth)
                .await;
            let endpoint = format!("{}/oauth/token", oauth.uri());
            let result = tokio::task::spawn_blocking(move || {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "client::tests::proxy_request_tests::auth_recovery::owned_auth_recovery_is_guarded_and_bounded", "--nocapture"])
                    .env("CODEX_TEST_NATIVE_RECOVERY", &endpoint)
                    .env(codex_login::auth::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR, endpoint)
                    .output()
            }).await??;
            assert!(
                result.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            return Ok(());
        }
    };
    for case in [
        "refresh",
        "quota_then_refresh",
        "twice",
        "local",
        "claude",
        "mismatch",
        "malformed",
        "suspended",
        "stale",
    ] {
        let fixture = OwnedFixture::with_refresh_endpoint(Some(&endpoint)).await?;
        let pair = if case == "quota_then_refresh" {
            Some(fixture.import_pair().await?)
        } else {
            None
        };
        let before = *fixture.manager.auth_change_receiver().borrow();
        let files = fixture.files.clone();
        let original = fixture
            .manager
            .auth_cached()
            .unwrap()
            .get_token_data()?
            .access_token;
        let published_original = original.clone();
        let auth_home = fixture.home.path().to_path_buf();
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let _mock = Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(move |_: &wiremock::Request| {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                if case == "quota_then_refresh" && attempt == 0 {
                    return ResponseTemplate::new(429)
                        .insert_header("x-cpa-trace-id", NATIVE_TRACE)
                        .set_body_json(json!({"error":{"type":"usage_limit_reached", "resets_at":chrono::Utc::now().timestamp()+3600}}));
                }
                if (attempt == 1 && case == "refresh") || (attempt == 2 && case == "quota_then_refresh") {
                    assert!(files.lock().unwrap().values().any(|record| {
                        record["access_token"]
                            .as_str()
                            .is_some_and(|token| !token.is_empty() && token != published_original)
                    }));
                    return ResponseTemplate::new(200).set_body_string(COMPLETED);
                }
                if case == "stale" {
                    let mut auth: codex_login::auth::AuthDotJson = serde_json::from_slice(
                        &std::fs::read(auth_home.join("auth.json")).unwrap(),
                    )
                    .unwrap();
                    auth.tokens.as_mut().unwrap().refresh_token = "changed-on-disk".into();
                    codex_login::save_auth(
                        &auth_home,
                        &auth,
                        AuthCredentialsStoreMode::File,
                        AuthKeyringBackendKind::default(),
                    )
                    .unwrap();
                }
                let code = match case {
                    "malformed" => "other",
                    "suspended" => "upstream_authentication_required",
                    _ => "auth_unavailable",
                };
                let response = ResponseTemplate::new(if case == "suspended" { 503 } else { 401 })
                    .set_body_json(json!({"error":{"type":"authentication_error", "code":code}}));
                if matches!(case, "local" | "suspended") {
                    response
                } else {
                    let trace = if case == "quota_then_refresh" {
                        "20260930010000-0000000000000003-aabbccdd"
                    } else if matches!(case, "claude" | "mismatch") {
                        CLAUDE_TRACE
                    } else {
                        NATIVE_TRACE
                    };
                    response.insert_header("x-cpa-trace-id", trace)
                }
            })
            .expect(if case == "quota_then_refresh" {
                3
            } else if matches!(case, "refresh" | "twice") {
                2
            } else {
                1
            })
            .mount_as_scoped(&fixture.server)
            .await;
        let mut client = test_model_client(SessionSource::Cli);
        Arc::get_mut(&mut client.state).unwrap().provider = create_model_provider(
            ModelProviderInfo::create_cli_proxy_provider(),
            Some(fixture.manager.clone()),
        );
        let metadata = test_responses_metadata_for_client(
            &client,
            /*turn_id*/ None,
            "stable-auth-session".into(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        );
        let mut model = test_model_info();
        model.slug = if case == "claude" {
            "claude-new"
        } else {
            "future-9.7"
        }
        .into();
        let mut session = client.new_session();
        let result = session
            .stream_responses_api(
                &Prompt::default(),
                &model,
                &test_session_telemetry(),
                /*effort*/ None,
                ReasoningSummaryConfig::None,
                /*service_tier*/ None,
                &metadata,
                &InferenceTraceContext::disabled(),
            )
            .await;
        let error = match result {
            Ok(mut stream) => {
                let mut error = None;
                while let Some(event) = stream.next().await {
                    if let Err(failure) = event {
                        error = Some(failure);
                        break;
                    }
                }
                error
            }
            Err(error) => Some(error),
        };
        assert_eq!(
            error.is_none(),
            matches!(case, "refresh" | "quota_then_refresh"),
            "{case}"
        );
        if let Some(error) = error {
            assert!(session.owned_retry_forbidden(&error), "{case}");
        }
        let refreshed = matches!(case, "refresh" | "quota_then_refresh" | "twice");
        assert_eq!(
            *fixture.manager.auth_change_receiver().borrow() > before,
            refreshed,
            "{case}"
        );
        if refreshed {
            let tokens = fixture.manager.auth_cached().unwrap().get_token_data()?;
            assert_ne!(tokens.access_token, original);
            assert!(
                fixture
                    .files
                    .lock()
                    .unwrap()
                    .values()
                    .any(|record| record["access_token"] == tokens.access_token)
            );
            if let Some(ids) = pair {
                assert_eq!(fixture.manager.active_account_id(), Some(ids[1].clone()));
                let profiles = AccountStore::new(fixture.home.path().to_path_buf()).list()?;
                assert_eq!(
                    profiles
                        .iter()
                        .map(|profile| profile.usage_limit_resets_at.is_some())
                        .collect::<Vec<_>>(),
                    vec![true, false]
                );
            }
            let requests = fixture
                .server
                .received_requests()
                .await
                .unwrap()
                .into_iter()
                .filter(|request| request.url.path() == "/v1/responses")
                .collect::<Vec<_>>();
            let requests = &requests[requests.len() - 2..];
            assert_eq!(
                (
                    serde_json::from_slice::<serde_json::Value>(&requests[0].body)?,
                    requests[0].headers.get("session-id").unwrap()
                ),
                (
                    serde_json::from_slice::<serde_json::Value>(&requests[1].body)?,
                    requests[1].headers.get("session-id").unwrap()
                )
            );
        }
    }
    Ok(())
}
