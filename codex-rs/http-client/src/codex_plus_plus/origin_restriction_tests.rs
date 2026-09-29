use std::collections::BTreeSet;

use pretty_assertions::assert_eq;
use reqwest::Url;

use crate::DestinationPolicy;
use crate::NetworkPolicy;
use crate::NetworkPolicyController;
use crate::NetworkPolicyDenied;

#[test]
fn origin_restriction_matches_scheme_host_and_effective_port() {
    let policy = NetworkPolicy::unmanaged()
        .restrict_to_origin(Url::parse("https://EXAMPLE.com:443/start?initial=true").unwrap());
    for (url, expected) in [
        ("https://example.com/v1/responses?stream=true", true),
        ("https://example.com:443/management/accounts", true),
        ("http://example.com/v1/responses", false),
        ("https://example.com:8443/v1/responses", false),
        ("https://other.example.com/v1/responses", false),
        ("https://example.com@evil.test/v1/responses", false),
    ] {
        assert_eq!(
            policy.acquire(&Url::parse(url).unwrap()).is_ok(),
            expected,
            "{url}"
        );
    }
    assert_eq!(
        policy.acquire_for_unsupported_sdk().unwrap_err(),
        NetworkPolicyDenied::UnsupportedTransport
    );
}

#[test]
fn origin_restriction_intersects_and_retains_endpoint_scope() {
    let endpoint = Url::parse("https://127.0.0.1:8443/v1/responses?stream=true").unwrap();
    let policy = NetworkPolicy::unmanaged()
        .restrict_to_endpoints(BTreeSet::from([endpoint.clone()]))
        .restrict_to_origin(Url::parse("https://127.0.0.1:8443/other").unwrap());
    assert!(policy.acquire(&endpoint).is_ok());
    assert_eq!(
        policy
            .acquire(&Url::parse("https://127.0.0.1:8443/v1/models").unwrap())
            .unwrap_err(),
        NetworkPolicyDenied::Destination
    );
    let same = policy.clone().restrict_to_origin(endpoint.clone());
    assert_eq!(same, policy);
    assert!(same.acquire(&endpoint).is_ok());
    let denied = same
        .restrict_to_origin(Url::parse("https://127.0.0.1:9443/").unwrap())
        .restrict_to_origin(endpoint.clone());
    assert_eq!(
        denied.acquire(&endpoint).unwrap_err(),
        NetworkPolicyDenied::Destination
    );
}

#[test]
fn origin_restriction_preserves_managed_denial_and_account_revocation() {
    let controller = NetworkPolicyController::default();
    let original = controller.policy();
    let endpoint = Url::parse("https://127.0.0.1:8443/v1/responses").unwrap();
    let policy = original
        .clone()
        .for_current_account()
        .restrict_to_origin(endpoint.clone());
    assert_eq!(
        policy.acquire(&endpoint).unwrap_err(),
        NetworkPolicyDenied::Unavailable
    );
    controller.publish(
        original.revision(),
        DestinationPolicy::Restricted {
            allowed_hosts: BTreeSet::from(["other.example.com".to_string()]),
        },
    );
    assert_eq!(
        policy.acquire(&endpoint).unwrap_err(),
        NetworkPolicyDenied::Destination
    );
    controller.publish(original.revision(), DestinationPolicy::Unrestricted);
    let permit = policy.acquire(&endpoint).unwrap();
    original.invalidate();
    controller.publish(original.revision(), DestinationPolicy::Unrestricted);
    assert_eq!(permit.check(), Err(NetworkPolicyDenied::Revoked));
    assert_eq!(
        policy.acquire(&endpoint).unwrap_err(),
        NetworkPolicyDenied::Revoked
    );
    assert!(original.acquire(&endpoint).is_ok());
}
