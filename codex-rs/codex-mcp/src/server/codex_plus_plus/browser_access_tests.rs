use super::*;
use pretty_assertions::assert_eq;
use std::collections::HashMap;

fn generated_args() -> Vec<String> {
    [
        "--import",
        "file:///plugin/require-browser-access.mjs",
        "/runtime/cua-repl.mjs",
    ]
    .map(str::to_string)
    .to_vec()
}

#[test]
fn standalone_launch_removes_only_browser_preload_after_regeneration() {
    for root in ["file:///plugin/old", "file:///plugin/new"] {
        let original = vec![
            "--import".to_string(),
            format!("{root}/require-browser-access.mjs"),
            "--import".to_string(),
            "file:///other/keep.mjs".to_string(),
            "/runtime/cua-repl.mjs".to_string(),
        ];
        let mut args = original.clone();
        repair_browser_preload(&mut args, None, |_| false);
        assert_eq!(args, original[2..]);
        repair_browser_preload(&mut args, None, |_| false);
        assert_eq!(args, original[2..]);
    }
}

#[test]
fn any_supplied_access_context_preserves_verification() {
    for key in ACCESS_CONTEXT {
        for value in ["", "supplied"] {
            let original = generated_args();
            let mut args = original.clone();
            repair_browser_preload(
                &mut args,
                Some(&HashMap::from([(key.to_string(), value.to_string())])),
                |_| false,
            );
            assert_eq!(args, original);
            repair_browser_preload(&mut args, None, |inherited| inherited == key);
            assert_eq!(args, original);
        }
    }
}

#[test]
fn desktop_unknown_remote_and_unrelated_servers_preserve_verification() {
    use crate::McpPluginAttribution;
    use crate::McpServerRegistration;
    use crate::ResolvedMcpCatalog;
    let config: codex_config::McpServerConfig = serde_json::from_value(serde_json::json!({
        "command": "node", "args": generated_args(),
    }))
    .unwrap();
    for plugin in [
        "unified-computer-use@openai-bundled",
        "other@openai-bundled",
    ] {
        let mut builder = ResolvedMcpCatalog::builder();
        builder.register(McpServerRegistration::from_plugin(
            "cua_repl".to_string(),
            McpPluginAttribution::new(plugin.to_string(), plugin.to_string()),
            /*plugin_order*/ 0,
            config.clone(),
        ));
        let catalog = builder.build();
        for source in [SessionSource::Cli, SessionSource::Exec] {
            let server = EffectiveMcpServer::from_host_config(config.clone())
                .with_standalone_browser_access(
                    "cua_repl",
                    catalog.server("cua_repl"),
                    Some(&source),
                    |_| false,
                );
            let mut expected = config.clone();
            if plugin == "unified-computer-use@openai-bundled"
                && let McpServerTransportConfig::Stdio { args, .. } = &mut expected.transport
            {
                *args = vec!["/runtime/cua-repl.mjs".to_string()];
            }
            assert_eq!(server.config(), &expected);
        }

        for source in [
            None,
            Some(SessionSource::VSCode),
            Some(SessionSource::Unknown),
            Some(SessionSource::Custom("desktop".to_string())),
        ] {
            let server = EffectiveMcpServer::from_host_config(config.clone())
                .with_standalone_browser_access(
                    "cua_repl",
                    catalog.server("cua_repl"),
                    source.as_ref(),
                    |_| false,
                );
            assert_eq!(server.config(), &config);
        }
        let server = EffectiveMcpServer::from_host_config(config.clone())
            .with_standalone_browser_access(
                "other",
                catalog.server("cua_repl"),
                Some(&SessionSource::Cli),
                |_| false,
            );
        assert_eq!(server.config(), &config);
        let mut remote = config.clone();
        remote.environment_id = "remote".to_string();
        let server = EffectiveMcpServer::from_host_config(remote.clone())
            .with_standalone_browser_access(
                "cua_repl",
                catalog.server("cua_repl"),
                Some(&SessionSource::Exec),
                |_| false,
            );
        assert_eq!(server.config(), &remote);
    }
}
