//! Temporary standalone browser launch repair for openai/codex#57641.
use codex_config::McpServerTransportConfig;
use codex_protocol::protocol::SessionSource;

use crate::McpRuntimeContext;
use crate::McpServerSource;
use crate::ResolvedMcpServer;
use crate::server::EffectiveMcpServer;

const ACCESS_CONTEXT: [&str; 3] = [
    "BROWSER_USE_ACCESS_EXECUTOR_ID",
    "BROWSER_USE_ACCESS_PIPE_PATH",
    "BROWSER_USE_ACCESS_VERIFYING_IDENTITY",
];

impl McpRuntimeContext {
    /// Captures the owning session's source for the standalone browser workaround.
    pub fn with_browser_session_source(mut self, source: &SessionSource) -> Self {
        self.browser_session_source = Some(source.clone());
        self
    }
}

impl EffectiveMcpServer {
    pub(crate) fn with_standalone_browser_access(
        mut self,
        name: &str,
        registration: Option<&ResolvedMcpServer>,
        source: Option<&SessionSource>,
        inherited: impl Fn(&str) -> bool,
    ) -> Self {
        let plugin_id = registration.and_then(|registration| match registration.source() {
            McpServerSource::Plugin(plugin) => Some(plugin.plugin_id()),
            _ => None,
        });
        if name == "cua_repl"
            && plugin_id == Some("unified-computer-use@openai-bundled")
            && self.config.is_local_environment()
            && matches!(source, Some(SessionSource::Cli | SessionSource::Exec))
            && let McpServerTransportConfig::Stdio { args, env, .. } = &mut self.config.transport
        {
            repair_browser_preload(args, env.as_ref(), inherited);
        }
        self
    }
}

fn repair_browser_preload(
    args: &mut Vec<String>,
    env: Option<&std::collections::HashMap<String, String>>,
    inherited: impl Fn(&str) -> bool,
) {
    // Presence, including an empty value, means the caller supplied access context.
    if ACCESS_CONTEXT
        .iter()
        .any(|key| inherited(key) || env.is_some_and(|env| env.contains_key(*key)))
    {
        return;
    }
    // The generated preload only supplies an empty executor ID. Without a host,
    // that converts optional verification into an unusable mandatory check.
    let mut index = 0;
    while index + 1 < args.len() {
        if args[index] == "--import"
            && args[index + 1]
                .replace('\\', "/")
                .ends_with("/require-browser-access.mjs")
        {
            args.drain(index..index + 2);
        } else {
            index += 1;
        }
    }
}

#[cfg(test)]
#[path = "browser_access_tests.rs"]
mod tests;
