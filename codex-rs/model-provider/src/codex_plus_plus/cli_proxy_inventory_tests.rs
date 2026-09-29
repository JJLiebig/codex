use super::*;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;
use wiremock::matchers::query_param;

fn source(account: &str) -> NativeCredentialSource {
    NativeCredentialSource::Imported(serde_json::from_value(json!(account)).unwrap())
}

#[tokio::test]
async fn exact_membership_preserves_foreign_and_unknown_metadata_without_guessing() {
    let server = MockServer::start().await;
    let owned = source("opaque-account");
    let (name, prefix) = native_route(&owned);
    let foreign = "foreign ?&/account.json";
    Mock::given(method("GET"))
        .and(path("/v0/management/auth-files"))
        .and(header("authorization", "Bearer synthetic-management"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "files": [
                {"name":name,"type":"codex","provider":"codex","auth_index":"exact-index","disabled":false,"new_field":42},
                {"name":foreign,"type":"claude","provider":"claude","auth_index":"foreign-index","disabled":true},
                {"name":"codex-native-imported-not-owned.json","type":"codex","provider":"codex","disabled":false},
                {"name":"unclassified.json","auth_index":"unknown","disabled":false}
            ], "extra":true
        })))
        .mount(&server).await;
    for file in [
        &name,
        foreign,
        "codex-native-imported-not-owned.json",
        "unclassified.json",
    ] {
        Mock::given(method("GET"))
            .and(path("/v0/management/auth-files/models"))
            .and(query_param("name", file))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[
                {"id":"vendor/new/unknown-x9.7","type":"claude","owned_by":"anthropic","future":true}
            ],"extra":0})))
            .mount(&server).await;
    }
    let endpoint = RuntimeEndpoint {
        base_url: server.uri(),
        inference_key: "synthetic-inference".into(),
        management_key: "synthetic-management".into(),
    };
    let client = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
        .build_client(&server.uri(), ClientRouteClass::Other)
        .unwrap();
    let inventory = read_inventory(client, &endpoint, std::slice::from_ref(&owned))
        .await
        .unwrap();
    assert_eq!(
        inventory[0],
        CredentialModels {
            source: Some(owned),
            name,
            auth_index: Some("exact-index".into()),
            provider: Some("codex".into()),
            prefix: Some(prefix),
            disabled: false,
            models: vec![RegisteredModel {
                id: "vendor/new/unknown-x9.7".into(),
                provider: Some("claude".into()),
                owned_by: Some("anthropic".into())
            }]
        }
    );
    assert!(
        inventory[1..]
            .iter()
            .all(|credential| credential.source.is_none() && credential.prefix.is_none())
    );
    assert!(inventory[1].disabled);
    assert_eq!(inventory[3].provider, None);
}

#[tokio::test]
async fn conflicts_missing_identity_and_aggregate_overlimit_fail_closed() {
    let server = MockServer::start().await;
    let owned = source("a");
    let (name, _) = native_route(&owned);
    let endpoint = RuntimeEndpoint {
        base_url: server.uri(),
        inference_key: String::new(),
        management_key: "fake".into(),
    };
    for (kind, provider, index) in [
        (Some("codex"), Some("claude"), Some("index")),
        (None, None, Some("index")),
        (Some("codex"), Some("codex"), None),
    ] {
        server.reset().await;
        Mock::given(path("/v0/management/auth-files"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files":[{"name":name,"type":kind,"provider":provider,"auth_index":index,"disabled":false}]})))
            .mount(&server).await;
        Mock::given(path("/v0/management/auth-files/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models":[]})))
            .mount(&server)
            .await;
        let client = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
            .build_client(&server.uri(), ClientRouteClass::Other)
            .unwrap();
        let inventory = read_inventory(client, &endpoint, std::slice::from_ref(&owned))
            .await
            .unwrap();
        assert_eq!((&inventory[0].source, &inventory[0].prefix), (&None, &None));
    }
    server.reset().await;
    Mock::given(path("/v0/management/auth-files"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"files":[{"name":name,"type":"codex","auth_index":"index","disabled":false}],"padding":"x".repeat(INVENTORY_BYTES/2)})))
        .mount(&server).await;
    Mock::given(path("/v0/management/auth-files/models"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"models":[],"padding":"y".repeat(INVENTORY_BYTES/2)})),
        )
        .mount(&server)
        .await;
    let client = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
        .build_client(&server.uri(), ClientRouteClass::Other)
        .unwrap();
    assert!(
        read_inventory(client, &endpoint, &[owned])
            .await
            .unwrap_err()
            .to_string()
            .contains("limit")
    );
}

#[test]
fn canonical_catalogue_requires_membership_and_preserves_all_rich_metadata() {
    let template = codex_models_manager::bundled_models_response()
        .unwrap()
        .models
        .remove(0);
    let retained = [
        "gpt-5.5",
        "gpt-5.6-sol",
        "gpt-5.10-sol",
        "gpt-6-astra",
        "gpt-6-astra-minor",
        "gpt-6.1-astra",
        "gpt-6.1-sol",
        "gpt-6.2-whatever",
        "future/unknown-x9.7",
    ];
    let excluded = [
        "gpt-5.4",
        "gpt-5.4-astra",
        "gpt-5.3-sol",
        "gpt-5",
        "gpt-4.1",
        "gpt-4o-mini",
        "gpt-3.5-turbo",
    ];
    let mut inventory = Vec::new();
    let mut rich = Vec::new();
    for account in ["a", "b"] {
        let source = source(account);
        let (name, prefix) = native_route(&source);
        let mut models = Vec::new();
        for slug in retained.iter().chain(&excluded) {
            let id = format!("{prefix}/{slug}");
            let mut model = template.clone();
            model.slug = id.clone();
            rich.push(model);
            models.push(RegisteredModel {
                id,
                provider: Some("openai".into()),
                owned_by: Some("openai".into()),
            });
        }
        inventory.push(CredentialModels {
            source: Some(source),
            name,
            auth_index: Some(account.into()),
            provider: Some("codex".into()),
            prefix: Some(prefix),
            disabled: false,
            models,
        });
    }
    let mut foreign = template.clone();
    foreign.slug = "gpt-5.4-foreign".into();
    rich.push(foreign.clone());
    let mut unknown = template.clone();
    unknown.slug = "unknown/with/slash".into();
    rich.push(unknown.clone());
    let mut expected: Vec<_> = retained
        .iter()
        .map(|slug| {
            let mut model = template.clone();
            model.slug = (*slug).into();
            model
        })
        .collect();
    expected.extend([foreign, unknown]);
    expected.sort_by(|a, b| a.slug.cmp(&b.slug));
    assert_eq!(normalize_catalogue(rich.clone(), &inventory), expected);
    // A provider conflict, disabled record, or a missing model ownership field cannot normalize.
    for case in 0..3 {
        let mut conflicted = inventory[0].clone();
        match case {
            0 => conflicted.provider = Some("claude".into()),
            1 => conflicted.disabled = true,
            2 => conflicted.models[0].owned_by = None,
            _ => unreachable!(),
        }
        assert_eq!(
            normalize_catalogue(vec![rich[0].clone()], &[conflicted]),
            vec![rich[0].clone()]
        );
    }
}

#[test]
fn conflicting_prompts_and_foreign_collisions_keep_original_entries() {
    let mut first = codex_models_manager::bundled_models_response()
        .unwrap()
        .models
        .remove(0);
    first.slug = "codex-native-imported-a/gpt-6.1-sol".into();
    let mut second = first.clone();
    second.slug = "codex-native-imported-b/gpt-6.1-sol".into();
    second
        .model_messages
        .as_mut()
        .unwrap()
        .instructions_template
        .as_mut()
        .unwrap()
        .push_str("different prompt");
    let inventory: Vec<_> = ["a", "b"]
        .iter()
        .map(|account| {
            let source = source(account);
            let (name, prefix) = native_route(&source);
            CredentialModels {
                source: Some(source),
                name,
                auth_index: Some((*account).into()),
                provider: Some("codex".into()),
                prefix: Some(prefix.clone()),
                disabled: false,
                models: vec![RegisteredModel {
                    id: format!("{prefix}/gpt-6.1-sol"),
                    provider: Some("openai".into()),
                    owned_by: Some("openai".into()),
                }],
            }
        })
        .collect();
    assert_eq!(
        normalize_catalogue(vec![first.clone(), second.clone()], &inventory),
        vec![first.clone(), second]
    );
    let mut foreign = first.clone();
    foreign.slug = "gpt-6.1-sol".into();
    assert_eq!(
        normalize_catalogue(vec![first.clone(), foreign.clone()], &inventory),
        vec![first, foreign]
    );
}
