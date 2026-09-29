use super::*;
use codex_http_client::ClientRouteClass;
use codex_http_client::OutboundProxyPolicy;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[tokio::test]
async fn claude_listing_filters_provider_and_never_fetches_auth_contents() {
    let server = MockServer::start().await;
    Mock::given(method("GET")).and(path("/v0/management/auth-files"))
        .and(header("authorization", "Bearer synthetic-management"))
        .respond_with(ResponseTemplate::new(/*status_code*/ 200).set_body_json(json!({"files":[
            {"name":"claude-first.json","provider":"claude","type":"claude","email":"one@example.invalid","disabled":false,"unavailable":false,"access_token":"secret-one","status_message":"secret-error"},
            {"name":"claude-second.json","provider":"claude","disabled":true,"unavailable":false,"refresh_token":"secret-two"},
            {"name":"claude-unknown.json","provider":"claude","unexpected":42},
            {"name":"codex.json","provider":"codex","type":"codex","disabled":false},
            {"name":"ambiguous.json","provider":"claude","type":"codex","disabled":false},
            {"name":"unclassified.json","type":"claude","disabled":false}
        ],"future":true}))).expect(1).mount(&server).await;
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let client = factory
        .build_client(&server.uri(), ClientRouteClass::Other)
        .unwrap();
    let endpoint = RuntimeEndpoint {
        base_url: server.uri(),
        inference_key: "synthetic-inference".into(),
        management_key: "synthetic-management".into(),
    };
    assert_eq!(
        read_claude_accounts(client, &endpoint).await.unwrap(),
        vec![
            ClaudeAccount {
                name: "claude-first.json".into(),
                email: Some("one@example.invalid".into()),
                disabled: false,
                unavailable: false
            },
            ClaudeAccount {
                name: "claude-second.json".into(),
                email: None,
                disabled: true,
                unavailable: false
            },
            ClaudeAccount {
                name: "claude-unknown.json".into(),
                email: None,
                disabled: true,
                unavailable: true
            },
        ]
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn claude_listing_without_runtime_does_not_provision_or_start() {
    let home = tempfile::tempdir().unwrap();
    assert_eq!(
        list_cli_proxy_claude_accounts(
            home.path(),
            HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
        )
        .await
        .unwrap(),
        None
    );
    assert!(!home.path().join("cli-proxy").exists());
}
