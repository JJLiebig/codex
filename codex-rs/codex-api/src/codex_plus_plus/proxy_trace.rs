//! Preserve only bounded attribution metadata from the owned HTTP response.
pub(crate) fn from_headers(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get("x-cpa-trace-id")?
        .to_str()
        .ok()
        .filter(|trace| trace.len() <= 128)
        .map(str::to_owned)
}
