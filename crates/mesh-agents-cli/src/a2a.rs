use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use http_body_util::BodyExt;
use mesh_agents_a2a::{
    agent_task_store_path, AgentDefinition, AgentRegistry, Artifact, JsonRpcId, JsonRpcRequest,
    JsonRpcResponse, LocalAgentService, PersistentTaskStore, Task, TaskStore,
};
use mesh_agents_acp_bridge::AcpAgentExecutor;
use rmcp::model::{
    object, CallToolRequestParams, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt as McpServiceExt};
use serde_json::{json, Map, Value};
use tower::ServiceExt as TowerServiceExt;

#[cfg(test)]
use mesh_agents_a2a::EchoAgentExecutor;

use crate::A2aCommand;

pub(crate) async fn dispatch_a2a_command(command: &A2aCommand) -> Result<()> {
    match command {
        A2aCommand::Mcp {
            agents_dir,
            data_dir,
        } => run_a2a_mcp(agents_dir.as_deref(), data_dir.as_deref()).await,
    }
}

async fn run_a2a_mcp(agents_dir: Option<&Path>, data_dir: Option<&Path>) -> Result<()> {
    let server = A2aMcpServer::new(LocalA2aTools::new(
        resolve_agents_dir(agents_dir)?,
        resolve_data_dir(data_dir)?,
        ExecutorMode::Acp,
    ));
    let service = server.serve(rmcp::transport::io::stdio()).await?;
    let _quit = service.waiting().await?;
    Ok(())
}

#[derive(Clone, Debug)]
struct A2aMcpServer {
    tools: Arc<LocalA2aTools>,
}

impl A2aMcpServer {
    fn new(tools: LocalA2aTools) -> Self {
        Self {
            tools: Arc::new(tools),
        }
    }
}

impl ServerHandler for A2aMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                "mesh-llm-a2a",
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions("Use these tools as an A2A client for local mesh-llm agents.")
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        a2a_tools().into_iter().find(|tool| tool.name == name)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + Send + '_ {
        async { Ok(ListToolsResult::with_all_items(a2a_tools())) }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResult, McpError>> + Send + '_ {
        async move {
            let arguments = request.arguments.unwrap_or_default();
            match self.tools.call(&request.name, arguments).await {
                Ok(value) => Ok(CallToolResult::structured(value)),
                Err(error) => Ok(CallToolResult::structured_error(json!({
                    "error": error.to_string(),
                }))),
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum ExecutorMode {
    Acp,
    #[cfg(test)]
    Echo,
}

#[derive(Clone, Debug)]
struct LocalA2aTools {
    agents_dir: PathBuf,
    data_dir: PathBuf,
    executor_mode: ExecutorMode,
}

impl LocalA2aTools {
    fn new(agents_dir: PathBuf, data_dir: PathBuf, executor_mode: ExecutorMode) -> Self {
        Self {
            agents_dir,
            data_dir,
            executor_mode,
        }
    }

    async fn call(&self, name: &str, args: Map<String, Value>) -> Result<Value> {
        match name {
            "get_agents" => self.get_agents(),
            "get_agent" => self.get_agent(required_arg(&args, "agent_id")?),
            "send_message" => self.send_message(args).await,
            "get_task" => self.get_task(args).await,
            "view_text_artifact" => self.view_text_artifact(args).await,
            "view_data_artifact" => self.view_data_artifact(args).await,
            unknown => bail!("unknown A2A MCP tool `{unknown}`"),
        }
    }

    fn get_agents(&self) -> Result<Value> {
        let registry = self.registry()?;
        let agents = registry
            .agents()
            .iter()
            .filter(|agent| agent.runtime.enabled)
            .map(agent_summary)
            .collect::<Vec<_>>();
        Ok(json!({ "agents": agents }))
    }

    fn get_agent(&self, agent_id: &str) -> Result<Value> {
        let agent = self.enabled_agent(agent_id)?;
        Ok(json!({
            "agent_id": agent.id,
            "agent_card": agent.card,
            "runtime": {
                "type": agent.runtime.runtime.kind,
                "max_concurrent_tasks": agent.runtime.runtime.max_concurrent_tasks,
                "workspace": agent.runtime.runtime.workspace,
            },
            "paths": {
                "dir": agent.dir,
                "agent_card": agent.card_path,
                "runtime_config": agent.runtime_path,
            },
        }))
    }

    async fn send_message(&self, args: Map<String, Value>) -> Result<Value> {
        let agent_id = required_arg(&args, "agent_id")?;
        let message = required_arg(&args, "message")?;
        let context_id = optional_arg(&args, "context_id");
        let agent = self.enabled_agent(agent_id)?;
        let response = self.post_send_message(&agent, message, context_id).await?;
        let result = response
            .result
            .context("A2A response did not include a result")?;
        let task = result.get("task").unwrap_or(&result);
        Ok(json!({
            "agent_id": agent_id,
            "task_id": task.get("id").and_then(Value::as_str),
            "context_id": task.get("contextId").and_then(Value::as_str),
            "result": result,
        }))
    }

    async fn get_task(&self, args: Map<String, Value>) -> Result<Value> {
        let agent_id = required_arg(&args, "agent_id")?;
        let task_id = required_arg(&args, "task_id")?;
        let task = self.load_task(agent_id, task_id).await?;
        Ok(json!({ "agent_id": agent_id, "task": task }))
    }

    async fn view_text_artifact(&self, args: Map<String, Value>) -> Result<Value> {
        let (agent_id, task_id, artifact_id) = artifact_args(&args)?;
        let artifact = self.load_artifact(agent_id, task_id, artifact_id).await?;
        let text = artifact
            .parts
            .iter()
            .filter_map(|part| part.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() {
            bail!("artifact `{artifact_id}` has no text parts");
        }
        Ok(json!({
            "agent_id": agent_id,
            "task_id": task_id,
            "artifact_id": artifact.artifact_id,
            "text": text,
        }))
    }

    async fn view_data_artifact(&self, args: Map<String, Value>) -> Result<Value> {
        let (agent_id, task_id, artifact_id) = artifact_args(&args)?;
        let artifact = self.load_artifact(agent_id, task_id, artifact_id).await?;
        Ok(json!({
            "agent_id": agent_id,
            "task_id": task_id,
            "artifact": artifact,
        }))
    }

    async fn post_send_message(
        &self,
        agent: &AgentDefinition,
        message: &str,
        context_id: Option<&str>,
    ) -> Result<JsonRpcResponse> {
        let service = self.local_service(agent.clone())?;
        let body = serde_json::to_string(&send_message_request(message, context_id))?;
        let request = axum::http::Request::builder()
            .uri("/")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body))?;
        let response = service.jsonrpc_router().oneshot(request).await?;
        let body = response.into_body().collect().await?.to_bytes();
        let response: JsonRpcResponse = serde_json::from_slice(&body)?;
        if let Some(error) = response.error.as_ref() {
            bail!("A2A JSON-RPC error {}: {}", error.code, error.message);
        }
        Ok(response)
    }

    fn local_service(&self, agent: AgentDefinition) -> Result<LocalAgentService> {
        match self.executor_mode {
            ExecutorMode::Acp => {
                LocalAgentService::new(agent.clone(), &self.data_dir, AcpAgentExecutor::new(agent))
                    .map_err(|error| anyhow!("failed to create local A2A service: {error}"))
            }
            #[cfg(test)]
            ExecutorMode::Echo => LocalAgentService::new(agent, &self.data_dir, EchoAgentExecutor)
                .map_err(|error| anyhow!("failed to create local A2A service: {error}")),
        }
    }

    async fn load_task(&self, agent_id: &str, task_id: &str) -> Result<Task> {
        let _agent = self.enabled_agent(agent_id)?;
        let path = agent_task_store_path(&self.data_dir, agent_id);
        let store = PersistentTaskStore::open(&path)
            .map_err(|error| anyhow!("failed to open task store {}: {error}", path.display()))?;
        store
            .get(task_id)
            .await
            .map_err(|error| anyhow!("failed to read task `{task_id}`: {error}"))?
            .with_context(|| format!("task `{task_id}` was not found for agent `{agent_id}`"))
    }

    async fn load_artifact(
        &self,
        agent_id: &str,
        task_id: &str,
        artifact_id: &str,
    ) -> Result<Artifact> {
        let task = self.load_task(agent_id, task_id).await?;
        let artifacts = task.artifacts.unwrap_or_default();
        artifacts
            .into_iter()
            .find(|artifact| artifact.artifact_id == artifact_id)
            .with_context(|| format!("artifact `{artifact_id}` was not found on task `{task_id}`"))
    }

    fn enabled_agent(&self, agent_id: &str) -> Result<AgentDefinition> {
        let registry = self.registry()?;
        let agent = registry
            .get(agent_id)
            .with_context(|| format!("agent `{agent_id}` was not found"))?;
        if !agent.runtime.enabled {
            bail!("agent `{agent_id}` is disabled");
        }
        Ok(agent.clone())
    }

    fn registry(&self) -> Result<AgentRegistry> {
        AgentRegistry::load_from_dir(&self.agents_dir)
    }
}

fn send_message_request(message: &str, context_id: Option<&str>) -> JsonRpcRequest {
    let mut params = json!({
        "message": {
            "messageId": next_message_id(),
            "role": "ROLE_USER",
            "parts": [{ "text": message }],
        }
    });
    if let Some(context_id) = context_id {
        params["contextId"] = Value::String(context_id.to_string());
    }
    JsonRpcRequest::new(JsonRpcId::Number(1), "SendMessage", Some(params))
}

fn next_message_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("mcp-{nanos}")
}

fn agent_summary(agent: &AgentDefinition) -> Value {
    json!({
        "agent_id": agent.id,
        "name": agent.card.name,
        "description": agent.card.description,
        "version": agent.card.version,
        "runtime": agent.runtime.runtime.kind,
        "max_concurrent_tasks": agent.runtime.runtime.max_concurrent_tasks,
        "card_url": format!("mesh://agents/{}", agent.id),
    })
}

fn required_arg<'a>(args: &'a Map<String, Value>, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing required string argument `{name}`"))
}

fn optional_arg<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

fn artifact_args<'a>(args: &'a Map<String, Value>) -> Result<(&'a str, &'a str, &'a str)> {
    Ok((
        required_arg(args, "agent_id")?,
        required_arg(args, "task_id")?,
        required_arg(args, "artifact_id")?,
    ))
}

fn a2a_tools() -> Vec<Tool> {
    vec![
        tool(
            "get_agents",
            "List local A2A agents that are enabled.",
            json!({}),
        ),
        tool("get_agent", "Get one local A2A Agent Card.", agent_schema()),
        tool(
            "send_message",
            "Send a user message to a local A2A agent.",
            send_message_schema(),
        ),
        tool("get_task", "Read one persisted A2A task.", task_schema()),
        tool(
            "view_text_artifact",
            "Read text content from an A2A task artifact.",
            artifact_schema(),
        ),
        tool(
            "view_data_artifact",
            "Read structured data for an A2A task artifact.",
            artifact_schema(),
        ),
    ]
}

fn tool(name: &'static str, description: &'static str, schema: Value) -> Tool {
    Tool::new(name, description, object(schema))
}

fn agent_schema() -> Value {
    schema(
        ["agent_id"],
        json!({
            "agent_id": { "type": "string" }
        }),
    )
}

fn send_message_schema() -> Value {
    schema(
        ["agent_id", "message"],
        json!({
            "agent_id": { "type": "string" },
            "message": { "type": "string" },
            "context_id": { "type": "string" }
        }),
    )
}

fn task_schema() -> Value {
    schema(
        ["agent_id", "task_id"],
        json!({
            "agent_id": { "type": "string" },
            "task_id": { "type": "string" }
        }),
    )
}

fn artifact_schema() -> Value {
    schema(
        ["agent_id", "task_id", "artifact_id"],
        json!({
            "agent_id": { "type": "string" },
            "task_id": { "type": "string" },
            "artifact_id": { "type": "string" }
        }),
    )
}

fn schema<const N: usize>(required: [&str; N], properties: Value) -> Value {
    let required = required.to_vec();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

fn resolve_agents_dir(dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = dir {
        return Ok(dir.to_path_buf());
    }
    Ok(home_dir()?.join(".mesh-llm").join("agents"))
}

fn resolve_data_dir(dir: Option<&Path>) -> Result<PathBuf> {
    if let Some(dir) = dir {
        return Ok(dir.to_path_buf());
    }
    Ok(home_dir()?.join(".mesh-llm"))
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("could not determine home directory")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn exposes_only_a2a_client_tools() {
        let names = a2a_tools()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect::<Vec<_>>();

        assert_eq!(
            names,
            vec![
                "get_agents",
                "get_agent",
                "send_message",
                "get_task",
                "view_text_artifact",
                "view_data_artifact",
            ]
        );
    }

    #[tokio::test]
    async fn local_tools_send_message_through_a2a_service_and_read_task() {
        let fixture = AgentFixture::new("pr-review");
        let tools = LocalA2aTools::new(
            fixture.agents_dir.clone(),
            fixture.data_dir.clone(),
            ExecutorMode::Echo,
        );

        let result = tools
            .call(
                "send_message",
                Map::from_iter([
                    ("agent_id".to_string(), json!("pr-review")),
                    ("message".to_string(), json!("review this")),
                ]),
            )
            .await
            .unwrap();
        let task_id = result["task_id"].as_str().unwrap().to_string();

        let task = tools
            .call(
                "get_task",
                Map::from_iter([
                    ("agent_id".to_string(), json!("pr-review")),
                    ("task_id".to_string(), json!(task_id)),
                ]),
            )
            .await
            .unwrap();

        assert_eq!(task["task"]["status"]["state"], "TASK_STATE_COMPLETED");
    }

    #[test]
    fn get_agents_omits_disabled_agents() {
        let fixture = AgentFixture::new("enabled");
        AgentFixture::new_in(&fixture.root, "disabled", false);
        let tools = LocalA2aTools::new(
            fixture.agents_dir.clone(),
            fixture.data_dir.clone(),
            ExecutorMode::Echo,
        );

        let result = tools.get_agents().unwrap();

        assert_eq!(result["agents"].as_array().unwrap().len(), 1);
        assert_eq!(result["agents"][0]["agent_id"], "enabled");
    }

    struct AgentFixture {
        root: PathBuf,
        agents_dir: PathBuf,
        data_dir: PathBuf,
    }

    impl AgentFixture {
        fn new(agent_id: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "mesh-llm-a2a-mcp-{agent_id}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).unwrap();
            Self::new_in(&root, agent_id, true)
        }

        fn new_in(root: &Path, agent_id: &str, enabled: bool) -> Self {
            let agents_dir = root.join("agents");
            let data_dir = root.join("data");
            let agent_dir = agents_dir.join(agent_id);
            fs::create_dir_all(agent_dir.join("work")).unwrap();
            fs::write(agent_dir.join("agent-card.json"), agent_card(agent_id)).unwrap();
            fs::write(agent_dir.join("runtime.toml"), runtime_config(enabled)).unwrap();
            Self {
                root: root.to_path_buf(),
                agents_dir,
                data_dir,
            }
        }
    }

    fn agent_card(agent_id: &str) -> String {
        json!({
            "name": agent_id,
            "description": "A test agent.",
            "version": "1.0.0",
            "supportedInterfaces": [{
                "url": format!("http://127.0.0.1:3131/a2a/agents/{agent_id}"),
                "protocolBinding": "JSONRPC",
                "protocolVersion": "1.0"
            }],
            "capabilities": { "streaming": true },
            "defaultInputModes": ["text/plain"],
            "defaultOutputModes": ["text/markdown"],
            "skills": []
        })
        .to_string()
    }

    fn runtime_config(enabled: bool) -> String {
        format!(
            r#"
enabled = {enabled}
visibility = "private"

[runtime]
type = "opencode"
max_concurrent_tasks = 1

[runtime.workspace]
mode = "path"
path = "work"

[tools.mesh]
enabled = true
"#
        )
    }
}
