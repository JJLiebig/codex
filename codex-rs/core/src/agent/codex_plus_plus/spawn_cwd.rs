//! Optional child working directory, shared by both spawn tools.

use crate::config::Config;
use crate::environment_selection::TurnEnvironmentSnapshot;
use codex_exec_server::LOCAL_ENVIRONMENT_ID;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::TurnEnvironmentRequest;
use codex_tools::JsonSchema;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path::replace_path_and_deduplicate;
use codex_utils_path_uri::PathUri;
use std::collections::BTreeMap;

pub(crate) fn add_property(properties: &mut BTreeMap<String, JsonSchema>) {
    properties.insert(
        "cwd".to_string(),
        JsonSchema::string(Some(
            "Working directory for the new agent. Relative paths resolve against the parent's local working directory. Omit to inherit.".to_string(),
        )),
    );
}

pub(crate) fn apply(
    config: &mut Config,
    environments: &TurnEnvironmentSnapshot,
    cwd: Option<&str>,
) -> Result<Option<Vec<TurnEnvironmentRequest>>, String> {
    let Some(cwd) = cwd else {
        config.subagent_cwd_override = false;
        return Ok(None);
    };
    if cwd.is_empty() {
        return Err("spawn_agent cwd must not be empty".to_string());
    }
    let base = environments
        .local_environment_cwd()
        .unwrap_or_else(|| config.cwd.clone());
    let cwd = AbsolutePathBuf::resolve_path_against_base(cwd, &base);
    if !cwd.is_dir() {
        return Err(format!(
            "spawn_agent cwd must be an existing directory: {}",
            cwd.display()
        ));
    }
    let mut requests = environments.inheritable_requests();
    let local = requests
        .iter_mut()
        .find(|request| request.environment_id == LOCAL_ENVIRONMENT_ID)
        .ok_or_else(|| "spawn_agent cwd requires a local execution environment".to_string())?;
    let old_roots: Vec<_> = config
        .workspace_roots
        .iter()
        .map(PathUri::from_abs_path)
        .collect();
    if config.workspace_roots_include_cwd {
        config.workspace_roots = replace_path_and_deduplicate(
            config.workspace_roots.clone(),
            config.cwd.as_path(),
            cwd.clone(),
        );
        config
            .permissions
            .set_workspace_roots(config.workspace_roots.clone());
        if matches!(local.config, EnvironmentConfigState::FromThread)
            && local.workspace_roots == old_roots
        {
            local.workspace_roots = config
                .workspace_roots
                .iter()
                .map(PathUri::from_abs_path)
                .collect();
        }
    }
    local.cwd = PathUri::from_abs_path(&cwd);
    config.cwd = cwd;
    config.subagent_cwd_override = true;
    Ok(Some(requests))
}

#[cfg(test)]
#[path = "spawn_cwd_tests.rs"]
mod tests;
