//! ACP harness bridge primitives for mesh-llm A2A agents.
//!
//! The bridge uses the official `agent-client-protocol` crate for ACP protocol
//! boundaries and the official A2A server executor trait from `a2a-server-lf`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use futures::{stream, stream::BoxStream};
use mesh_agents_a2a::{
    A2AError, AgentDefinition, AgentExecutor, Artifact, InstructionDelivery, Message, Part, Role,
    RuntimeConfig, RuntimeKind, StreamResponse, Task, TaskState, TaskStatus, WorkspaceMode,
};
use serde_json::{json, Value};

pub use agent_client_protocol::{AcpAgent, Client, ConnectTo, Stdio};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcpCommand {
    pub command: String,
    pub args: Vec<String>,
}

impl AcpCommand {
    pub fn from_runtime(runtime: &RuntimeConfig) -> Result<Self> {
        match runtime.kind {
            RuntimeKind::Opencode => opencode_command(runtime),
            RuntimeKind::Goose => goose_command(runtime),
            RuntimeKind::Pi => pi_command(runtime),
            RuntimeKind::Acp => {
                let Some(command) = runtime.command.clone() else {
                    bail!("runtime.type = \"acp\" requires command");
                };
                Ok(Self {
                    command,
                    args: runtime.args.clone(),
                })
            }
            RuntimeKind::Remote => bail!("remote A2A agents do not use ACP commands"),
        }
    }
}

fn opencode_command(runtime: &RuntimeConfig) -> Result<AcpCommand> {
    if let Some(command) = runtime.command.clone() {
        let args = if runtime.args.is_empty() {
            default_opencode_args()
        } else {
            runtime.args.clone()
        };
        return Ok(AcpCommand { command, args });
    }

    if executable_on_path("opencode") {
        return Ok(AcpCommand {
            command: "opencode".to_string(),
            args: default_opencode_args(),
        });
    }

    bail!("could not find OpenCode CLI `opencode` on PATH")
}

fn default_opencode_args() -> Vec<String> {
    vec!["acp".to_string()]
}

fn goose_command(runtime: &RuntimeConfig) -> Result<AcpCommand> {
    if let Some(command) = runtime.command.clone() {
        let args = if runtime.args.is_empty() {
            default_goose_args(&command)
        } else {
            runtime.args.clone()
        };
        return Ok(AcpCommand { command, args });
    }

    if executable_on_path("goose") {
        return Ok(AcpCommand {
            command: "goose".to_string(),
            args: vec!["acp".to_string()],
        });
    }

    let app_goosed = Path::new("/Applications/Goose.app/Contents/Resources/bin/goosed");
    if app_goosed.is_file() {
        bail!(
            "Desktop Goose is installed at {}, but goosed agent is not ACP stdio; install or link the Goose CLI as `goose`, or set runtime.command to an ACP-compatible Goose command",
            app_goosed.display()
        );
    }

    bail!("could not find Goose CLI `goose` on PATH")
}

fn default_goose_args(_command: &str) -> Vec<String> {
    vec!["acp".to_string()]
}

fn pi_command(runtime: &RuntimeConfig) -> Result<AcpCommand> {
    if let Some(command) = runtime.command.clone() {
        let args = if runtime.args.is_empty() {
            default_pi_args()
        } else {
            runtime.args.clone()
        };
        return Ok(AcpCommand { command, args });
    }

    if executable_on_path("pi") {
        return Ok(AcpCommand {
            command: "pi".to_string(),
            args: default_pi_args(),
        });
    }

    bail!("could not find Pi CLI `pi` on PATH")
}

fn default_pi_args() -> Vec<String> {
    vec!["acp".to_string()]
}

fn executable_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(name).is_file()))
}

#[derive(Clone, Debug)]
pub struct AcpAgentExecutor {
    agent: AgentDefinition,
    data_dir: PathBuf,
}

impl AcpAgentExecutor {
    #[must_use]
    pub fn new(agent: AgentDefinition, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            agent,
            data_dir: data_dir.into(),
        }
    }
}

impl AgentExecutor for AcpAgentExecutor {
    fn execute(
        &self,
        ctx: mesh_agents_a2a::ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let agent = self.agent.clone();
        let data_dir = self.data_dir.clone();
        Box::pin(stream::once(async move {
            let task = run_task(agent, data_dir, ctx).await;
            Ok(StreamResponse::Task(task))
        }))
    }

    fn cancel(
        &self,
        ctx: mesh_agents_a2a::ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let task = Task {
            id: ctx.task_id,
            context_id: ctx.context_id,
            status: TaskStatus {
                state: TaskState::Canceled,
                message: None,
                timestamp: None,
            },
            artifacts: None,
            history: ctx.stored_task.and_then(|task| task.history),
            metadata: None,
        };
        Box::pin(stream::once(async move { Ok(StreamResponse::Task(task)) }))
    }
}

async fn run_task(
    agent: AgentDefinition,
    data_dir: PathBuf,
    ctx: mesh_agents_a2a::ExecutorContext,
) -> Task {
    let prompt = match task_prompt(&agent, ctx.message.as_ref()) {
        Ok(prompt) => prompt,
        Err(err) => return failed_task(ctx, err.to_string()),
    };
    let workspace = match task_workspace(&agent, &ctx.task_id) {
        Ok(workspace) => workspace,
        Err(err) => return failed_task(ctx, err.to_string()),
    };
    let task_paths = match task_runtime_paths(&data_dir, &agent.id, &ctx.task_id) {
        Ok(paths) => paths,
        Err(err) => return failed_task(ctx, err.to_string()),
    };
    if let Err(err) = std::fs::write(&task_paths.prompt_path, &prompt).with_context(|| {
        format!(
            "failed to write task prompt to {}",
            task_paths.prompt_path.display()
        )
    }) {
        return failed_task(ctx, err.to_string());
    }
    match run_acp_once(&agent, &workspace, &task_paths, prompt).await {
        Ok(output) => match completed_task_from_paths(ctx, output, &task_paths) {
            Ok(task) => task,
            Err(err) => failed_task(err.ctx, err.message),
        },
        Err(err) => failed_task(ctx, err.to_string()),
    }
}

async fn run_acp_once(
    agent: &AgentDefinition,
    workspace: &Path,
    task_paths: &TaskRuntimePaths,
    prompt: String,
) -> Result<String> {
    let agent_process = AcpAgent::from_args(acp_process_args(agent, workspace, task_paths)?)?;

    Client
        .builder()
        .name("mesh-agents-a2a")
        .connect_with(agent_process, async |connection| {
            connection
                .send_request(agent_client_protocol::schema::InitializeRequest::new(
                    agent_client_protocol::schema::ProtocolVersion::V1,
                ))
                .block_task()
                .await?;

            connection
                .build_session(workspace)
                .block_task()
                .run_until(async |mut session| {
                    session.send_prompt(prompt)?;
                    session.read_to_string().await
                })
                .await
        })
        .await
        .map_err(Into::into)
}

#[cfg(test)]
async fn initialize_acp_session_once(runtime: &RuntimeConfig, workspace: &Path) -> Result<()> {
    let test_agent = test_agent_for_runtime(runtime.clone());
    let task_paths = test_task_runtime_paths();
    let agent = AcpAgent::from_args(acp_process_args(&test_agent, workspace, &task_paths)?)?;

    Client
        .builder()
        .name("mesh-agents-a2a")
        .connect_with(agent, async |connection| {
            connection
                .send_request(agent_client_protocol::schema::InitializeRequest::new(
                    agent_client_protocol::schema::ProtocolVersion::V1,
                ))
                .block_task()
                .await?;

            connection
                .build_session(workspace)
                .block_task()
                .run_until(async |_session| Ok(()))
                .await
        })
        .await
        .map_err(Into::into)
}

#[cfg(test)]
fn test_agent_for_runtime(runtime: RuntimeConfig) -> AgentDefinition {
    AgentDefinition {
        id: "pr-review".to_string(),
        dir: PathBuf::from("/tmp/pr-review"),
        card_path: PathBuf::from("/tmp/pr-review/agent-card.json"),
        runtime_path: PathBuf::from("/tmp/pr-review/runtime.toml"),
        card: serde_json::from_value(serde_json::json!({
            "name": "PR Review",
            "description": "Reviews pull requests.",
            "version": "1.0.0",
            "supportedInterfaces": [],
            "capabilities": {},
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/markdown"],
            "skills": []
        }))
        .expect("test card is valid"),
        runtime: mesh_agents_a2a::AgentRuntimeConfig {
            runtime,
            ..mesh_agents_a2a::AgentRuntimeConfig::default()
        },
    }
}

#[cfg(test)]
fn test_task_runtime_paths() -> TaskRuntimePaths {
    TaskRuntimePaths {
        task_id: "task-1".to_string(),
        data_dir: PathBuf::from("/tmp/mesh-data"),
        prompt_path: PathBuf::from("/tmp/mesh-data/a2a/agents/pr-review/runtime/task-1/prompt.txt"),
        artifacts_dir: PathBuf::from(
            "/tmp/mesh-data/a2a/agents/pr-review/runtime/task-1/artifacts",
        ),
        logs_dir: PathBuf::from("/tmp/mesh-data/a2a/agents/pr-review/runtime/task-1/logs"),
    }
}

fn acp_process_args(
    agent: &AgentDefinition,
    workspace: &Path,
    task_paths: &TaskRuntimePaths,
) -> Result<Vec<String>> {
    let runtime = &agent.runtime.runtime;
    let command = AcpCommand::from_runtime(runtime)?;
    let context = TemplateContext::new(agent, workspace, task_paths);
    let mut args = runtime
        .env
        .iter()
        .map(|(key, value)| Ok(format!("{key}={}", context.expand(value)?)))
        .collect::<Result<Vec<_>>>()?;
    args.push(context.expand(&command.command)?);
    args.extend(
        command
            .args
            .iter()
            .map(|arg| context.expand(arg))
            .collect::<Result<Vec<_>>>()?,
    );
    Ok(args)
}

#[derive(Clone, Debug)]
struct TaskRuntimePaths {
    task_id: String,
    data_dir: PathBuf,
    prompt_path: PathBuf,
    artifacts_dir: PathBuf,
    logs_dir: PathBuf,
}

fn task_runtime_paths(
    data_dir: impl AsRef<Path>,
    agent_id: &str,
    task_id: &str,
) -> Result<TaskRuntimePaths> {
    let root = data_dir
        .as_ref()
        .join("a2a")
        .join("agents")
        .join(sanitize_path_component(agent_id))
        .join("runtime")
        .join(sanitize_path_component(task_id));
    let artifacts_dir = root.join("artifacts");
    let logs_dir = root.join("logs");
    std::fs::create_dir_all(&artifacts_dir)
        .with_context(|| format!("failed to create {}", artifacts_dir.display()))?;
    std::fs::create_dir_all(&logs_dir)
        .with_context(|| format!("failed to create {}", logs_dir.display()))?;
    Ok(TaskRuntimePaths {
        task_id: task_id.to_string(),
        data_dir: data_dir.as_ref().to_path_buf(),
        prompt_path: root.join("prompt.txt"),
        artifacts_dir,
        logs_dir,
    })
}

struct TemplateContext {
    values: HashMap<&'static str, String>,
}

impl TemplateContext {
    fn new(agent: &AgentDefinition, workspace: &Path, task_paths: &TaskRuntimePaths) -> Self {
        let runtime = &agent.runtime.runtime;
        let instructions_file = agent
            .runtime
            .instructions
            .as_ref()
            .and_then(|instructions| instructions.file.as_ref())
            .map(path_to_string);
        let instructions_dir = agent
            .runtime
            .instructions
            .as_ref()
            .and_then(|instructions| instructions.file.as_ref())
            .and_then(|path| path.parent())
            .map(path_to_string);
        let mut values = HashMap::from([
            ("agent.id", agent.id.clone()),
            ("agent.name", agent.card.name.clone()),
            ("agent.dir", path_to_string(&agent.dir)),
            ("agent.card_path", path_to_string(&agent.card_path)),
            ("agent.runtime_path", path_to_string(&agent.runtime_path)),
            ("task.id", task_paths.task_id.clone()),
            ("task.workspace", path_to_string(workspace)),
            ("task.prompt_path", path_to_string(&task_paths.prompt_path)),
            (
                "task.artifacts_dir",
                path_to_string(&task_paths.artifacts_dir),
            ),
            ("task.logs_dir", path_to_string(&task_paths.logs_dir)),
            (
                "mesh.mcp_url",
                mesh_url("MESH_LLM_MCP_URL", "http://127.0.0.1:3131/mcp"),
            ),
            (
                "mesh.api_url",
                mesh_url("MESH_LLM_API_URL", "http://127.0.0.1:3131"),
            ),
            (
                "mesh.openai_url",
                mesh_url("MESH_LLM_OPENAI_URL", "http://127.0.0.1:9337/v1"),
            ),
            (
                "mesh.model",
                runtime.model.clone().unwrap_or_else(|| "auto".to_string()),
            ),
            ("mesh.data_dir", path_to_string(&task_paths.data_dir)),
        ]);
        if let Some(value) = instructions_file {
            values.insert("instructions.file", value);
        }
        if let Some(value) = instructions_dir {
            values.insert("instructions.dir", value);
        }
        Self { values }
    }

    fn expand(&self, value: &str) -> Result<String> {
        let expanded = self.expand_mesh_templates(value)?;
        expand_env_vars(&expanded)
    }

    fn expand_mesh_templates(&self, value: &str) -> Result<String> {
        let mut output = String::with_capacity(value.len());
        let mut rest = value;
        while let Some(start) = rest.find("{{") {
            output.push_str(&rest[..start]);
            let after_start = &rest[start + 2..];
            let Some(end) = after_start.find("}}") else {
                bail!("unterminated template variable in `{value}`");
            };
            let key = after_start[..end].trim();
            let replacement = self.template_value(key, value)?;
            output.push_str(&replacement);
            rest = &after_start[end + 2..];
        }
        output.push_str(rest);
        Ok(output)
    }

    fn template_value(&self, key: &str, source: &str) -> Result<String> {
        if let Some(name) = key.strip_prefix("env.") {
            return std::env::var(name).with_context(|| {
                format!("environment variable `{name}` used in `{source}` is not set")
            });
        }
        self.values
            .get(key)
            .cloned()
            .with_context(|| format!("unknown template variable `{{{{ {key} }}}}` in `{source}`"))
    }
}

fn path_to_string(path: impl AsRef<Path>) -> String {
    path.as_ref().display().to_string()
}

fn mesh_url(env_name: &str, default: &str) -> String {
    std::env::var(env_name).unwrap_or_else(|_| default.to_string())
}

fn expand_env_vars(value: &str) -> Result<String> {
    let mut output = String::with_capacity(value.len());
    let chars = value.char_indices().collect::<Vec<_>>();
    let mut index = 0;
    while index < chars.len() {
        let (byte_index, ch) = chars[index];
        if ch != '$' {
            output.push(ch);
            index += 1;
            continue;
        }
        let Some((_, next)) = chars.get(index + 1).copied() else {
            output.push('$');
            index += 1;
            continue;
        };
        if next == '{' {
            let name_start = index + 2;
            let Some(end_index) = chars[name_start..]
                .iter()
                .position(|(_, candidate)| *candidate == '}')
                .map(|position| name_start + position)
            else {
                bail!("unterminated environment variable in `{value}`");
            };
            let start_byte = chars[name_start].0;
            let end_byte = chars[end_index].0;
            let name = &value[start_byte..end_byte];
            output.push_str(&env_value(name, value)?);
            index = end_index + 1;
            continue;
        }
        if is_env_name_start(next) {
            let name_start = index + 1;
            let mut name_end = name_start + 1;
            while let Some((_, candidate)) = chars.get(name_end).copied() {
                if !is_env_name_continue(candidate) {
                    break;
                }
                name_end += 1;
            }
            let start_byte = chars[name_start].0;
            let end_byte = chars
                .get(name_end)
                .map(|(position, _)| *position)
                .unwrap_or(value.len());
            let name = &value[start_byte..end_byte];
            output.push_str(&env_value(name, value)?);
            index = name_end;
            continue;
        }
        output.push_str(&value[byte_index..chars[index + 1].0]);
        index += 2;
    }
    Ok(output)
}

fn is_env_name_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_env_name_continue(ch: char) -> bool {
    is_env_name_start(ch) || ch.is_ascii_digit()
}

fn env_value(name: &str, source: &str) -> Result<String> {
    if name.is_empty() {
        bail!("empty environment variable reference in `{source}`");
    }
    std::env::var(name)
        .with_context(|| format!("environment variable `{name}` used in `{source}` is not set"))
}

fn task_prompt(agent: &AgentDefinition, message: Option<&Message>) -> Result<String> {
    let user_prompt = message.and_then(Message::text).unwrap_or_default();
    let Some(instructions) = &agent.runtime.instructions else {
        return Ok(user_prompt.to_string());
    };
    if instructions.delivery != InstructionDelivery::FirstPrompt {
        return Ok(user_prompt.to_string());
    }
    let Some(path) = &instructions.file else {
        return Ok(user_prompt.to_string());
    };
    let instructions = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read instructions from {}", path.display()))?;
    Ok(format!("{instructions}\n\n---\n\n{user_prompt}"))
}

fn task_workspace(agent: &AgentDefinition, task_id: &str) -> Result<PathBuf> {
    let workspace = &agent.runtime.runtime.workspace;
    let path = match workspace.mode {
        WorkspaceMode::Path => workspace
            .path
            .clone()
            .context("workspace.mode = \"path\" requires workspace.path")?,
        WorkspaceMode::TempPerTask => {
            let prefix = workspace.prefix.as_deref().unwrap_or("mesh-a2a-");
            std::env::temp_dir().join(format!(
                "{prefix}{}-{}",
                sanitize_path_component(&agent.id),
                sanitize_path_component(task_id)
            ))
        }
        WorkspaceMode::AgentDir | WorkspaceMode::None => agent.dir.clone(),
    };
    std::fs::create_dir_all(&path)
        .with_context(|| format!("failed to create workspace {}", path.display()))?;
    Ok(path)
}

fn sanitize_path_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn completed_task_from_paths(
    ctx: mesh_agents_a2a::ExecutorContext,
    output: String,
    task_paths: &TaskRuntimePaths,
) -> std::result::Result<Task, TaskBuildError> {
    let artifacts = match task_artifacts(task_paths, &output) {
        Ok(artifacts) => artifacts,
        Err(error) => {
            return Err(TaskBuildError {
                ctx,
                message: error.to_string(),
            });
        }
    };
    Ok(completed_task(ctx, output, artifacts))
}

fn completed_task(
    ctx: mesh_agents_a2a::ExecutorContext,
    output: String,
    artifacts: Vec<Artifact>,
) -> Task {
    let response = Message::new(Role::Agent, vec![Part::text(output)]);
    Task {
        id: ctx.task_id,
        context_id: ctx.context_id,
        status: TaskStatus {
            state: TaskState::Completed,
            message: Some(response),
            timestamp: None,
        },
        artifacts: Some(artifacts),
        history: ctx.stored_task.and_then(|task| task.history),
        metadata: Some(executor_metadata()),
    }
}

struct TaskBuildError {
    ctx: mesh_agents_a2a::ExecutorContext,
    message: String,
}

impl std::fmt::Debug for TaskBuildError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TaskBuildError")
            .field("message", &self.message)
            .finish_non_exhaustive()
    }
}

fn task_artifacts(task_paths: &TaskRuntimePaths, output: &str) -> Result<Vec<Artifact>> {
    let mut artifacts = artifacts_from_dir(&task_paths.artifacts_dir)?;
    if !artifacts
        .iter()
        .any(|artifact| artifact.artifact_id == "summary.md")
    {
        artifacts.insert(0, text_artifact("summary.md", output.to_string()));
    }
    if !artifacts
        .iter()
        .any(|artifact| artifact.artifact_id == "findings.json")
    {
        artifacts.push(data_artifact(
            "findings.json",
            fallback_findings_json(output),
        ));
    }
    Ok(artifacts)
}

fn artifacts_from_dir(path: &Path) -> Result<Vec<Artifact>> {
    let mut entries = std::fs::read_dir(path)
        .with_context(|| format!("failed to read artifacts directory {}", path.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    entries.sort();

    entries
        .into_iter()
        .map(|path| artifact_from_file(&path))
        .collect()
}

fn artifact_from_file(path: &Path) -> Result<Artifact> {
    let artifact_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("artifact file name must be UTF-8")?
        .to_string();
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read artifact {}", path.display()))?;
    if path
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        let value = serde_json::from_str::<Value>(&raw)
            .with_context(|| format!("failed to parse JSON artifact {}", path.display()))?;
        return Ok(data_artifact(artifact_id, value));
    }
    Ok(text_artifact(artifact_id, raw))
}

fn text_artifact(artifact_id: impl Into<String>, text: String) -> Artifact {
    let artifact_id = artifact_id.into();
    Artifact {
        artifact_id: artifact_id.clone(),
        name: Some(artifact_id.clone()),
        description: Some("Text artifact produced by the ACP harness.".to_string()),
        parts: vec![Part::text(text).with_media_type(media_type_for(&artifact_id))],
        metadata: Some(HashMap::from([(
            "path_hint".to_string(),
            Value::String(artifact_id),
        )])),
        extensions: None,
    }
}

fn data_artifact(artifact_id: impl Into<String>, value: Value) -> Artifact {
    let artifact_id = artifact_id.into();
    Artifact {
        artifact_id: artifact_id.clone(),
        name: Some(artifact_id.clone()),
        description: Some("Structured artifact produced by the ACP harness.".to_string()),
        parts: vec![Part::data(value).with_media_type("application/json")],
        metadata: Some(HashMap::from([(
            "path_hint".to_string(),
            Value::String(artifact_id),
        )])),
        extensions: None,
    }
}

fn fallback_findings_json(output: &str) -> Value {
    json!({
        "schema_version": 1,
        "target": null,
        "status": "completed",
        "summary": output.trim(),
        "findings": [],
        "residual_risk": "The ACP harness did not write findings.json; review the summary.md artifact for details.",
    })
}

fn media_type_for(path: &str) -> &'static str {
    if path.ends_with(".md") {
        "text/markdown"
    } else if path.ends_with(".json") {
        "application/json"
    } else {
        "text/plain"
    }
}

fn failed_task(ctx: mesh_agents_a2a::ExecutorContext, error: String) -> Task {
    let response = Message::new(Role::Agent, vec![Part::text(error)]);
    Task {
        id: ctx.task_id,
        context_id: ctx.context_id,
        status: TaskStatus {
            state: TaskState::Failed,
            message: Some(response),
            timestamp: None,
        },
        artifacts: None,
        history: ctx.stored_task.and_then(|task| task.history),
        metadata: Some(executor_metadata()),
    }
}

fn executor_metadata() -> HashMap<String, serde_json::Value> {
    HashMap::from([("mesh_llm_executor".to_string(), json!("acp"))])
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeTaskEvent {
    StatusChanged {
        task_id: String,
        status: String,
    },
    MessageDelta {
        task_id: String,
        text: String,
    },
    ArtifactCreated {
        task_id: String,
        artifact_id: String,
    },
    InputRequired {
        task_id: String,
        prompt: String,
    },
    Completed {
        task_id: String,
    },
    Failed {
        task_id: String,
        error: String,
    },
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use mesh_agents_a2a::{
        AgentRuntimeConfig, InstructionsConfig, QueueConfig, WorkspaceConfig, WorkspaceKeep,
    };

    fn runtime(kind: RuntimeKind) -> RuntimeConfig {
        RuntimeConfig {
            kind,
            command: None,
            args: Vec::new(),
            model: None,
            mode: None,
            session_policy: None,
            max_concurrent_tasks: 1,
            queue: QueueConfig::default(),
            workspace: WorkspaceConfig::default(),
            env: Default::default(),
        }
    }

    #[test]
    fn opencode_defaults_to_opencode_acp() {
        let result = AcpCommand::from_runtime(&runtime(RuntimeKind::Opencode));

        if let Ok(command) = result {
            assert_eq!(command.command, "opencode");
            assert_eq!(command.args, ["acp"]);
        } else {
            let error = result.expect_err("missing OpenCode should return an error");
            assert!(
                error.to_string().contains("OpenCode CLI"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn opencode_configured_command_defaults_to_acp() {
        let mut runtime = runtime(RuntimeKind::Opencode);
        runtime.command = Some("/tmp/opencode".to_string());

        let command = AcpCommand::from_runtime(&runtime).unwrap();

        assert_eq!(command.command, "/tmp/opencode");
        assert_eq!(command.args, ["acp"]);
    }

    #[test]
    fn goose_default_requires_acp_cli() {
        let result = AcpCommand::from_runtime(&runtime(RuntimeKind::Goose));

        if let Ok(command) = result {
            assert_eq!(command.command, "goose");
            assert_eq!(command.args, ["acp"]);
        } else {
            let error = result.unwrap_err().to_string();
            assert!(
                error.contains("Goose CLI") || error.contains("Desktop Goose"),
                "{error}"
            );
        }
    }

    #[test]
    fn goose_configured_command_defaults_to_acp() {
        let mut runtime = runtime(RuntimeKind::Goose);
        runtime.command = Some("goose".to_string());

        let command = AcpCommand::from_runtime(&runtime).unwrap();

        assert_eq!(command.command, "goose");
        assert_eq!(command.args, ["acp"]);
    }

    #[test]
    fn pi_configured_command_defaults_to_acp() {
        let mut runtime = runtime(RuntimeKind::Pi);
        runtime.command = Some("pi".to_string());

        let command = AcpCommand::from_runtime(&runtime).unwrap();

        assert_eq!(command.command, "pi");
        assert_eq!(command.args, ["acp"]);
    }

    #[test]
    fn acp_requires_command() {
        let error = AcpCommand::from_runtime(&runtime(RuntimeKind::Acp))
            .unwrap_err()
            .to_string();

        assert!(error.contains("requires command"));
    }

    #[test]
    fn acp_process_args_include_runtime_env() {
        let mut runtime = runtime(RuntimeKind::Opencode);
        runtime.command = Some("opencode".to_string());
        runtime.env.insert("GOOSE_PROVIDER".into(), "openai".into());
        runtime.env.insert("GOOSE_MODEL".into(), "mesh".into());
        let agent = test_agent_for_runtime(runtime);
        let paths = test_task_runtime_paths();

        let args = acp_process_args(&agent, Path::new("/tmp/workspace"), &paths).unwrap();

        assert_eq!(args[0], "GOOSE_MODEL=mesh");
        assert_eq!(args[1], "GOOSE_PROVIDER=openai");
        assert_eq!(args[2], "opencode");
        assert_eq!(args[3], "acp");
    }

    #[test]
    fn acp_process_args_expand_runtime_templates_and_env() {
        let root = temp_root("templates");
        let instructions_path = root.join("instructions.md");
        std::fs::write(&instructions_path, "Review carefully.").unwrap();
        let mut runtime = runtime(RuntimeKind::Acp);
        runtime.command = Some("{{ env.HOME }}/bin/harness".to_string());
        runtime.args = vec![
            "--agent".to_string(),
            "{{ agent.id }}".to_string(),
            "--cwd".to_string(),
            "{{ task.workspace }}".to_string(),
            "--prompt".to_string(),
            "{{ task.prompt_path }}".to_string(),
            "--mcp".to_string(),
            "{{ mesh.mcp_url }}".to_string(),
            "--config".to_string(),
            "$HOME/.config/harness.toml".to_string(),
        ];
        runtime.env.insert(
            "TASK_ARTIFACTS".to_string(),
            "{{ task.artifacts_dir }}".to_string(),
        );
        runtime.model = Some("qwen3-coder".to_string());
        let mut agent = test_agent_for_runtime(runtime);
        agent.dir = root.join("pr-review");
        agent.card_path = agent.dir.join("agent-card.json");
        agent.runtime_path = agent.dir.join("runtime.toml");
        agent.runtime.instructions = Some(InstructionsConfig {
            file: Some(instructions_path.clone()),
            delivery: InstructionDelivery::FirstPrompt,
        });
        let paths = TaskRuntimePaths {
            task_id: "task-42".to_string(),
            data_dir: root.join("data"),
            prompt_path: root.join("data/a2a/agents/pr-review/runtime/task-42/prompt.txt"),
            artifacts_dir: root.join("data/a2a/agents/pr-review/runtime/task-42/artifacts"),
            logs_dir: root.join("data/a2a/agents/pr-review/runtime/task-42/logs"),
        };

        let args = acp_process_args(&agent, Path::new("/tmp/workspace"), &paths).unwrap();

        assert_eq!(
            args[0],
            format!("TASK_ARTIFACTS={}", paths.artifacts_dir.display())
        );
        assert_eq!(
            args[1],
            format!("{}/bin/harness", std::env::var("HOME").unwrap())
        );
        let expected = vec![
            "--agent".to_string(),
            "pr-review".to_string(),
            "--cwd".to_string(),
            "/tmp/workspace".to_string(),
            "--prompt".to_string(),
            paths.prompt_path.display().to_string(),
            "--mcp".to_string(),
            "http://127.0.0.1:3131/mcp".to_string(),
            "--config".to_string(),
            format!("{}/.config/harness.toml", std::env::var("HOME").unwrap()),
        ];
        assert_eq!(&args[2..], expected.as_slice());
    }

    #[test]
    fn prompt_includes_first_prompt_instructions() {
        let root = temp_root("instructions");
        let instructions_path = root.join("instructions.md");
        std::fs::write(&instructions_path, "Review the code carefully.").unwrap();
        let mut agent = test_agent(&root);
        agent.runtime.instructions = Some(InstructionsConfig {
            file: Some(instructions_path),
            delivery: InstructionDelivery::FirstPrompt,
        });
        let message = Message::new(Role::User, vec![Part::text("Please review PR 42")]);

        let prompt = task_prompt(&agent, Some(&message)).unwrap();

        assert_eq!(
            prompt,
            "Review the code carefully.\n\n---\n\nPlease review PR 42"
        );
    }

    #[test]
    fn temp_workspace_uses_agent_and_task_ids() {
        let root = temp_root("workspace");
        let mut agent = test_agent(&root);
        agent.runtime.runtime.workspace = WorkspaceConfig {
            mode: WorkspaceMode::TempPerTask,
            path: None,
            prefix: Some("mesh-test-".to_string()),
            keep: WorkspaceKeep::OnFailure,
        };

        let workspace = task_workspace(&agent, "task/1").unwrap();

        assert!(workspace.ends_with("mesh-test-pr-review-task_1"));
        assert!(workspace.is_dir());
    }

    #[test]
    fn completed_task_collects_harness_artifacts() {
        let root = temp_root("artifacts");
        let artifacts_dir = root.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        std::fs::write(artifacts_dir.join("summary.md"), "# Review\n\nNo findings.").unwrap();
        std::fs::write(
            artifacts_dir.join("findings.json"),
            r#"[{"severity":"low","issue":"missing test"}]"#,
        )
        .unwrap();
        let paths = TaskRuntimePaths {
            task_id: "task-1".to_string(),
            data_dir: root.clone(),
            prompt_path: root.join("prompt.txt"),
            artifacts_dir,
            logs_dir: root.join("logs"),
        };

        let task = completed_task_from_paths(test_context(), "fallback output".to_string(), &paths)
            .unwrap();

        let artifacts = task
            .artifacts
            .expect("completed task should have artifacts");
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].artifact_id, "findings.json");
        assert_eq!(artifacts[1].artifact_id, "summary.md");
        assert_eq!(
            artifacts[1].parts[0].as_text(),
            Some("# Review\n\nNo findings.")
        );
    }

    #[test]
    fn completed_task_adds_default_artifacts_when_harness_writes_none() {
        let root = temp_root("fallback-artifact");
        let artifacts_dir = root.join("artifacts");
        std::fs::create_dir_all(&artifacts_dir).unwrap();
        let paths = TaskRuntimePaths {
            task_id: "task-1".to_string(),
            data_dir: root.clone(),
            prompt_path: root.join("prompt.txt"),
            artifacts_dir,
            logs_dir: root.join("logs"),
        };

        let task =
            completed_task_from_paths(test_context(), "review output".to_string(), &paths).unwrap();

        let artifacts = task
            .artifacts
            .expect("completed task should have artifacts");
        assert_eq!(artifacts.len(), 2);
        assert_eq!(artifacts[0].artifact_id, "summary.md");
        assert_eq!(artifacts[0].parts[0].as_text(), Some("review output"));
        assert_eq!(artifacts[1].artifact_id, "findings.json");
        let findings = serde_json::to_value(&artifacts[1].parts[0]).unwrap();
        assert_eq!(findings["data"]["schema_version"], 1);
        assert_eq!(findings["data"]["findings"], json!([]));
    }

    #[tokio::test]
    #[ignore = "requires an installed and configured OpenCode ACP agent"]
    async fn live_opencode_acp_initialize_smoke() {
        let runtime = live_opencode_runtime();
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            initialize_acp_session_once(&runtime, &std::env::temp_dir()),
        )
        .await
        .expect("OpenCode ACP initialize smoke timed out")
        .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an installed and configured OpenCode ACP agent with a reachable provider"]
    async fn live_opencode_acp_smoke() {
        let runtime = live_opencode_runtime();
        let agent = test_agent_for_runtime(runtime);
        let paths = test_task_runtime_paths();
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            run_acp_once(
                &agent,
                &std::env::temp_dir(),
                &paths,
                "Reply with exactly: mesh-a2a-smoke".to_string(),
            ),
        )
        .await
        .expect("OpenCode ACP smoke timed out")
        .unwrap();

        assert!(output.contains("mesh-a2a-smoke"), "{output}");
    }

    fn live_opencode_runtime() -> RuntimeConfig {
        RuntimeConfig {
            kind: RuntimeKind::Opencode,
            command: std::env::var("MESH_LLM_OPENCODE_COMMAND").ok(),
            args: std::env::var("MESH_LLM_OPENCODE_ARGS")
                .ok()
                .map(|args| args.split_whitespace().map(ToOwned::to_owned).collect())
                .unwrap_or_default(),
            model: None,
            mode: None,
            session_policy: None,
            max_concurrent_tasks: 1,
            queue: QueueConfig::default(),
            workspace: WorkspaceConfig::default(),
            env: Default::default(),
        }
    }

    fn test_context() -> mesh_agents_a2a::ExecutorContext {
        mesh_agents_a2a::ExecutorContext {
            message: None,
            task_id: "task-1".to_string(),
            stored_task: None,
            context_id: "context-1".to_string(),
            metadata: None,
            user: None,
            service_params: Default::default(),
            tenant: None,
        }
    }

    fn test_agent(root: &Path) -> AgentDefinition {
        AgentDefinition {
            id: "pr-review".to_string(),
            dir: root.join("pr-review"),
            card_path: root.join("pr-review/agent-card.json"),
            runtime_path: root.join("pr-review/runtime.toml"),
            card: serde_json::from_value(serde_json::json!({
                "name": "PR Review",
                "description": "Reviews pull requests.",
                "version": "1.0.0",
                "supportedInterfaces": [],
                "capabilities": {},
                "defaultInputModes": ["text/plain"],
                "defaultOutputModes": ["text/markdown"],
                "skills": []
            }))
            .unwrap(),
            runtime: AgentRuntimeConfig::default(),
        }
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mesh-agents-acp-bridge-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }
}
