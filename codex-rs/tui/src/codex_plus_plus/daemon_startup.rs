//! Windows hosts may keep Codex inside a Job Object that prevents a durable daemon.

#[cfg(windows)]
pub(crate) fn requires_embedded(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.to_string()
            == "host Job Object prevents daemon detachment; start from a host that allows breakaway"
    })
}

#[cfg(not(windows))]
pub(crate) fn requires_embedded(_error: &anyhow::Error) -> bool {
    false
}

#[cfg(test)]
#[path = "daemon_startup_tests.rs"]
mod tests;
