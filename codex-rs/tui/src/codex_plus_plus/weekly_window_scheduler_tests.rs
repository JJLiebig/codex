use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use codex_http_client::OutboundProxyPolicy;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::OPENAI_PROVIDER_ID;

use super::*;
use crate::codex_plus_plus::native_account_maintenance;

#[tokio::test]
async fn custom_conversation_provider_keeps_native_maintenance_eligible() {
    let home = tempfile::tempdir().unwrap();
    let mut config = crate::legacy_core::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await
        .unwrap();
    config.model_provider_id = "custom".to_string();
    config.model_provider =
        ModelProviderInfo::create_openai_provider(Some("http://localhost:1234/v1".to_string()));
    let account_home = home.path().join("accounts/acct_test");
    let request = native_account_maintenance::ping_request(&config, account_home.clone());
    assert_eq!(request.auth_config.codex_home, account_home);
    assert_eq!(request.model_provider_id, OPENAI_PROVIDER_ID);
    assert_eq!(preflight_native_account_maintenance(&config), Ok(()));

    config.respect_system_proxy = true;
    let routed = native_account_maintenance::ping_request(&config, home.path().into());
    assert_eq!(
        routed.http_client_factory.outbound_proxy_policy(),
        OutboundProxyPolicy::RespectSystemProxy
    );
    assert_eq!(
        preflight_native_account_maintenance(&config),
        Err(WeeklyWindowPingOutcome::UnsupportedRouting)
    );

    config.respect_system_proxy = false;
    config.chatgpt_base_url = "https://other.example/backend-api".to_string();
    assert_eq!(
        preflight_native_account_maintenance(&config),
        Err(WeeklyWindowPingOutcome::UnsupportedConfiguration)
    );
}

#[tokio::test(start_paused = true)]
async fn schedule_scans_on_time_and_observes_disable_until_dropped() {
    let scans = Arc::new(AtomicUsize::new(0));
    let task_scans = Arc::clone(&scans);
    let drained = Arc::new(AtomicBool::new(false));
    let task_drained = Arc::clone(&drained);
    let release = Arc::new(tokio::sync::Notify::new());
    let task_release = Arc::clone(&release);
    let (state, receiver) = watch::channel(SchedulerSettings {
        weekly: true,
        auto_redeem: None,
    });
    let scheduler = WeeklyWindowScheduler {
        state,
        statuses: Arc::new(Mutex::new(HashMap::new())),
        _task: tokio::spawn(run_schedule(
            move |_control| {
                let scan = task_scans.fetch_add(1, Ordering::Relaxed);
                let release = Arc::clone(&task_release);
                let drained = Arc::clone(&task_drained);
                async move {
                    if scan == 2 {
                        release.notified().await;
                        drained.store(true, Ordering::Relaxed);
                    }
                }
            },
            receiver,
        )),
    };

    tokio::task::yield_now().await;
    assert_eq!(scans.load(Ordering::Relaxed), 1);
    tokio::time::advance(SCAN_INTERVAL).await;
    tokio::task::yield_now().await;
    assert_eq!(scans.load(Ordering::Relaxed), 2);

    let active_scan = scheduler.state.subscribe();
    scheduler.set_settings(/*weekly*/ false, /*auto_redeem*/ None);
    scheduler.set_settings(/*weekly*/ false, Some(AutoRedeemResets::default()));
    assert!(active_scan.has_changed().unwrap());
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(scans.load(Ordering::Relaxed), 3);

    drop(scheduler);
    release.notify_one();
    tokio::task::yield_now().await;
    assert!(drained.load(Ordering::Relaxed));
    tokio::time::advance(SCAN_INTERVAL).await;
    tokio::task::yield_now().await;
    assert_eq!(scans.load(Ordering::Relaxed), 3);
}

#[test]
fn scheduler_automation_is_enabled_by_either_setting() {
    assert!(!SchedulerSettings::default().enabled());
    assert!(
        SchedulerSettings {
            weekly: true,
            auto_redeem: None,
        }
        .enabled()
    );
    assert!(
        SchedulerSettings {
            weekly: false,
            auto_redeem: Some(AutoRedeemResets::default()),
        }
        .enabled()
    );
}

#[test]
fn scheduler_status_is_bounded() {
    let mut statuses = HashMap::new();
    for index in 0..=MAX_STATUS_ACCOUNTS {
        record_status(
            &mut statuses,
            &serde_json::from_str(&format!("\"acct_{index}\"")).expect("account id"),
            WeeklyWindowStatus::Waiting(Some(100)),
        );
    }
    assert_eq!(statuses.len(), MAX_STATUS_ACCOUNTS);
}

#[test]
fn ping_outcomes_preserve_safe_failure_evidence() {
    use WeeklyWindowPingOutcome::*;
    use WeeklyWindowUsage::*;
    let assert_completed = |outcome, usage, refreshed_usage| {
        assert_eq!(
            attempt_outcome(outcome, usage),
            WeeklyWindowAttemptOutcome::Completed { refreshed_usage }
        )
    };
    let usage = |unused| Present {
        unused,
        resets_at: Some(42),
    };
    assert_completed(Completed, usage(true), usage(true));
    assert_completed(Completed, Missing, Missing);
    assert_eq!(
        attempt_outcome(Rejected { status: Some(422) }, Missing),
        WeeklyWindowAttemptOutcome::Retryable {
            error: WeeklyWindowRetryableError::Rejected { status: Some(422) }
        }
    );
    assert_eq!(
        attempt_outcome(Ambiguous { status: Some(503) }, Missing),
        WeeklyWindowAttemptOutcome::Ambiguous { status: Some(503) }
    );
    assert_eq!(
        attempt_outcome(UnsupportedRouting, Missing),
        WeeklyWindowAttemptOutcome::Unsupported {
            error: WeeklyWindowError::UnsupportedRouting
        }
    );
}

#[tokio::test]
async fn scans_reconcile_old_manual_and_same_pass_automatic_completions_without_starting_proxy() {
    use base64::Engine;
    use codex_login::ResetCredentialSource;
    use codex_login::ResetReconciliation;
    use sha2::Digest;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;
    for source in [ResetCredentialSource::Root, ResetCredentialSource::Imported] {
        let imported = source == ResetCredentialSource::Imported;
        let home = tempfile::tempdir().unwrap();
        let server = MockServer::start().await;
        let mut config = crate::legacy_core::config::ConfigBuilder::default()
            .codex_home(home.path().into())
            .build()
            .await
            .unwrap();
        config.chatgpt_base_url = server.uri();
        config.cli_auth_credentials_store_mode = codex_login::AuthCredentialsStoreMode::File;
        config.model_provider = ModelProviderInfo::create_cli_proxy_provider();
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            serde_json::json!({"exp": 4102444800_u64, "https://api.openai.com/auth": {"chatgpt_account_id": "workspace-a"}}).to_string()
        );
        let token = format!("e30.{claims}.c2ln");
        std::fs::write(home.path().join("auth.json"), serde_json::json!({
            "tokens": {"id_token": token, "access_token": token, "refresh_token": "synthetic-refresh", "account_id": "workspace-a"},
            "last_refresh": Utc::now()
        }).to_string()).unwrap();
        Mock::given(method("GET")).and(path("/api/codex/usage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "account_id": "workspace-a", "plan_type": "pro", "rate_limit": {
                    "allowed": true, "limit_reached": false, "primary_window": null,
                    "secondary_window": {"used_percent": 1, "limit_window_seconds": 604800, "reset_after_seconds": 3600, "reset_at": 2000000000}
                }
            }))).expect(if imported { 2 } else { 1 }).mount(&server).await;
        Mock::given(method("POST"))
            .and(path("/api/codex/rate-limit-reset-credits/consume"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"code": "reset", "windows_reset": 2})),
            )
            .expect(u64::from(imported))
            .mount(&server)
            .await;
        let digest = sha2::Sha256::digest(b"account:workspace-a");
        let id: AccountId =
            serde_json::from_value(serde_json::json!(format!("acct_{digest:.16x}"))).unwrap();
        let store = AccountStore::new(home.path().into());
        if imported {
            store
                .import_current(
                    /*label*/ None,
                    codex_login::AuthCredentialsStoreMode::File,
                    codex_login::AuthKeyringBackendKind::default(),
                )
                .unwrap();
            codex_login::auth::login_with_chatgpt_auth_tokens(
                &home.path().join("accounts").join(id.as_str()),
                &token,
                "workspace-a",
                /*chatgpt_plan_type*/ None,
            )
            .unwrap();
        }
        let mut lease = store.acquire_reset_mutation_lease(&id).unwrap();
        if imported {
            lease.load_or_begin("credit").unwrap();
        } else {
            lease
                .begin_manual("confirmed", source, /*credit_id*/ None)
                .unwrap();
            lease
                .confirm_manual("confirmed", /*completed_at*/ 1)
                .unwrap();
        }
        drop(lease);
        let (_settings, control) = watch::channel(SchedulerSettings {
            weekly: false,
            auto_redeem: imported.then_some(AutoRedeemResets::default()),
        });
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        scan(
            config,
            control,
            Arc::default(),
            Arc::new(Mutex::new(auto_redeem_resets::CompletionNotices::new())),
            AppEventSender::new(tx),
        )
        .await;
        let state = store
            .acquire_reset_mutation_lease(&id)
            .unwrap()
            .state()
            .unwrap();
        assert_eq!(state.phase, None);
        assert_eq!(
            state.completion.unwrap().reconciliation,
            ResetReconciliation::Pending
        );
        assert!(!home.path().join("cli-proxy").exists());
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(
                event,
                crate::app_event::AppEvent::UsageResetCompleted { .. }
            ));
        }
    }
}
