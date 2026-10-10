use super::*;
use crate::legacy_core::config::ConfigBuilder;
use codex_protocol::auth::RefreshTokenFailedError;
use codex_protocol::protocol::RateLimitWindow;
use pretty_assertions::assert_eq;
use wiremock::MockServer;

const TEST_ID_TOKEN: &str = "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln";

#[tokio::test]
async fn fetch_enforces_forced_workspace_before_network() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("auth.json"), format!(r#"{{"OPENAI_API_KEY":null,"tokens":{{"id_token":"{TEST_ID_TOKEN}","access_token":"token","refresh_token":"refresh","account_id":"account-123"}},"last_refresh":"2099-01-01T00:00:00Z"}}"#)).unwrap();
    let server = MockServer::start().await;
    let mut config = ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await
        .unwrap();
    config.chatgpt_base_url = server.uri();
    config.forced_chatgpt_workspace_id = Some(vec!["other".to_string()]);
    assert!(fetch(home.path().into(), config).await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn refreshed_usage_reconciles_persisted_account_cooldown() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("auth.json"), format!(r#"{{"OPENAI_API_KEY":null,"tokens":{{"id_token":"{TEST_ID_TOKEN}","access_token":"token","refresh_token":"refresh","account_id":"account-123"}},"last_refresh":"2099-01-01T00:00:00Z"}}"#)).unwrap();
    let store = AccountStore::new(home.path().to_path_buf());
    let profile = store
        .import_current(
            /*label*/ None,
            AuthCredentialsStoreMode::File,
            codex_login::AuthKeyringBackendKind::default(),
        )
        .unwrap();
    let server = MockServer::start().await;
    let mut config = ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await
        .unwrap();
    config.chatgpt_base_url = server.uri();
    let future = chrono::Utc::now().timestamp() + 86_400;
    let window = |used_percent, seconds| {
        serde_json::json!({
            "used_percent": used_percent,
            "limit_window_seconds": seconds,
            "reset_after_seconds": 86_400,
            "reset_at": future,
        })
    };
    for (rate_limit, blocked) in [
        (
            serde_json::json!({"allowed": true, "limit_reached": false, "primary_window": window(/*used_percent*/ 0, /*seconds*/ 18_000), "secondary_window": window(/*used_percent*/ 0, /*seconds*/ 604_800)}),
            false,
        ),
        (
            serde_json::json!({"allowed": true, "limit_reached": false, "primary_window": window(/*used_percent*/ 0, /*seconds*/ 604_800)}),
            false,
        ),
        (
            serde_json::json!({"allowed": false, "limit_reached": true, "primary_window": window(/*used_percent*/ 100, /*seconds*/ 18_000), "secondary_window": window(/*used_percent*/ 0, /*seconds*/ 604_800)}),
            true,
        ),
        (
            serde_json::json!({"allowed": false, "limit_reached": true, "primary_window": window(/*used_percent*/ 0, /*seconds*/ 18_000), "secondary_window": window(/*used_percent*/ 100, /*seconds*/ 604_800)}),
            true,
        ),
        (serde_json::Value::Null, true),
    ] {
        store
            .record_usage_limit_resets_at(&profile.id, future)
            .unwrap();
        server.reset().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .respond_with(wiremock::ResponseTemplate::new(/*s*/ 200).set_body_json(
                serde_json::json!({
                    "plan_type": "pro",
                    "rate_limit": rate_limit,
                }),
            ))
            .mount(&server)
            .await;
        let loaded = load(
            &config,
            &[(
                profile.id.clone(),
                home.path().join("accounts").join(profile.id.as_str()),
            )],
            &store,
        )
        .await;
        assert_eq!(loaded.usage.len(), 1);
        let candidates = AccountStore::new(home.path().to_path_buf())
            .candidates()
            .unwrap();
        assert_eq!(candidates[0].blocked, blocked, "usage: {rate_limit}");
    }
}

#[test]
fn maps_five_hour_and_weekly_windows_to_picker_usage() {
    let response = RateLimitsWithResetCredits {
        rate_limits: vec![RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            normal_model_slug: None,
            primary: Some(RateLimitWindow {
                used_percent: 67.6,
                window_minutes: Some(300),
                resets_at: Some(1_749_950_000),
            }),
            secondary: Some(RateLimitWindow {
                used_percent: 0.4,
                window_minutes: Some(10_080),
                resets_at: Some(1_750_000_000),
            }),
            credits: None,
            individual_limit: None,
            spend_control_reached: None,
            plan_type: Some(codex_protocol::account::PlanType::Pro),
            rate_limit_reached_type: None,
        }],
        rate_limit_reset_credits: Some(codex_backend_client::RateLimitResetCreditsSummary {
            available_count: 2,
        }),
        account_id: None,
        user_id: None,
        rate_limit_upsell: None,
        ordinary_usage_allowed: None,
    };

    assert_eq!(
        account_usage(&response),
        AccountUsage {
            plan_type: Some(codex_protocol::account::PlanType::Pro),
            available_resets: Some(2),
            primary_window_minutes: Some(300),
            five_hour_reset_at: Some(1_749_950_000),
            five_hour_remaining_percent: Some(32),
            five_hour_exhausted: false,
            weekly_reset_at: Some(1_750_000_000),
            weekly_unused: Some(false),
            weekly_remaining_percent: Some(100),
            weekly_exhausted: false,
        }
    );
}

#[test]
fn maps_weekly_only_primary_window_to_weekly_picker_usage() {
    let snapshot = RateLimitSnapshot {
        limit_id: Some("codex".to_string()),
        limit_name: None,
        normal_model_slug: None,
        primary: Some(RateLimitWindow {
            used_percent: 0.0,
            window_minutes: Some(10_080),
            resets_at: Some(1_750_000_000),
        }),
        secondary: None,
        credits: None,
        individual_limit: None,
        spend_control_reached: None,
        plan_type: None,
        rate_limit_reached_type: None,
    };

    assert_eq!(
        account_usage_from_snapshot(&snapshot),
        AccountUsage {
            plan_type: None,
            available_resets: None,
            primary_window_minutes: None,
            five_hour_reset_at: None,
            five_hour_remaining_percent: None,
            five_hour_exhausted: false,
            weekly_reset_at: Some(1_750_000_000),
            weekly_unused: Some(true),
            weekly_remaining_percent: Some(100),
            weekly_exhausted: false,
        }
    );
}

#[test]
fn exhausted_until_uses_the_later_exhausted_window_reset() {
    let usage = AccountUsage {
        plan_type: None,
        available_resets: None,
        primary_window_minutes: Some(300),
        five_hour_reset_at: Some(1_749_950_000),
        five_hour_remaining_percent: Some(0),
        five_hour_exhausted: true,
        weekly_reset_at: Some(1_750_000_000),
        weekly_unused: Some(false),
        weekly_remaining_percent: Some(0),
        weekly_exhausted: true,
    };

    assert_eq!(usage.exhausted_until(), Some(1_750_000_000));
}

#[test]
fn rounded_zero_remaining_does_not_mark_window_exhausted() {
    let response = RateLimitsWithResetCredits {
        rate_limits: vec![RateLimitSnapshot {
            limit_id: Some("codex".to_string()),
            limit_name: None,
            normal_model_slug: None,
            primary: Some(RateLimitWindow {
                used_percent: 99.6,
                window_minutes: Some(300),
                resets_at: Some(1_749_950_000),
            }),
            secondary: None,
            credits: None,
            individual_limit: None,
            spend_control_reached: None,
            plan_type: None,
            rate_limit_reached_type: None,
        }],
        rate_limit_reset_credits: None,
        account_id: None,
        user_id: None,
        rate_limit_upsell: None,
        ordinary_usage_allowed: None,
    };

    assert_eq!(
        account_usage(&response),
        AccountUsage {
            plan_type: None,
            available_resets: None,
            primary_window_minutes: Some(300),
            five_hour_reset_at: Some(1_749_950_000),
            five_hour_remaining_percent: Some(0),
            five_hour_exhausted: false,
            weekly_reset_at: None,
            weekly_unused: None,
            weekly_remaining_percent: None,
            weekly_exhausted: false,
        }
    );
}

#[test]
fn reused_refresh_token_marks_imported_account_as_login_required() {
    let error = anyhow!(RefreshTokenFailedError::new(
        RefreshTokenFailedReason::Exhausted,
        "refresh token already used",
    ));

    assert!(login_required(&error));
}
