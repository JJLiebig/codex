//! Composable exact-origin narrowing for an owned provider's HTTP client.

use reqwest::Url;

use super::NetworkPolicy;

#[derive(Clone, PartialEq, Eq)]
pub(super) struct OriginRestriction {
    // A missing origin is the empty intersection, which remains denied on further narrowing.
    origin: Option<Url>,
}

impl OriginRestriction {
    pub(super) fn allows(&self, url: &Url) -> bool {
        self.origin
            .as_ref()
            .is_some_and(|origin| origin.origin() == url.origin())
    }
}

impl NetworkPolicy {
    /// Narrows this policy to one HTTPS host and effective port, allowing all paths and queries.
    /// Existing managed/account and endpoint restrictions remain enforced. Repeated calls intersect.
    /// Non-HTTPS URLs cannot authorize network destinations.
    /// Request and redirect enforcement requires a route-aware client, such as
    /// [`crate::HttpClientBuilder::build_with_tls`]; raw transports do not acquire policy permits.
    pub fn restrict_to_origin(mut self, origin: Url) -> Self {
        let mut restriction = OriginRestriction {
            origin: Url::parse(&origin.origin().ascii_serialization())
                .ok()
                .filter(|origin| origin.scheme() == "https"),
        };
        if self
            .origin
            .as_ref()
            .is_some_and(|existing| existing != &restriction)
        {
            restriction.origin = None;
        }
        self.origin = Some(restriction);
        self
    }
}

#[cfg(test)]
#[path = "origin_restriction_tests.rs"]
mod tests;
