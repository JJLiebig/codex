//! Restore child location using existing persisted settings and current parent permissions.

use crate::config::Config;
use crate::environment_selection::TurnEnvironmentSnapshot;
use codex_history::InitialHistory;
use codex_history::RolloutItem;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnEnvironmentSelection;
use std::path::Path;

pub(crate) fn restore(
    config: &mut Config,
    environments: Option<&TurnEnvironmentSnapshot>,
    history: &InitialHistory,
    thread_id: ThreadId,
    stored_cwd: &Path,
) -> Result<Option<Vec<TurnEnvironmentSelection>>, String> {
    let Some(environments) = environments else {
        return Ok(None);
    };
    let Some(current_cwd) = environments.local_environment_cwd() else {
        return Ok(None);
    };
    let cwd = history
        .get_rollout_items()
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::ThreadSettingsApplied(event))
                if event.thread_id == Some(thread_id) =>
            {
                Some(event.thread_settings.cwd.as_path())
            }
            _ => None,
        })
        .unwrap_or(stored_cwd);
    if current_cwd.as_path() == cwd {
        return Ok(None);
    }
    let cwd = cwd
        .to_str()
        .ok_or_else(|| "saved child cwd is not UTF-8".to_string())?;
    let roots = environments.selected_capability_roots();
    Ok(
        super::spawn_cwd::apply(config, environments, Some(cwd))?.map(|requests| {
            requests
                .into_iter()
                .map(|request| TurnEnvironmentSelection::new(request, &roots))
                .collect()
        }),
    )
}

#[cfg(test)]
#[path = "spawn_cwd_resume_tests.rs"]
mod tests;
