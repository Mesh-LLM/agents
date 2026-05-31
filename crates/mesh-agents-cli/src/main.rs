mod a2a;
mod agents;
mod mesh;
mod plugin;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(name = "agents")]
#[command(about = "Mesh-native A2A agents for mesh-llm clients")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run the A2A MCP server over stdio.
    Mcp {
        /// Agents directory. Defaults to ~/.mesh-llm/agents.
        #[arg(long)]
        agents_dir: Option<PathBuf>,
        /// mesh-llm data directory. Defaults to ~/.mesh-llm.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Manage local agent definitions.
    Agents {
        #[command(subcommand)]
        command: AgentsCommand,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum AgentsCommand {
    /// List local agent definitions.
    List {
        /// Agents directory. Defaults to ~/.mesh-llm/agents.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Create an agent directory with starter files.
    Init {
        /// Agent id, used as ~/.mesh-llm/agents/<agent-id>.
        agent_id: String,
        /// Runtime harness to configure.
        #[arg(long, value_enum, default_value_t = AgentRuntimeArg::Opencode)]
        runtime: AgentRuntimeArg,
        /// Agents directory. Defaults to ~/.mesh-llm/agents.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Replace an existing generated agent directory.
        #[arg(long)]
        force: bool,
    },
    /// Validate one local agent, or all agents if omitted.
    Validate {
        /// Agent id to validate.
        agent_id: Option<String>,
        /// Agents directory. Defaults to ~/.mesh-llm/agents.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Show one local agent definition.
    Show {
        /// Agent id to show.
        agent_id: String,
        /// Agents directory. Defaults to ~/.mesh-llm/agents.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
    /// Enable a local agent definition.
    Enable {
        /// Agent id to enable.
        agent_id: String,
        /// Agents directory. Defaults to ~/.mesh-llm/agents.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Disable a local agent definition.
    Disable {
        /// Agent id to disable.
        agent_id: String,
        /// Agents directory. Defaults to ~/.mesh-llm/agents.
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum AgentRuntimeArg {
    Opencode,
    Goose,
    Pi,
    Acp,
    Remote,
}

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var_os("MESH_LLM_PLUGIN_ENDPOINT").is_some() {
        return plugin::run_plugin_from_env().await;
    }

    let cli = Cli::parse();
    match cli.command {
        Command::Mcp {
            agents_dir,
            data_dir,
        } => a2a::run_a2a_mcp(agents_dir.as_deref(), data_dir.as_deref()).await,
        Command::Agents { command } => agents::dispatch_agents_command(&command),
    }
}
