//! Optional child working directory, shared by both spawn tools.

use crate::config::Config;
use crate::environment_selection::TurnEnvironmentSnapshot;
use codex_exec_server::LOCAL_ENVIRONMENT_ID;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::TurnEnvironmentRequest;
use codex_tools::JsonSchema;
use codex_utils_path_uri::PathUri;
use std::collections::BTreeMap;

pub(crate) fn add_property(properties: &mut BTreeMap<String, JsonSchema>) {
    properties.insert("cwd".to_string(), JsonSchema::string(Some(
        "Working directory for the new agent. Relative paths resolve against the caller's working directory. Omit to inherit.".to_string(),
    )));
}

pub(crate) fn apply(
    config: &mut Config,
    environments: &TurnEnvironmentSnapshot,
    cwd: Option<&str>,
) -> Result<Option<Vec<TurnEnvironmentRequest>>, String> {
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    let mut requests = environments.inheritable_requests();
    let base = requests
        .first()
        .map(|environment| environment.cwd.clone())
        .unwrap_or_else(|| PathUri::from_abs_path(&config.cwd));
    let cwd = base.join(cwd).map_err(|err| err.to_string())?;
    let update_host_cwd = requests
        .first()
        .is_none_or(|environment| environment.environment_id == LOCAL_ENVIRONMENT_ID);
    if let Some(environment) = requests.first_mut() {
        rebind_roots(&mut environment.workspace_roots, &base, &cwd);
        if let EnvironmentConfigState::Ready(config) = &mut environment.config {
            rebind_roots(&mut config.workspace_roots, &base, &cwd);
        }
        environment.cwd = cwd.clone();
    }
    if update_host_cwd {
        let cwd = cwd.to_abs_path().map_err(|err| err.to_string())?;
        // Match the existing cwd-change behavior while retaining the configured policy.
        config.workspace_roots = codex_utils_path::replace_path_and_deduplicate(
            config.workspace_roots.clone(),
            config.cwd.as_path(),
            cwd.clone(),
        );
        config
            .permissions
            .set_workspace_roots(config.workspace_roots.clone());
        config.cwd = cwd;
    }
    Ok((!requests.is_empty()).then_some(requests))
}

pub(crate) fn rebind_roots(roots: &mut [PathUri], previous: &PathUri, cwd: &PathUri) {
    for root in roots {
        if root == previous {
            *root = cwd.clone();
        }
    }
}

#[cfg(test)]
#[path = "spawn_cwd_tests.rs"]
mod tests;
