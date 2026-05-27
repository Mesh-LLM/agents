//! ACP harness bridge primitives for mesh-llm A2A agents.
//!
//! The bridge uses the official `agent-client-protocol` crate for ACP protocol
//! boundaries and the official A2A server executor trait from `a2a-server-lf`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use futures::{stream, stream::BoxStream};
use mesh_agents_a2a::{
    A2AError, AgentDefinition, AgentExecutor, InstructionDelivery, Message, Part, Role,
    RuntimeConfig, RuntimeKind, StreamResponse, Task, TaskState, TaskStatus, WorkspaceMode,
};

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

fn executable_on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(name).is_file()))
}

#[derive(Clone, Debug)]
pub struct AcpAgentExecutor {
    agent: AgentDefinition,
}

impl AcpAgentExecutor {
    #[must_use]
    pub fn new(agent: AgentDefinition) -> Self {
        Self { agent }
    }
}

impl AgentExecutor for AcpAgentExecutor {
    fn execute(
        &self,
        ctx: mesh_agents_a2a::ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let agent = self.agent.clone();
        Box::pin(stream::once(async move {
            let task = run_task(agent, ctx).await;
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

async fn run_task(agent: AgentDefinition, ctx: mesh_agents_a2a::ExecutorContext) -> Task {
    let prompt = match task_prompt(&agent, ctx.message.as_ref()) {
        Ok(prompt) => prompt,
        Err(err) => return failed_task(ctx, err.to_string()),
    };
    let workspace = match task_workspace(&agent, &ctx.task_id) {
        Ok(workspace) => workspace,
        Err(err) => return failed_task(ctx, err.to_string()),
    };
    match run_acp_once(&agent.runtime.runtime, &workspace, prompt).await {
        Ok(output) => completed_task(ctx, output),
        Err(err) => failed_task(ctx, err.to_string()),
    }
}

async fn run_acp_once(runtime: &RuntimeConfig, workspace: &Path, prompt: String) -> Result<String> {
    let agent = AcpAgent::from_args(acp_process_args(runtime)?)?;

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
    let agent = AcpAgent::from_args(acp_process_args(runtime)?)?;

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

fn acp_process_args(runtime: &RuntimeConfig) -> Result<Vec<String>> {
    let command = AcpCommand::from_runtime(runtime)?;
    let mut args = runtime
        .env
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>();
    args.push(command.command);
    args.extend(command.args);
    Ok(args)
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

fn completed_task(ctx: mesh_agents_a2a::ExecutorContext, output: String) -> Task {
    let response = Message::new(Role::Agent, vec![Part::text(output)]);
    Task {
        id: ctx.task_id,
        context_id: ctx.context_id,
        status: TaskStatus {
            state: TaskState::Completed,
            message: Some(response),
            timestamp: None,
        },
        artifacts: None,
        history: ctx.stored_task.and_then(|task| task.history),
        metadata: Some(executor_metadata()),
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
    HashMap::from([(
        "mesh_llm_executor".to_string(),
        serde_json::Value::String("acp".to_string()),
    )])
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
        let command = AcpCommand::from_runtime(&runtime(RuntimeKind::Opencode)).unwrap();

        assert_eq!(command.command, "opencode");
        assert_eq!(command.args, ["acp"]);
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

        let args = acp_process_args(&runtime).unwrap();

        assert_eq!(args[0], "GOOSE_MODEL=mesh");
        assert_eq!(args[1], "GOOSE_PROVIDER=openai");
        assert_eq!(args[2], "opencode");
        assert_eq!(args[3], "acp");
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
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            run_acp_once(
                &runtime,
                &std::env::temp_dir(),
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
