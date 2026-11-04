use super::*;
use crate::legacy_core::config::ConfigBuilder;
use pretty_assertions::assert_eq;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;

#[tokio::test]
async fn reports_all_accounts_without_changing_account_state() {
    let home = tempfile::tempdir().unwrap();
    let profiles = ["ok", "failed", "missing", "weekly", "no-auth", "slow"].map(|id| {
        json!({
            "id": id, "label": format!("{id},\nlabel"), "enabled": false,
            "login_required": true, "priority": 0,
            "usage_limit_resets_at": 123,
            "auth": { "scope": "file", "path": format!("accounts/{id}/auth.json") }
        })
    });
    let index = serde_json::to_vec(&json!({"accounts": profiles})).unwrap();
    std::fs::create_dir(home.path().join("accounts")).unwrap();
    std::fs::write(home.path().join("accounts/index.json"), &index).unwrap();
    let mut credentials = Vec::new();
    for id in ["ok", "failed", "missing", "weekly", "slow"] {
        let path = home.path().join("accounts").join(id);
        std::fs::create_dir(&path).unwrap();
        let auth = format!(
            r#"{{"OPENAI_API_KEY":null,"tokens":{{"id_token":"{}","access_token":"secret-{id}","refresh_token":"refresh-{id}","account_id":"{id}"}},"last_refresh":"2099-01-01T00:00:00Z"}}"#,
            "eyJhbGciOiJub25lIiwidHlwIjoiSldUIn0.eyJlbWFpbCI6InVzZXJAZXhhbXBsZS5jb20iLCJlbWFpbF92ZXJpZmllZCI6dHJ1ZSwiaHR0cHM6Ly9hcGkub3BlbmFpLmNvbS9hdXRoIjp7ImNoYXRncHRfdXNlcl9pZCI6InVzZXItMTIzNDUiLCJ1c2VyX2lkIjoidXNlci0xMjM0NSIsImNoYXRncHRfcGxhbl90eXBlIjoicHJvIiwiY2hhdGdwdF9hY2NvdW50X2lkIjoiYWNjb3VudC0xMjMifX0.c2ln"
        );
        std::fs::write(path.join("auth.json"), &auth).unwrap();
        credentials.push((path.join("auth.json"), auth));
    }
    let server = MockServer::start().await;
    let window = |used, seconds| {
        json!({
            "used_percent": used, "limit_window_seconds": seconds,
            "reset_after_seconds": 100, "reset_at": 1_750_000_000
        })
    };
    for (id, response) in [
        (
            "slow",
            ResponseTemplate::new(/*s*/ 200)
                .set_body_json(json!({"plan_type": "pro"}))
                .set_delay(std::time::Duration::from_secs(/*secs*/ 6)),
        ),
        (
            "ok",
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "plan_type": "pro", "rate_limit": {
                    "allowed": true, "limit_reached": false,
                    "primary_window": window(/*used*/ 25, /*seconds*/ 14_400),
                    "secondary_window": window(/*used*/ 60, /*seconds*/ 604_800)
                }
            })),
        ),
        (
            "weekly",
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({
                "plan_type": "free", "rate_limit": {
                    "allowed": true, "limit_reached": false,
                    "primary_window": window(/*used*/ 10, /*seconds*/ 604_800)
                }
            })),
        ),
        (
            "missing",
            ResponseTemplate::new(/*s*/ 200).set_body_json(json!({"plan_type": "pro"})),
        ),
        (
            "failed",
            ResponseTemplate::new(/*s*/ 500).set_body_string("secret-backend-error"),
        ),
    ] {
        Mock::given(method("GET"))
            .and(header("authorization", format!("Bearer secret-{id}")))
            .respond_with(response)
            .mount(&server)
            .await;
    }
    let mut config = ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await
        .unwrap();
    config.chatgpt_base_url = server.uri();
    let report = account_usage_report(&config).await.unwrap();
    insta::assert_snapshot!("account_usage_report", report.to_string());
    insta::assert_snapshot!(
        "account_usage_report_json",
        serde_json::to_string_pretty(&report).unwrap()
    );
    assert_eq!(
        std::fs::read(home.path().join("accounts/index.json")).unwrap(),
        index
    );
    for (path, auth) in credentials {
        assert_eq!(std::fs::read_to_string(path).unwrap(), auth);
    }
    assert!(!home.path().join("auth.json").exists());
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn empty_report_does_not_create_an_account_store() {
    let home = tempfile::tempdir().unwrap();
    let config = ConfigBuilder::default()
        .codex_home(home.path().into())
        .build()
        .await
        .unwrap();
    assert_eq!(
        account_usage_report(&config).await.unwrap().to_string(),
        "accounts[0]{id,label,primary_window_minutes,primary_remaining_percent,primary_resets_at,weekly_remaining_percent,weekly_window_minutes,weekly_resets_at,status,error}:\n"
    );
    assert!(!home.path().join("accounts").exists());
}
