use super::super::cli_proxy_credentials::native_route;
use super::super::cli_proxy_inventory::RegisteredModel;
use super::*;
use pretty_assertions::assert_eq;

#[test]
fn exact_route_rejects_missing_selected_membership_and_conflicting_ownership() {
    let account = serde_json::from_str("\"same-account\"").unwrap();
    let sources = [
        NativeCredentialSource::Root(account),
        NativeCredentialSource::Imported(serde_json::from_str("\"same-account\"").unwrap()),
    ];
    let inventory: Vec<_> = sources
        .iter()
        .map(|source| {
            let (name, prefix) = native_route(source);
            CredentialModels {
                source: Some(source.clone()),
                auth_index: Some(name.clone()),
                name,
                provider: Some("codex".into()),
                prefix: Some(prefix.clone()),
                disabled: false,
                models: vec![RegisteredModel {
                    id: format!("{prefix}/future-9.7"),
                    provider: Some("openai".into()),
                    owned_by: Some("openai".into()),
                }],
            }
        })
        .collect();
    for (index, selected) in sources.iter().enumerate() {
        for model in ["future-9.7", inventory[index].models[0].id.as_str()] {
            let (wire, route) = resolve_route(
                model,
                Some(selected),
                inventory.clone(),
                /*auth_revision*/ 7,
            )
            .unwrap();
            assert_eq!(
                (
                    wire,
                    route.native_source(),
                    route.native_auth_index(),
                    route.auth_revision
                ),
                (
                    inventory[index].models[0].id.clone(),
                    Some(selected),
                    Some(inventory[index].name.as_str()),
                    7
                )
            );
        }
    }
    assert!(
        resolve_route(
            "future-9.7",
            Some(&sources[1]),
            inventory[..1].to_vec(),
            /*auth_revision*/ 7
        )
        .is_err()
    );
    assert!(
        resolve_route(
            "unknown",
            Some(&sources[0]),
            inventory.clone(),
            /*auth_revision*/ 7
        )
        .is_err()
    );
    for case in 0..5 {
        let mut conflict = inventory.clone();
        match case {
            0 => conflict[0].disabled = true,
            1 => conflict[0].models[0].provider = None,
            2 => conflict[1].auth_index = conflict[0].auth_index.clone(),
            3 => conflict.push(conflict[0].clone()),
            4 => {
                conflict[1].source = None;
                conflict[1].prefix = None;
                conflict[1].name = "foreign.json".into();
                conflict[1].models[0].id = "future-9.7".into();
            }
            _ => unreachable!(),
        }
        assert!(
            resolve_route(
                "future-9.7",
                Some(&sources[0]),
                conflict,
                /*auth_revision*/ 7
            )
            .is_err()
        );
    }
    let claude = CredentialModels {
        source: None,
        auth_index: Some("claude-a".into()),
        name: "claude.json".into(),
        provider: Some("claude".into()),
        prefix: None,
        disabled: false,
        models: vec![RegisteredModel {
            id: "new/claude".into(),
            provider: Some("claude".into()),
            owned_by: Some("anthropic".into()),
        }],
    };
    let (wire, route) = resolve_route(
        "new/claude",
        Some(&sources[0]),
        vec![claude.clone(), claude],
        /*auth_revision*/ 7,
    )
    .unwrap();
    assert_eq!(
        (
            wire.as_str(),
            route.native_source(),
            route.native_auth_index()
        ),
        ("new/claude", None, None)
    );
}

#[tokio::test]
async fn native_configured_request_keeps_its_model_and_auth() {
    let provider = crate::create_model_provider(
        codex_model_provider_info::ModelProviderInfo {
            base_url: Some("http://127.0.0.1:1/v1".into()),
            experimental_bearer_token: Some("configured-secret".into()),
            ..Default::default()
        },
        /*auth_manager*/ None,
    );
    let prepared = prepare_configured_request(provider.as_ref(), "native-model")
        .await
        .unwrap();
    let mut headers = http::HeaderMap::new();
    prepared.auth.auth.add_auth_headers(&mut headers);
    assert_eq!(
        (
            prepared.model.as_str(),
            prepared.provider.base_url.as_str(),
            headers
                .get(http::header::AUTHORIZATION)
                .unwrap()
                .to_str()
                .unwrap()
        ),
        (
            "native-model",
            "http://127.0.0.1:1/v1",
            "Bearer configured-secret"
        )
    );
    assert!(prepared.route.is_none() && prepared.http_client.is_none());
}
