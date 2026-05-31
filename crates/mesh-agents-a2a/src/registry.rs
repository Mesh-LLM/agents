use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

use crate::AgentCard;

const AGENT_CARD_FILE: &str = "agent-card.json";
const RUNTIME_FILE: &str = "runtime.toml";

#[derive(Clone, Debug, Default)]
pub struct AgentRegistry {
    agents: Vec<AgentDefinition>,
}

impl AgentRegistry {
    #[must_use]
    pub fn empty() -> Self {
        Self { agents: Vec::new() }
    }

    pub fn load_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            return Ok(Self::empty());
        }
        if !path.is_dir() {
            bail!("agents path {} is not a directory", path.display());
        }

        let mut agents = Vec::new();
        for entry in fs::read_dir(path)
            .with_context(|| format!("failed to read agents directory {}", path.display()))?
        {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                agents.push(AgentDefinition::load(entry_path)?);
            } else if entry_path.extension().is_some_and(|ext| ext == "json") {
                bail!(
                    "loose Agent Card files are not supported under {}; use <agent-id>/agent-card.json",
                    path.display()
                );
            }
        }

        agents.sort_by(|left, right| left.id.cmp(&right.id));
        Ok(Self { agents })
    }

    #[must_use]
    pub fn agents(&self) -> &[AgentDefinition] {
        &self.agents
    }

    #[must_use]
    pub fn get(&self, agent_id: &str) -> Option<&AgentDefinition> {
        self.agents.iter().find(|agent| agent.id == agent_id)
    }
}

#[derive(Clone, Debug)]
pub struct AgentDefinition {
    pub id: String,
    pub dir: PathBuf,
    pub card_path: PathBuf,
    pub runtime_path: PathBuf,
    pub card: AgentCard,
    pub runtime: AgentRuntimeConfig,
}

impl AgentDefinition {
    pub fn load(dir: PathBuf) -> Result<Self> {
        let id = dir
            .file_name()
            .and_then(|name| name.to_str())
            .context("agent directory must have a UTF-8 name")?
            .to_string();
        validate_agent_id(&id)?;

        let card_path = dir.join(AGENT_CARD_FILE);
        let runtime_path = dir.join(RUNTIME_FILE);
        if !card_path.is_file() {
            bail!("agent {id} is missing {AGENT_CARD_FILE}");
        }
        if !runtime_path.is_file() {
            bail!("agent {id} is missing {RUNTIME_FILE}");
        }

        let card: AgentCard = read_json(&card_path)?;
        let mut runtime: AgentRuntimeConfig = read_toml(&runtime_path)?;
        runtime.normalize_relative_paths(&dir);
        validate_agent_definition(&id, &card, &runtime)?;

        Ok(Self {
            id,
            dir,
            card_path,
            runtime_path,
            card,
            runtime,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AgentRuntimeConfig {
    pub enabled: bool,
    pub visibility: Visibility,
    pub runtime: RuntimeConfig,
    pub instructions: Option<InstructionsConfig>,
    pub tools: ToolsConfig,
    pub policy: PolicyConfig,
    pub auth: Option<AuthConfig>,
}

impl Default for AgentRuntimeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            visibility: Visibility::Private,
            runtime: RuntimeConfig::default(),
            instructions: None,
            tools: ToolsConfig::default(),
            policy: PolicyConfig::default(),
            auth: None,
        }
    }
}

impl AgentRuntimeConfig {
    fn normalize_relative_paths(&mut self, agent_dir: &Path) {
        self.runtime.workspace.normalize_relative_paths(agent_dir);
        if let Some(instructions) = &mut self.instructions {
            instructions.normalize_relative_paths(agent_dir);
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    #[default]
    Private,
    Public,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    #[serde(rename = "type")]
    pub kind: RuntimeKind,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub session_policy: Option<String>,
    pub max_concurrent_tasks: usize,
    pub queue: QueueConfig,
    pub workspace: WorkspaceConfig,
    pub env: std::collections::BTreeMap<String, String>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            kind: RuntimeKind::Opencode,
            command: None,
            args: Vec::new(),
            model: None,
            mode: None,
            session_policy: Some("per_task".to_string()),
            max_concurrent_tasks: 1,
            queue: QueueConfig::default(),
            workspace: WorkspaceConfig::default(),
            env: std::collections::BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    #[default]
    Opencode,
    Goose,
    Pi,
    Acp,
    Remote,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct QueueConfig {
    pub mode: QueueMode,
    pub max_pending_tasks: usize,
}

impl Default for QueueConfig {
    fn default() -> Self {
        Self {
            mode: QueueMode::Queue,
            max_pending_tasks: 16,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueMode {
    #[default]
    Queue,
    Reject,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceConfig {
    pub mode: WorkspaceMode,
    pub path: Option<PathBuf>,
    pub prefix: Option<String>,
    pub keep: WorkspaceKeep,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            mode: WorkspaceMode::TempPerTask,
            path: None,
            prefix: Some("mesh-a2a-".to_string()),
            keep: WorkspaceKeep::OnFailure,
        }
    }
}

impl WorkspaceConfig {
    fn normalize_relative_paths(&mut self, agent_dir: &Path) {
        if let Some(path) = &mut self.path {
            if path.is_relative() {
                *path = agent_dir.join(&path);
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    Path,
    #[default]
    TempPerTask,
    AgentDir,
    None,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceKeep {
    Never,
    #[default]
    OnFailure,
    Always,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct InstructionsConfig {
    pub file: Option<PathBuf>,
    pub delivery: InstructionDelivery,
}

impl Default for InstructionsConfig {
    fn default() -> Self {
        Self {
            file: None,
            delivery: InstructionDelivery::FirstPrompt,
        }
    }
}

impl InstructionsConfig {
    fn normalize_relative_paths(&mut self, agent_dir: &Path) {
        if let Some(path) = &mut self.file {
            if path.is_relative() {
                *path = agent_dir.join(&path);
            }
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstructionDelivery {
    #[default]
    FirstPrompt,
    TopOfMind,
    None,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsConfig {
    pub mesh: MeshToolsConfig,
    pub extra: Vec<ToolConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MeshToolsConfig {
    pub enabled: bool,
    pub available_tools: Vec<String>,
}

impl Default for MeshToolsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            available_tools: vec![
                "agents.get_agents".to_string(),
                "agents.get_agent".to_string(),
                "agents.send_message".to_string(),
                "agents.get_task".to_string(),
                "agents.view_text_artifact".to_string(),
                "agents.view_data_artifact".to_string(),
            ],
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ToolConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ToolKind,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub timeout_secs: Option<u64>,
    pub env_keys: Vec<String>,
    pub available_tools: Vec<String>,
}

impl Default for ToolConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: ToolKind::Mcp,
            command: None,
            args: Vec::new(),
            timeout_secs: None,
            env_keys: Vec::new(),
            available_tools: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    #[default]
    Mcp,
    HarnessBuiltin,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    pub approval: Option<String>,
    pub filesystem: Option<String>,
    pub network: Option<String>,
    pub max_task_seconds: Option<u64>,
    pub advertise_on_mesh: bool,
    pub public_mesh: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    pub bearer_token_env: Option<String>,
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read JSON file {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn read_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read TOML file {}", path.display()))?;
    toml::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
}

fn validate_agent_definition(
    agent_id: &str,
    card: &AgentCard,
    runtime: &AgentRuntimeConfig,
) -> Result<()> {
    validate_agent_card(agent_id, card)?;
    validate_runtime_config(agent_id, runtime)
}

fn validate_agent_card(agent_id: &str, card: &AgentCard) -> Result<()> {
    validate_required(&card.name, &format!("agent {agent_id} card.name"))?;
    validate_required(
        &card.description,
        &format!("agent {agent_id} card.description"),
    )?;
    validate_required(&card.version, &format!("agent {agent_id} card.version"))?;
    validate_agent_interfaces(agent_id, &card.supported_interfaces)?;
    validate_string_list(
        &card.default_input_modes,
        &format!("agent {agent_id} card.defaultInputModes"),
    )?;
    validate_string_list(
        &card.default_output_modes,
        &format!("agent {agent_id} card.defaultOutputModes"),
    )?;
    for (index, skill) in card.skills.iter().enumerate() {
        let path = format!("agent {agent_id} card.skills[{index}]");
        validate_required(&skill.id, &format!("{path}.id"))?;
        validate_required(&skill.name, &format!("{path}.name"))?;
        validate_required(&skill.description, &format!("{path}.description"))?;
        validate_string_list(&skill.tags, &format!("{path}.tags"))?;
    }
    Ok(())
}

fn validate_agent_interfaces(agent_id: &str, interfaces: &[a2a::AgentInterface]) -> Result<()> {
    if interfaces.is_empty() {
        bail!("agent {agent_id} card.supportedInterfaces must not be empty");
    }
    for (index, interface) in interfaces.iter().enumerate() {
        let path = format!("agent {agent_id} card.supportedInterfaces[{index}]");
        validate_required(&interface.url, &format!("{path}.url"))?;
        validate_required(
            &interface.protocol_binding,
            &format!("{path}.protocolBinding"),
        )?;
        validate_required(
            &interface.protocol_version,
            &format!("{path}.protocolVersion"),
        )?;
    }
    Ok(())
}

fn validate_runtime_config(agent_id: &str, config: &AgentRuntimeConfig) -> Result<()> {
    validate_runtime_command(agent_id, &config.runtime)?;
    validate_workspace(agent_id, &config.runtime.workspace)?;
    validate_instructions(agent_id, config.instructions.as_ref())?;
    validate_tools(agent_id, &config.tools)?;
    if let Some(auth) = &config.auth {
        validate_optional_non_empty(
            auth.bearer_token_env.as_deref(),
            &format!("agent {agent_id} auth.bearer_token_env"),
        )?;
    }
    if config.runtime.max_concurrent_tasks == 0 {
        bail!("agent {agent_id} runtime.max_concurrent_tasks must be at least 1");
    }
    Ok(())
}

fn validate_runtime_command(agent_id: &str, runtime: &RuntimeConfig) -> Result<()> {
    match runtime.kind {
        RuntimeKind::Acp => {
            validate_required_option(
                runtime.command.as_deref(),
                &format!("agent {agent_id} runtime.command"),
            )?;
        }
        RuntimeKind::Opencode | RuntimeKind::Goose | RuntimeKind::Pi | RuntimeKind::Remote => {
            validate_optional_non_empty(
                runtime.command.as_deref(),
                &format!("agent {agent_id} runtime.command"),
            )?;
        }
    }
    validate_string_list(&runtime.args, &format!("agent {agent_id} runtime.args"))?;
    for (key, value) in &runtime.env {
        validate_required(key, &format!("agent {agent_id} runtime.env key"))?;
        validate_required(value, &format!("agent {agent_id} runtime.env.{key}"))?;
    }
    Ok(())
}

fn validate_workspace(agent_id: &str, workspace: &WorkspaceConfig) -> Result<()> {
    if matches!(workspace.mode, WorkspaceMode::Path) {
        let path = workspace
            .path
            .as_ref()
            .filter(|path| !path.as_os_str().is_empty())
            .with_context(|| format!("agent {agent_id} runtime.workspace.path is required"))?;
        if path.as_os_str().is_empty() {
            bail!("agent {agent_id} runtime.workspace.path must not be empty");
        }
    }
    validate_optional_non_empty(
        workspace.prefix.as_deref(),
        &format!("agent {agent_id} runtime.workspace.prefix"),
    )
}

fn validate_instructions(agent_id: &str, instructions: Option<&InstructionsConfig>) -> Result<()> {
    let Some(instructions) = instructions else {
        return Ok(());
    };
    let Some(path) = &instructions.file else {
        return Ok(());
    };
    if path.as_os_str().is_empty() {
        bail!("agent {agent_id} instructions.file must not be empty");
    }
    if !path.is_file() {
        bail!(
            "agent {agent_id} instructions.file {} does not exist",
            path.display()
        );
    }
    Ok(())
}

fn validate_tools(agent_id: &str, tools: &ToolsConfig) -> Result<()> {
    validate_string_list(
        &tools.mesh.available_tools,
        &format!("agent {agent_id} tools.mesh.available_tools"),
    )?;
    for (index, tool) in tools.extra.iter().enumerate() {
        let path = format!("agent {agent_id} tools.extra[{index}]");
        validate_required(&tool.name, &format!("{path}.name"))?;
        validate_optional_non_empty(tool.command.as_deref(), &format!("{path}.command"))?;
        validate_string_list(&tool.args, &format!("{path}.args"))?;
        validate_string_list(&tool.env_keys, &format!("{path}.env_keys"))?;
        validate_string_list(&tool.available_tools, &format!("{path}.available_tools"))?;
    }
    Ok(())
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

fn validate_required(value: &str, path: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("{path} must not be empty");
    }
    Ok(())
}

fn validate_required_option(value: Option<&str>, path: &str) -> Result<()> {
    match value {
        Some(value) => validate_required(value, path),
        None => bail!("{path} is required"),
    }
}

fn validate_optional_non_empty(value: Option<&str>, path: &str) -> Result<()> {
    if let Some(value) = value {
        validate_required(value, path)?;
    }
    Ok(())
}

fn validate_string_list(values: &[String], path: &str) -> Result<()> {
    for (index, value) in values.iter().enumerate() {
        validate_required(value, &format!("{path}[{index}]"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("mesh-agents-a2a-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp registry directory");
        root
    }

    fn write_minimal_agent(root: &Path, id: &str) -> PathBuf {
        let agent_dir = root.join(id);
        fs::create_dir_all(&agent_dir).expect("create test agent directory");
        fs::write(
            agent_dir.join(AGENT_CARD_FILE),
            r#"{
              "name": "Test Agent",
              "description": "A test agent.",
              "version": "1.0.0",
              "supportedInterfaces": [
                {
                  "url": "http://localhost:3131/a2a/agents/test",
                  "protocolBinding": "JSONRPC",
                  "protocolVersion": "1.0"
                }
              ],
              "capabilities": { "streaming": true },
              "defaultInputModes": ["text/plain"],
              "defaultOutputModes": ["text/markdown"],
              "skills": []
            }"#,
        )
        .expect("write test agent card");
        fs::write(
            agent_dir.join(RUNTIME_FILE),
            r#"
enabled = true
visibility = "private"

[runtime]
type = "opencode"
max_concurrent_tasks = 2

[runtime.workspace]
mode = "path"
path = "work"

[tools.mesh]
enabled = true

[[tools.extra]]
name = "github"
type = "mcp"
command = "github-mcp-server"
"#,
        )
        .expect("write test runtime config");
        agent_dir
    }

    fn write_agent_without_instructions(root: &Path, id: &str, runtime: &str) -> PathBuf {
        let agent_dir = root.join(id);
        fs::create_dir_all(&agent_dir).expect("create test agent directory");
        fs::write(
            agent_dir.join(AGENT_CARD_FILE),
            r#"{
              "name": "Test Agent",
              "description": "A test agent.",
              "version": "1.0.0",
              "supportedInterfaces": [
                {
                  "url": "http://localhost:3131/a2a/agents/test",
                  "protocolBinding": "JSONRPC",
                  "protocolVersion": "1.0"
                }
              ],
              "capabilities": { "streaming": true },
              "defaultInputModes": ["text/plain"],
              "defaultOutputModes": ["text/markdown"],
              "skills": []
            }"#,
        )
        .expect("write test agent card");
        fs::write(agent_dir.join(RUNTIME_FILE), runtime).expect("write test runtime config");
        agent_dir
    }

    #[test]
    fn loads_directory_agents() {
        let root = temp_root("loads");
        let agent_dir = write_minimal_agent(&root, "pr-review");

        let registry = AgentRegistry::load_from_dir(&root).expect("load test agent registry");

        assert_eq!(registry.agents().len(), 1);
        let agent = registry.get("pr-review").expect("pr-review agent exists");
        assert_eq!(agent.id, "pr-review");
        assert_eq!(agent.runtime.runtime.max_concurrent_tasks, 2);
        assert_eq!(
            agent
                .runtime
                .runtime
                .workspace
                .path
                .as_ref()
                .expect("workspace path should be normalized"),
            &agent_dir.join("work")
        );
        assert_eq!(agent.runtime.tools.extra[0].name, "github");
    }

    #[test]
    fn rejects_loose_json_cards() {
        let root = temp_root("loose");
        fs::write(root.join("agent-card.json"), "{}").expect("write loose agent card");

        let error = AgentRegistry::load_from_dir(&root)
            .expect_err("loose card should be rejected")
            .to_string();

        assert!(error.contains("loose Agent Card files are not supported"));
    }

    #[test]
    fn rejects_acp_runtime_without_command() {
        let root = temp_root("acp-command");
        write_agent_without_instructions(
            &root,
            "pr-review",
            r#"
enabled = true
visibility = "private"

[runtime]
type = "acp"
max_concurrent_tasks = 1

[runtime.workspace]
mode = "temp_per_task"
"#,
        );

        let error = AgentRegistry::load_from_dir(&root)
            .expect_err("ACP runtime without command should be rejected")
            .to_string();

        assert!(error.contains("runtime.command is required"), "{error}");
    }

    #[test]
    fn rejects_path_workspace_without_path() {
        let root = temp_root("workspace-path");
        write_agent_without_instructions(
            &root,
            "pr-review",
            r#"
enabled = true
visibility = "private"

[runtime]
type = "opencode"
max_concurrent_tasks = 1

[runtime.workspace]
mode = "path"
"#,
        );

        let error = AgentRegistry::load_from_dir(&root)
            .expect_err("path workspace without path should be rejected")
            .to_string();

        assert!(
            error.contains("runtime.workspace.path is required"),
            "{error}"
        );
    }

    #[test]
    fn rejects_missing_instruction_file() {
        let root = temp_root("instructions");
        write_agent_without_instructions(
            &root,
            "pr-review",
            r#"
enabled = true
visibility = "private"

[runtime]
type = "opencode"
max_concurrent_tasks = 1

[runtime.workspace]
mode = "temp_per_task"

[instructions]
file = "missing.md"
"#,
        );

        let error = AgentRegistry::load_from_dir(&root)
            .expect_err("missing instructions file should be rejected")
            .to_string();

        assert!(error.contains("instructions.file"), "{error}");
        assert!(error.contains("missing.md"), "{error}");
    }

    #[test]
    fn rejects_empty_card_interfaces() {
        let root = temp_root("card-interfaces");
        let agent_dir = root.join("pr-review");
        fs::create_dir_all(&agent_dir).expect("create test agent directory");
        fs::write(
            agent_dir.join(AGENT_CARD_FILE),
            r#"{
              "name": "Test Agent",
              "description": "A test agent.",
              "version": "1.0.0",
              "supportedInterfaces": [],
              "capabilities": { "streaming": true },
              "defaultInputModes": ["text/plain"],
              "defaultOutputModes": ["text/markdown"],
              "skills": []
            }"#,
        )
        .expect("write invalid card");
        fs::write(
            agent_dir.join(RUNTIME_FILE),
            r#"
enabled = true
visibility = "private"

[runtime]
type = "opencode"
max_concurrent_tasks = 1
"#,
        )
        .expect("write runtime config");

        let error = AgentRegistry::load_from_dir(&root)
            .expect_err("card without interfaces should be rejected")
            .to_string();

        assert!(error.contains("supportedInterfaces"), "{error}");
    }
}
