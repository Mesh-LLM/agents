mod a2a;
mod agents;
mod codex;
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
    /// Configure Codex for mesh agents.
    Codex {
        #[command(subcommand)]
        command: CodexCommand,
    },
    /// Low-level skill plumbing.
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
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
    Acp,
    Remote,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CodexCommand {
    /// Install the mesh-agents skill and configure the A2A MCP server.
    Setup {
        /// Codex config file. Defaults to ~/.codex/config.toml.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Codex skills directory. Defaults to ~/.codex/skills.
        #[arg(long)]
        skills_dir: Option<PathBuf>,
        /// Command Codex should run for the A2A MCP server.
        #[arg(long, default_value = "agents")]
        mcp_command: String,
        /// Print changes instead of writing files.
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
pub(crate) enum SkillsCommand {
    /// List built-in skills.
    List,
    /// Install a built-in skill.
    Install {
        /// Skill name. Currently only mesh-agents.
        skill: String,
        /// Codex skills directory. Defaults to ~/.codex/skills.
        #[arg(long)]
        codex_dir: Option<PathBuf>,
        /// Replace an existing installed skill.
        #[arg(long)]
        force: bool,
    },
    /// Remove a built-in skill.
    Uninstall {
        /// Skill name. Currently only mesh-agents.
        skill: String,
        /// Codex skills directory. Defaults to ~/.codex/skills.
        #[arg(long)]
        codex_dir: Option<PathBuf>,
    },
    /// Show built-in skill install status.
    Status {
        /// Codex skills directory. Defaults to ~/.codex/skills.
        #[arg(long)]
        codex_dir: Option<PathBuf>,
    },
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
        Command::Codex { command } => codex::dispatch_codex_command(&command),
        Command::Skills { command } => codex::dispatch_skills_command(&command),
    }
}
