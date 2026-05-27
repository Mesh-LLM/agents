use std::path::PathBuf;

use anyhow::Result;
use mesh_llm_plugin::{
    capability, plugin_server_info, PluginMetadata, PluginRuntime, PluginStartupPolicy,
};

const DEFAULT_PLUGIN_NAME: &str = "agents";
const MCP_ENDPOINT_ID: &str = "mcp";

pub(crate) async fn run_plugin_from_env() -> Result<()> {
    let plugin_name = std::env::var("MESH_LLM_PLUGIN_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PLUGIN_NAME.to_string());
    PluginRuntime::run(build_plugin(plugin_name)).await
}

fn build_plugin(name: String) -> mesh_llm_plugin::SimplePlugin {
    let command = current_agents_command();
    build_plugin_with_command(name, command)
}

fn build_plugin_with_command(name: String, command: String) -> mesh_llm_plugin::SimplePlugin {
    mesh_llm_plugin::plugin! {
        metadata: PluginMetadata::new(
            name,
            env!("CARGO_PKG_VERSION"),
            plugin_server_info(
                "mesh-agents",
                env!("CARGO_PKG_VERSION"),
                "Mesh Agents",
                "Discovers A2A agents and exposes official A2A MCP client tools.",
                Some("Use the MCP endpoint to discover agents, submit A2A tasks, inspect task state, and read artifacts."),
            ),
        ),
        startup_policy: PluginStartupPolicy::Any,
        provides: [
            capability("agents:a2a"),
            capability("endpoint:mcp"),
            capability("endpoint:mcp/a2a"),
        ],
        mcp: [
            mesh_llm_plugin::mcp::external_stdio(MCP_ENDPOINT_ID, command)
                .arg("mcp")
                .namespace("a2a"),
        ],
        health: |_context| {
            Box::pin(async move { Ok("mcp=agents mcp".to_string()) })
        },
    }
}

fn current_agents_command() -> String {
    std::env::current_exe()
        .ok()
        .and_then(path_to_string)
        .unwrap_or_else(|| DEFAULT_PLUGIN_NAME.to_string())
}

fn path_to_string(path: PathBuf) -> Option<String> {
    let value = path.into_os_string().into_string().ok()?;
    if value.trim().is_empty() {
        None
    } else {
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use mesh_llm_plugin::Plugin;

    use super::*;

    #[test]
    fn manifest_advertises_agents_mcp_stdio_endpoint() {
        let plugin = build_plugin_with_command("agents".to_string(), "agents".to_string());
        let manifest = plugin.manifest().expect("plugin manifest");
        assert_eq!(manifest.endpoints.len(), 1);
        let endpoint = &manifest.endpoints[0];

        assert_eq!(endpoint.endpoint_id, MCP_ENDPOINT_ID);
        assert_eq!(
            endpoint.address.as_deref().map(command_basename),
            Some("agents")
        );
        assert_eq!(endpoint.args, vec!["mcp"]);
        assert_eq!(endpoint.namespace.as_deref(), Some("a2a"));
    }

    fn command_basename(command: &str) -> &str {
        command.rsplit('/').next().unwrap_or(command)
    }
}
