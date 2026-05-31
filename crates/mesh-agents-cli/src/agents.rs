use anyhow::{bail, Context, Result};
use mesh_agents_a2a::{AgentDefinition, AgentRegistry};
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{AgentRuntimeArg, AgentsCommand};

const AGENT_CARD_FILE: &str = "agent-card.json";
const RUNTIME_FILE: &str = "runtime.toml";
const INSTRUCTIONS_FILE: &str = "instructions.md";

pub(crate) fn dispatch_agents_command(command: &AgentsCommand) -> Result<()> {
    match command {
        AgentsCommand::List { dir, json } => list_agents(dir.as_deref(), *json),
        AgentsCommand::Init {
            agent_id,
            runtime,
            dir,
            force,
        } => init_agent(agent_id, *runtime, dir.as_deref(), *force),
        AgentsCommand::Validate {
            agent_id,
            dir,
            json,
        } => validate_agents(agent_id.as_deref(), dir.as_deref(), *json),
        AgentsCommand::Show {
            agent_id,
            dir,
            json,
        } => show_agent(agent_id, dir.as_deref(), *json),
        AgentsCommand::Enable { agent_id, dir } => {
            set_agent_enabled(agent_id, dir.as_deref(), true)
        }
        AgentsCommand::Disable { agent_id, dir } => {
            set_agent_enabled(agent_id, dir.as_deref(), false)
        }
    }
}

fn list_agents(dir: Option<&Path>, json: bool) -> Result<()> {
    let registry = load_registry(dir)?;
    if json {
        let rows: Vec<_> = registry.agents().iter().map(agent_json_summary).collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    println!("id\tenabled\tvisibility\truntime\tconcurrency\tname");
    for agent in registry.agents() {
        println!(
            "{}\t{}\t{:?}\t{:?}\t{}\t{}",
            agent.id,
            agent.runtime.enabled,
            agent.runtime.visibility,
            agent.runtime.runtime.kind,
            agent.runtime.runtime.max_concurrent_tasks,
            agent.card.name
        );
    }
    Ok(())
}

fn init_agent(
    agent_id: &str,
    runtime: AgentRuntimeArg,
    dir: Option<&Path>,
    force: bool,
) -> Result<()> {
    validate_agent_id(agent_id)?;
    let root = agents_dir(dir)?;
    let agent_dir = root.join(agent_id);
    if agent_dir.exists() {
        if !force {
            bail!(
                "agent {} already exists at {}; pass --force to replace it",
                agent_id,
                agent_dir.display()
            );
        }
        fs::remove_dir_all(&agent_dir)
            .with_context(|| format!("failed to remove {}", agent_dir.display()))?;
    }

    fs::create_dir_all(&agent_dir)
        .with_context(|| format!("failed to create {}", agent_dir.display()))?;
    write_agent_card(&agent_dir, agent_id)?;
    write_runtime(&agent_dir, runtime)?;
    fs::write(
        agent_dir.join(INSTRUCTIONS_FILE),
        default_instructions(agent_id),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            agent_dir.join(INSTRUCTIONS_FILE).display()
        )
    })?;

    let definition = AgentDefinition::load(agent_dir.clone())?;
    println!("created agent {} at {}", definition.id, agent_dir.display());
    Ok(())
}

fn validate_agents(agent_id: Option<&str>, dir: Option<&Path>, json: bool) -> Result<()> {
    let registry = load_registry(dir)?;
    if let Some(agent_id) = agent_id {
        let agent = require_agent(&registry, agent_id)?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&validation_report([agent]))?
            );
            return Ok(());
        }
        println!("agent {agent_id} is valid");
        return Ok(());
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&validation_report(registry.agents()))?
        );
        return Ok(());
    }

    for agent in registry.agents() {
        println!("agent {} is valid", agent.id);
    }
    if registry.agents().is_empty() {
        println!("no agents found in {}", agents_dir(dir)?.display());
    }
    Ok(())
}

fn show_agent(agent_id: &str, dir: Option<&Path>, json: bool) -> Result<()> {
    let registry = load_registry(dir)?;
    let agent = require_agent(&registry, agent_id)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&agent_json_summary(agent))?
        );
        return Ok(());
    }

    println!("id: {}", agent.id);
    println!("name: {}", agent.card.name);
    println!("description: {}", agent.card.description);
    println!("version: {}", agent.card.version);
    println!("enabled: {}", agent.runtime.enabled);
    println!("visibility: {:?}", agent.runtime.visibility);
    println!("runtime: {:?}", agent.runtime.runtime.kind);
    println!(
        "max_concurrent_tasks: {}",
        agent.runtime.runtime.max_concurrent_tasks
    );
    println!("dir: {}", agent.dir.display());
    println!("agent_card: {}", agent.card_path.display());
    println!("runtime_config: {}", agent.runtime_path.display());
    Ok(())
}

fn set_agent_enabled(agent_id: &str, dir: Option<&Path>, enabled: bool) -> Result<()> {
    let registry = load_registry(dir)?;
    let agent = require_agent(&registry, agent_id)?;
    let mut runtime = agent.runtime.clone();
    runtime.enabled = enabled;
    let raw = toml::to_string_pretty(&runtime)?;
    fs::write(&agent.runtime_path, raw)
        .with_context(|| format!("failed to write {}", agent.runtime_path.display()))?;
    println!(
        "{} agent {agent_id}",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn load_registry(dir: Option<&Path>) -> Result<AgentRegistry> {
    AgentRegistry::load_from_dir(agents_dir(dir)?)
}

fn agents_dir(dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = dir {
        return Ok(dir.to_path_buf());
    }
    Ok(home_dir()?.join(".mesh-llm").join("agents"))
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("could not determine home directory")
}

fn require_agent<'a>(registry: &'a AgentRegistry, agent_id: &str) -> Result<&'a AgentDefinition> {
    registry
        .get(agent_id)
        .ok_or_else(|| anyhow::anyhow!("agent {agent_id} was not found"))
}

fn agent_json_summary(agent: &AgentDefinition) -> serde_json::Value {
    serde_json::json!({
        "id": agent.id,
        "name": agent.card.name,
        "description": agent.card.description,
        "version": agent.card.version,
        "enabled": agent.runtime.enabled,
        "visibility": agent.runtime.visibility,
        "runtime": agent.runtime.runtime.kind,
        "max_concurrent_tasks": agent.runtime.runtime.max_concurrent_tasks,
        "dir": agent.dir,
        "agent_card": agent.card_path,
        "runtime_config": agent.runtime_path,
    })
}

fn validation_report<'a>(
    agents: impl IntoIterator<Item = &'a AgentDefinition>,
) -> AgentValidationReport {
    let agents = agents.into_iter().collect::<Vec<_>>();
    let mut runtimes = BTreeMap::new();
    for agent in &agents {
        *runtimes
            .entry(runtime_label(agent).to_string())
            .or_default() += 1;
    }
    AgentValidationReport {
        status: "ok",
        total: agents.len(),
        enabled: agents.iter().filter(|agent| agent.runtime.enabled).count(),
        public: agents
            .iter()
            .filter(|agent| {
                matches!(
                    &agent.runtime.visibility,
                    mesh_agents_a2a::Visibility::Public
                )
            })
            .count(),
        advertised_on_mesh: agents
            .iter()
            .filter(|agent| agent.runtime.policy.advertise_on_mesh)
            .count(),
        runtimes,
        agents: agents.into_iter().map(agent_validation_row).collect(),
    }
}

fn agent_validation_row(agent: &AgentDefinition) -> AgentValidationRow {
    AgentValidationRow {
        id: agent.id.clone(),
        name: agent.card.name.clone(),
        enabled: agent.runtime.enabled,
        visibility: visibility_label(agent).to_string(),
        runtime: runtime_label(agent).to_string(),
        max_concurrent_tasks: agent.runtime.runtime.max_concurrent_tasks,
        advertise_on_mesh: agent.runtime.policy.advertise_on_mesh,
        public_mesh: agent.runtime.policy.public_mesh,
    }
}

fn runtime_label(agent: &AgentDefinition) -> &'static str {
    match &agent.runtime.runtime.kind {
        mesh_agents_a2a::RuntimeKind::Opencode => "opencode",
        mesh_agents_a2a::RuntimeKind::Goose => "goose",
        mesh_agents_a2a::RuntimeKind::Pi => "pi",
        mesh_agents_a2a::RuntimeKind::Acp => "acp",
        mesh_agents_a2a::RuntimeKind::Remote => "remote",
    }
}

fn visibility_label(agent: &AgentDefinition) -> &'static str {
    match &agent.runtime.visibility {
        mesh_agents_a2a::Visibility::Private => "private",
        mesh_agents_a2a::Visibility::Public => "public",
    }
}

#[derive(Serialize)]
struct AgentValidationReport {
    status: &'static str,
    total: usize,
    enabled: usize,
    public: usize,
    advertised_on_mesh: usize,
    runtimes: BTreeMap<String, usize>,
    agents: Vec<AgentValidationRow>,
}

#[derive(Serialize)]
struct AgentValidationRow {
    id: String,
    name: String,
    enabled: bool,
    visibility: String,
    runtime: String,
    max_concurrent_tasks: usize,
    advertise_on_mesh: bool,
    public_mesh: bool,
}

fn write_agent_card(agent_dir: &Path, agent_id: &str) -> Result<()> {
    let card = serde_json::json!({
        "name": title_from_agent_id(agent_id),
        "description": "Reviews pull requests and returns prioritized findings with file/line references.",
        "version": "0.1.0",
        "supportedInterfaces": [
            {
                "url": format!("http://127.0.0.1:3131/a2a/agents/{agent_id}"),
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0"
            }
        ],
        "capabilities": {
            "streaming": true
        },
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/markdown"],
        "skills": [
            {
                "id": "pr-review",
                "name": "Pull request review",
                "description": "Inspect a GitHub pull request and report correctness, regression, and test risks.",
                "tags": ["github", "code-review", "pull-request"]
            }
        ]
    });
    fs::write(
        agent_dir.join(AGENT_CARD_FILE),
        format!("{}\n", serde_json::to_string_pretty(&card)?),
    )
    .with_context(|| {
        format!(
            "failed to write {}",
            agent_dir.join(AGENT_CARD_FILE).display()
        )
    })
}

fn write_runtime(agent_dir: &Path, runtime: AgentRuntimeArg) -> Result<()> {
    let runtime_type = match runtime {
        AgentRuntimeArg::Opencode => "opencode",
        AgentRuntimeArg::Goose => "goose",
        AgentRuntimeArg::Pi => "pi",
        AgentRuntimeArg::Acp => "acp",
        AgentRuntimeArg::Remote => "remote",
    };
    let runtime_command = match runtime {
        AgentRuntimeArg::Acp => "command = \"agent-command\"\nargs = [\"acp\"]\n",
        AgentRuntimeArg::Remote => "command = \"\"\n",
        AgentRuntimeArg::Opencode | AgentRuntimeArg::Goose | AgentRuntimeArg::Pi => "",
    };
    let raw = format!(
        r#"enabled = true
visibility = "private"

[runtime]
type = "{runtime_type}"
{runtime_command}max_concurrent_tasks = 1

[runtime.workspace]
mode = "temp_per_task"
keep = "on_failure"

[instructions]
file = "instructions.md"
delivery = "first_prompt"

[tools.mesh]
enabled = true

[policy]
advertise_on_mesh = false
public_mesh = false
"#
    );
    fs::write(agent_dir.join(RUNTIME_FILE), raw)
        .with_context(|| format!("failed to write {}", agent_dir.join(RUNTIME_FILE).display()))
}

fn default_instructions(agent_id: &str) -> String {
    format!(
        "You are {agent_id}, a code review agent. Prioritize concrete bugs, regressions, security risks, and missing tests. Lead with findings and include file and line references whenever possible.\n"
    )
}

fn title_from_agent_id(agent_id: &str) -> String {
    agent_id
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ")
}

fn capitalize(part: &str) -> String {
    let mut chars = part.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    format!("{}{}", first.to_uppercase(), chars.as_str())
}

fn validate_agent_id(agent_id: &str) -> Result<()> {
    if agent_id.is_empty() {
        bail!("agent id cannot be empty");
    }
    if !agent_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
    {
        bail!("agent id {agent_id:?} must contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("mesh-llm-agent-cli-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn init_generates_loadable_opencode_agent() {
        let root = temp_root("init");

        init_agent("pr-review", AgentRuntimeArg::Opencode, Some(&root), false).unwrap();

        let registry = AgentRegistry::load_from_dir(&root).unwrap();
        let agent = registry.get("pr-review").unwrap();
        assert_eq!(agent.card.name, "Pr Review");
        assert!(agent.runtime.enabled);
        assert_eq!(agent.runtime.runtime.max_concurrent_tasks, 1);
        assert!(agent.dir.join("instructions.md").is_file());
    }

    #[test]
    fn validation_report_counts_runtime_and_policy() {
        let root = temp_root("report");
        init_agent("pr-review", AgentRuntimeArg::Opencode, Some(&root), false).unwrap();
        init_agent("explicit-acp", AgentRuntimeArg::Acp, Some(&root), false).unwrap();

        let registry = AgentRegistry::load_from_dir(&root).unwrap();
        let report = validation_report(registry.agents());

        assert_eq!(report.status, "ok");
        assert_eq!(report.total, 2);
        assert_eq!(report.enabled, 2);
        assert_eq!(report.runtimes["opencode"], 1);
        assert_eq!(report.runtimes["acp"], 1);
        assert!(report.agents.iter().all(|agent| !agent.advertise_on_mesh));
    }
}
