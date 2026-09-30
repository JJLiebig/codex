use std::num::NonZeroU64;

use codex_protocol::protocol::RateLimitWindow;
use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::body_partial_json;
use wiremock::matchers::method;
use wiremock::matchers::path;

use super::*;
use crate::history_cell::HistoryCell;

const NOW: i64 = 1_800_000_000;
const TEST_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln";

fn settings() -> AutoRedeemResets {
    AutoRedeemResets {
        before_expiry_minutes: NonZeroU64::new(60),
        weekly_exhausted_min_wait_hours: NonZeroU64::new(72),
    }
}

#[test]
fn project_config_cannot_enable_redemption() {
    let project = codex_config::ConfigLayerEntry::new(
        codex_config::ConfigLayerSource::Project {
            dot_codex_folder: std::env::current_dir().unwrap().try_into().unwrap(),
        },
        toml::from_str(
            "[auto_redeem_resets]\nbefore_expiry_minutes=1\nweekly_exhausted_min_wait_hours=1",
        )
        .unwrap(),
    );
    let stack =
        ConfigLayerStack::new(vec![project], Default::default(), Default::default()).unwrap();
    assert_eq!(super::settings(&stack), None);
}

fn credit(id: &str, expires_at: Option<i64>) -> RateLimitResetCreditDetails {
    RateLimitResetCreditDetails {
        id: id.to_string(),
        reset_type: "codex_rate_limits".to_string(),
        status: "available".to_string(),
        granted_at: "2026-01-01T00:00:00Z".to_string(),
        expires_at: expires_at
            .map(|value| DateTime::from_timestamp(value, 0).unwrap().to_rfc3339()),
        title: None,
        description: None,
    }
}

fn usage(used_percent: f64, resets_at: Option<i64>, reached: bool) -> RateLimitsWithResetCredits {
    RateLimitsWithResetCredits {
        rate_limits: vec![RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            normal_model_slug: None,
            primary: Some(RateLimitWindow {
                used_percent: 100.0,
                window_minutes: Some(300),
                resets_at: Some(NOW + 300),
            }),
            secondary: Some(RateLimitWindow {
                used_percent,
                window_minutes: Some(MINUTES_PER_WEEK),
                resets_at,
            }),
            credits: None,
            individual_limit: None,
            spend_control_reached: None,
            plan_type: None,
            rate_limit_reached_type: reached.then_some(RateLimitReachedType::RateLimitReached),
        }],
        rate_limit_reset_credits: None,
        ordinary_usage_allowed: None,
        account_id: None,
        user_id: None,
        rate_limit_upsell: None,
    }
}

#[test]
fn strict_selection_is_deterministic_and_fails_closed() {
    let mut duplicate = credit("duplicate", Some(NOW + 60));
    duplicate.status = "available".to_string();
    let cases = [
        (
            "expiry ignores weekly ambiguity",
            vec![
                credit("later", Some(NOW + 120)),
                credit("earlier", Some(NOW + 60)),
            ],
            usage(50.0, None, false),
            Some("earlier"),
        ),
        (
            "far expiry is not eligible",
            vec![credit("far", Some(NOW + 3_601))],
            usage(50.0, Some(NOW + 400_000), false),
            None,
        ),
        (
            "weekly exhaustion selects a non-expiring credit",
            vec![credit("stable", None)],
            usage(100.0, Some(NOW + 73 * 60 * 60), true),
            Some("stable"),
        ),
        (
            "wrong exhaustion type fails closed",
            vec![credit("stable", None)],
            usage(100.0, Some(NOW + 73 * 60 * 60), false),
            None,
        ),
        (
            "duplicate exact ids are rejected",
            vec![duplicate.clone(), duplicate],
            usage(50.0, None, false),
            None,
        ),
        (
            "expired credits are rejected",
            vec![credit("expired", Some(NOW))],
            usage(100.0, Some(NOW + 73 * 60 * 60), true),
            None,
        ),
    ];
    for (name, credits, usage, expected) in cases {
        assert_eq!(
            select_credit(
                &RateLimitResetCreditsDetails {
                    available_count: credits.len() as i64,
                    credits,
                },
                &usage,
                settings(),
                NOW,
            )
            .as_deref(),
            expected,
            "{name}"
        );
    }
    assert!(!FreshRedemption::RecoveryOnly.allowed());
}

#[test]
fn redemption_triggers_can_be_enabled_independently() {
    let credits = RateLimitResetCreditsDetails {
        available_count: 1,
        credits: vec![credit("reset", Some(NOW + 60))],
    };
    let expiry_only = AutoRedeemResets {
        weekly_exhausted_min_wait_hours: None,
        ..settings()
    };
    let exhaustion_only = AutoRedeemResets {
        before_expiry_minutes: None,
        ..settings()
    };
    for (settings, used_percent, expected) in [
        (expiry_only, 50.0, Some("reset")),
        (exhaustion_only, 50.0, None),
        (exhaustion_only, 100.0, Some("reset")),
        (
            AutoRedeemResets {
                before_expiry_minutes: None,
                weekly_exhausted_min_wait_hours: None,
            },
            100.0,
            None,
        ),
    ] {
        assert_eq!(
            select_credit(
                &credits,
                &usage(
                    used_percent,
                    Some(NOW + 73 * 60 * 60),
                    /*reached*/ true
                ),
                settings,
                NOW
            )
            .as_deref(),
            expected
        );
    }
    let non_expiring = RateLimitResetCreditsDetails {
        available_count: 1,
        credits: vec![credit("reset", None)],
    };
    assert_eq!(
        select_credit(
            &non_expiring,
            &usage(
                /*used_percent*/ 100.0,
                Some(NOW + 73 * 60 * 60),
                /*reached*/ true
            ),
            expiry_only,
            NOW
        ),
        None
    );
}

#[test]
fn profile_match_requires_the_exact_workspace_identity() {
    let auth = CodexAuth::from_external_chatgpt_tokens(
        "e30.e30.c2ln",
        "workspace-a",
        /*chatgpt_plan_type*/ None,
    )
    .unwrap();
    let digest = Sha256::digest(b"account:workspace-a");
    let expected: AccountId = serde_json::from_str(&format!("\"acct_{digest:.16x}\"")).unwrap();
    let other: AccountId = serde_json::from_str("\"acct_other\"").unwrap();
    assert!(matches_profile(&expected, &auth));
    assert!(!matches_profile(&other, &auth));

    let mut tokens = auth.get_token_data().unwrap();
    tokens.account_id = None;
    tokens.id_token.chatgpt_account_id = None;
    tokens.id_token.chatgpt_user_id = Some("user-a".to_string());
    let digest = Sha256::digest(b"user:user-a");
    assert_eq!(profile_id(&tokens), Some(format!("acct_{digest:.16x}")));
}

#[test]
fn exact_weekly_accepts_an_unused_window_without_a_reset_time() {
    let usage = usage(0.0, None, false);
    assert_eq!(exact_weekly(&usage).unwrap().1.used_percent, 0.0);
}

#[test]
fn automatic_selection_flag_does_not_gate_reset_automation() {
    let home = tempfile::tempdir().unwrap();
    let id: AccountId = serde_json::from_str("\"acct_test\"").unwrap();
    let account_home = home.path().join("accounts").join(id.as_str());
    std::fs::create_dir_all(&account_home).unwrap();
    std::fs::write(account_home.join("auth.json"), "{}").unwrap();
    std::fs::write(
        home.path().join("accounts/index.json"),
        r#"{"accounts":[{"id":"acct_test","label":"test@example.com","enabled":true,"automation_enabled":false,"auth":{"scope":"file","path":"accounts/acct_test/auth.json"}}]}"#,
    )
    .unwrap();

    assert_eq!(
        current_account_home(&AccountStore::new(home.path().into()), &id).unwrap(),
        account_home
    );
}

#[tokio::test]
async fn redemption_flow_consumes_selected_credit_and_finishes_recovery() {
    let home = tempfile::tempdir().unwrap();
    let store = AccountStore::new(home.path().to_path_buf());
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/codex/usage"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            r#"{"plan_type":"pro","rate_limit":{"allowed":true,"limit_reached":false,"primary_window":null,"secondary_window":{"used_percent":1,"limit_window_seconds":604800,"reset_after_seconds":3600,"reset_at":2000000000}}}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    let expires_at = (Utc::now() + chrono::Duration::minutes(30)).to_rfc3339();
    Mock::given(method("GET"))
        .and(path("/api/codex/rate-limit-reset-credits"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            format!(r#"{{"credits":[{{"id":"credit-1","reset_type":"codex_rate_limits","status":"available","granted_at":"2026-01-01T00:00:00Z","expires_at":"{expires_at}"}}],"available_count":1}}"#),
            "application/json",
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/codex/rate-limit-reset-credits/consume"))
        .and(body_partial_json(
            serde_json::json!({"credit_id": "credit-1"}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(r#"{"code":"reset","windows_reset":2}"#, "application/json"),
        )
        .mount(&server)
        .await;
    let mut config = crate::legacy_core::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await
        .unwrap();
    config.chatgpt_base_url = server.uri();
    config.model_provider =
        codex_model_provider_info::ModelProviderInfo::create_cli_proxy_provider();
    let auth = CodexAuth::from_external_chatgpt_tokens(
        TEST_ID_TOKEN,
        "account-123",
        /*chatgpt_plan_type*/ None,
    )
    .unwrap();
    let id: AccountId = serde_json::from_str(&format!(
        "\"{}\"",
        profile_id(&auth.get_token_data().unwrap()).unwrap()
    ))
    .unwrap();
    let account = ResetAccount {
        config: &config,
        store: &store,
        id: &id,
        home: home.path().to_path_buf(),
        client: BackendClient::from_auth(
            &config.chatgpt_base_url,
            &auth,
            config.auth_route_config().http_client_factory().clone(),
        ),
    };

    let usage = account
        .client
        .get_rate_limits_with_reset_credits()
        .await
        .unwrap();
    let credits = account
        .client
        .list_rate_limit_reset_credits()
        .await
        .unwrap();
    let credit_id = select_credit(&credits, &usage, settings(), Utc::now().timestamp()).unwrap();
    let mut lease = store.acquire_reset_mutation_lease(&id).unwrap();
    let ResetAttemptPhase::Redeeming {
        credit_id,
        redeem_request_id,
    } = lease.load_or_begin(&credit_id).unwrap()
    else {
        panic!("expected redemption phase");
    };
    account
        .redeem(&mut lease, &credit_id, &redeem_request_id)
        .await
        .unwrap();

    let completed = lease.state().unwrap();
    assert_eq!(
        completed,
        codex_login::ResetState {
            phase: None,
            completion: Some(codex_login::ResetCompletion {
                id: redeem_request_id,
                completed_at: completed.completion.as_ref().unwrap().completed_at,
                manual: false,
                source: Some(ResetCredentialSource::Imported),
            }),
        }
    );
}

#[test]
fn completion_notice_snapshot() {
    let text = notice_cell("Primary")
        .display_lines(80)
        .into_iter()
        .flat_map(|line| line.spans)
        .map(|span| span.content.into_owned())
        .collect::<String>();
    insta::with_settings!({ snapshot_path => "snapshots" }, {
        insta::assert_snapshot!("auto_redeem_completion_notice", text);
    });
}

#[test]
fn completion_notice_precedes_ready_signals_until_recovery_finishes() {
    let home = tempfile::tempdir().unwrap();
    let store = AccountStore::new(home.path().into());
    let id: AccountId = serde_json::from_str("\"acct_test\"").unwrap();
    std::fs::create_dir_all(home.path().join("accounts")).unwrap();
    std::fs::write(home.path().join("accounts/index.json"),
        r#"{"accounts":[{"id":"acct_test","label":"Primary","auth":{"scope":"file","path":"accounts/acct_test/auth.json"}}]}"#).unwrap();
    let mut notices = CompletionNotices::new();
    let mut lease = store.acquire_reset_mutation_lease(&id).unwrap();
    let ResetAttemptPhase::Redeeming {
        redeem_request_id, ..
    } = lease.load_or_begin("credit").unwrap()
    else {
        unreachable!()
    };
    lease
        .confirm_redeemed(
            &redeem_request_id,
            Utc::now().timestamp_nanos_opt().unwrap(),
            /*source*/ None,
        )
        .unwrap();
    drop(lease);
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let tx = AppEventSender::new(tx);
    notices.poll(&store, &tx);
    assert!(matches!(
        events.try_recv(),
        Ok(AppEvent::InsertHistoryCell(_))
    ));
    assert!(events.try_recv().is_err());
    store
        .acquire_reset_mutation_lease(&id)
        .unwrap()
        .finish_weekly_activation()
        .unwrap();
    for _ in 0..2 {
        notices.poll(&store, &tx);
        assert!(
            matches!(events.try_recv(), Ok(AppEvent::UsageResetCompleted { account_id, .. }) if account_id == id)
        );
        assert!(
            events.try_recv().is_err(),
            "redemption notice must remain deduplicated"
        );
    }
}

#[tokio::test]
async fn pending_manual_attempt_skips_automatic_redemption_and_completion_notices() {
    let home = tempfile::tempdir().unwrap();
    let store = AccountStore::new(home.path().into());
    let id: AccountId = serde_json::from_str("\"acct_test\"").unwrap();
    let config = crate::legacy_core::config::ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await
        .unwrap();
    let mut lease = store.acquire_reset_mutation_lease(&id).unwrap();
    lease
        .begin_manual("manual", ResetCredentialSource::Imported)
        .unwrap();
    let pending = lease.state().unwrap();
    drop(lease);
    // No profile or auth is available: touching either would fail this scan.
    process_account(&config, &store, &id, settings(), &FreshRedemption::Allowed)
        .await
        .unwrap();
    let mut notices = CompletionNotices::new();
    let mut lease = store.acquire_reset_mutation_lease(&id).unwrap();
    assert_eq!(lease.state().unwrap(), pending);
    lease
        .confirm_manual("manual", Utc::now().timestamp_nanos_opt().unwrap())
        .unwrap();
    drop(lease);
    std::fs::write(home.path().join("accounts/index.json"),
        r#"{"accounts":[{"id":"acct_test","label":"Primary","auth":{"scope":"file","path":"accounts/acct_test/auth.json"}}]}"#).unwrap();
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    notices.poll(&store, &AppEventSender::new(tx));
    assert!(events.try_recv().is_err());
}
