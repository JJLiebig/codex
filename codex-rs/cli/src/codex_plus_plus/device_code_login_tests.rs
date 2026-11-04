use super::login_with_device_code;
use codex_config::types::AuthCredentialsStoreMode;
use codex_login::AccountStore;
use codex_login::AuthKeyringBackendKind;
use codex_login::ServerOptions;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn returning_device_login_supports_import_and_rejects_failed_grants() -> anyhow::Result<()> {
    for (status, workspaces, succeeds) in [
        (200, None, true),
        (404, None, false),
        (200, Some(vec!["other-workspace".to_string()]), false),
    ] {
        let home = tempfile::tempdir()?;
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/usercode"))
            .respond_with(ResponseTemplate::new(status).set_body_json(json!({
                "device_auth_id": "synthetic-device",
                "user_code": "SYNTHETIC",
                "interval": "0"
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/accounts/deviceauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "authorization_code": "synthetic-code",
                "code_challenge": "synthetic-challenge",
                "code_verifier": "synthetic-verifier"
            })))
            .mount(&server)
            .await;
        let jwt = jsonwebtoken::encode(
            &jsonwebtoken::Header::default(),
            &json!({
                "email": "device@example.test",
                "https://api.openai.com/auth": { "chatgpt_account_id": "device-account" }
            }),
            &jsonwebtoken::EncodingKey::from_secret(b"synthetic-test-secret"),
        )?;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "id_token": jwt,
                "access_token": "synthetic-access",
                "refresh_token": "synthetic-refresh"
            })))
            .mount(&server)
            .await;
        let mut opts = ServerOptions::new(
            home.path().to_path_buf(),
            codex_login::CLIENT_ID.to_string(),
            workspaces,
            AuthCredentialsStoreMode::File,
            AuthKeyringBackendKind::default(),
            codex_login::test_support::transport_default_auth_route_config(),
        );
        opts.issuer = server.uri();
        let result = login_with_device_code(opts).await;
        let store = AccountStore::new(home.path().to_path_buf());
        if succeeds {
            result?;
            let profile = store.import_current(
                /*label*/ None,
                AuthCredentialsStoreMode::File,
                AuthKeyringBackendKind::default(),
            )?;
            assert_eq!(
                (profile.id.as_str(), profile.label.as_str()),
                ("acct_2e40ed53f7fd9cb4", "device@example.test")
            );
            assert_eq!(store.list()?, vec![profile]);
        } else {
            assert!(result.is_err());
            assert!(
                store
                    .import_current(
                        /*label*/ None,
                        AuthCredentialsStoreMode::File,
                        AuthKeyringBackendKind::default(),
                    )
                    .is_err()
            );
            assert!(store.list()?.is_empty());
        }
    }
    Ok(())
}
