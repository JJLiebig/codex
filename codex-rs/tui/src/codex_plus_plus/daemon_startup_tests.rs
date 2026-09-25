use super::requires_embedded;

#[test]
fn only_windows_job_detachment_failure_uses_embedded() {
    let error = anyhow::anyhow!(
        "host Job Object prevents daemon detachment; start from a host that allows breakaway"
    )
    .context("daemon startup failed");
    assert_eq!(requires_embedded(&error), cfg!(windows));
    assert!(!requires_embedded(&anyhow::anyhow!(
        "daemon startup failed"
    )));
}
