use super::*;
use pretty_assertions::assert_eq;

#[test]
fn reserved_provider_discards_supplied_endpoint_and_credentials() {
    let supplied = ModelProviderInfo {
        name: codex_model_provider_info::CLI_PROXY_PROVIDER_NAME.into(),
        base_url: Some("http://127.0.0.1:9999/v1".into()),
        model_catalog_url: Some("http://127.0.0.1:9999/models".into()),
        experimental_bearer_token: Some("must-not-escape".into()),
        ..ModelProviderInfo::default()
    };
    let provider = crate::provider::create_model_provider(supplied, None);
    assert_eq!(
        provider.info(),
        &ModelProviderInfo::create_cli_proxy_provider()
    );
}
