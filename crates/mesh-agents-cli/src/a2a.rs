use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use http_body_util::BodyExt;
use mesh_agents_a2a::{
    agent_task_store_path, AgentDefinition, AgentRegistry, Artifact, JsonRpcId, JsonRpcRequest,
    JsonRpcResponse, LocalAgentService, PersistentTaskStore, QueueMode, Task, TaskStore,
};
use mesh_agents_acp_bridge::AcpAgentExecutor;
use rmcp::model::{
    object, CallToolRequestParams, CallToolResult, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt as McpServiceExt};
use serde_json::{json, Map, Value};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::ServiceExt as TowerServiceExt;

use crate::mesh::{
    local_agent_summaries, remote_agent_summaries, RemoteAgentCache, RemoteTaskCache,
};

#[cfg(test)]
use mesh_agents_a2a::EchoAgentExecutor;

pub(crate) async fn run_a2a_mcp(agents_dir: Option<&Path>, data_dir: Option<&Path>) -> Result<()> {
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

#[allow(clippy::manual_async_fn)]
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
    gates: AgentGates,
}

impl LocalA2aTools {
    fn new(agents_dir: PathBuf, data_dir: PathBuf, executor_mode: ExecutorMode) -> Self {
        Self {
            agents_dir,
            data_dir,
            executor_mode,
            gates: AgentGates::default(),
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
        let mut agents = local_agent_summaries(&self.agents_dir)?;
        agents.extend(remote_agent_summaries(&self.data_dir)?);
        Ok(json!({ "agents": agents }))
    }

    fn get_agent(&self, agent_id: &str) -> Result<Value> {
        if let Some(agent) = self.registry()?.get(agent_id) {
            if !agent.runtime.enabled {
                bail!("agent `{agent_id}` is disabled");
            }
            return Ok(json!({
                "agent_id": agent.id,
                "location": "local",
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
            }));
        }

        let remote = RemoteAgentCache::load(&self.data_dir)?
            .get(agent_id)
            .cloned()
            .with_context(|| format!("agent `{agent_id}` was not found"))?;
        Ok(json!({
            "agent_id": remote.agent_id,
            "location": "remote",
            "peer_id": remote.peer_id,
            "agent_card": remote.card,
        }))
    }

    async fn send_message(&self, args: Map<String, Value>) -> Result<Value> {
        let agent_id = required_arg(&args, "agent_id")?;
        let message = required_arg(&args, "message")?;
        let context_id = optional_arg(&args, "context_id");
        let agent = self.enabled_agent(agent_id)?;
        let _permit = self.gates.acquire(&agent).await?;
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
            ExecutorMode::Acp => LocalAgentService::new(
                agent.clone(),
                &self.data_dir,
                AcpAgentExecutor::new(agent, self.data_dir.clone()),
            )
            .map_err(|error| anyhow!("failed to create local A2A service: {error}")),
            #[cfg(test)]
            ExecutorMode::Echo => LocalAgentService::new(agent, &self.data_dir, EchoAgentExecutor)
                .map_err(|error| anyhow!("failed to create local A2A service: {error}")),
        }
    }

    async fn load_task(&self, agent_id: &str, task_id: &str) -> Result<Task> {
        if self.enabled_agent(agent_id).is_err() {
            let remote = RemoteTaskCache::load(&self.data_dir)?
                .get(agent_id, task_id)
                .cloned()
                .with_context(|| {
                    format!("remote task `{task_id}` was not found for agent `{agent_id}`")
                })?;
            return decode_remote_task(remote.result)
                .with_context(|| format!("failed to decode remote task `{task_id}`"));
        }
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

fn decode_remote_task(value: Value) -> Result<Task> {
    if let Some(task) = value.get("task") {
        return serde_json::from_value(task.clone()).context("failed to decode wrapped task");
    }
    serde_json::from_value(value).context("failed to decode task")
}

#[derive(Clone, Debug, Default)]
struct AgentGates {
    gates: Arc<Mutex<std::collections::HashMap<String, Arc<AgentGate>>>>,
}

impl AgentGates {
    async fn acquire(&self, agent: &AgentDefinition) -> Result<AgentGatePermit> {
        let gate = self.gate_for(agent)?;
        gate.acquire(agent).await
    }

    fn gate_for(&self, agent: &AgentDefinition) -> Result<Arc<AgentGate>> {
        let limit = agent.runtime.runtime.max_concurrent_tasks.max(1);
        let mut gates = self
            .gates
            .lock()
            .map_err(|_| anyhow!("agent concurrency gate lock was poisoned"))?;
        let gate = gates
            .entry(agent.id.clone())
            .or_insert_with(|| Arc::new(AgentGate::new(limit)));
        if gate.limit == limit {
            return Ok(gate.clone());
        }

        let gate = Arc::new(AgentGate::new(limit));
        gates.insert(agent.id.clone(), gate.clone());
        Ok(gate)
    }
}

#[derive(Debug)]
struct AgentGate {
    limit: usize,
    semaphore: Arc<Semaphore>,
    queued: AtomicUsize,
}

impl AgentGate {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            semaphore: Arc::new(Semaphore::new(limit)),
            queued: AtomicUsize::new(0),
        }
    }

    async fn acquire(&self, agent: &AgentDefinition) -> Result<AgentGatePermit> {
        match agent.runtime.runtime.queue.mode {
            QueueMode::Reject => self.try_acquire(agent),
            QueueMode::Queue => self.acquire_queued(agent).await,
        }
    }

    fn try_acquire(&self, agent: &AgentDefinition) -> Result<AgentGatePermit> {
        self.semaphore
            .clone()
            .try_acquire_owned()
            .map(|permit| AgentGatePermit { _permit: permit })
            .map_err(|_| anyhow!("agent `{}` is at its concurrency limit", agent.id))
    }

    async fn acquire_queued(&self, agent: &AgentDefinition) -> Result<AgentGatePermit> {
        let max_pending = agent.runtime.runtime.queue.max_pending_tasks;
        if max_pending == 0 {
            return self.try_acquire(agent);
        }

        let queued = self.queued.fetch_add(1, Ordering::SeqCst) + 1;
        if queued > max_pending {
            self.queued.fetch_sub(1, Ordering::SeqCst);
            bail!(
                "agent `{}` task queue is full; pending limit is {}",
                agent.id,
                max_pending
            );
        }
        let queued_slot = QueuedSlot(&self.queued);

        let permit = self
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map(|permit| AgentGatePermit { _permit: permit })
            .map_err(|_| anyhow!("agent `{}` concurrency gate was closed", agent.id));
        drop(queued_slot);
        permit
    }
}

#[derive(Debug)]
struct AgentGatePermit {
    _permit: OwnedSemaphorePermit,
}

struct QueuedSlot<'a>(&'a AtomicUsize);

impl Drop for QueuedSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
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

fn required_arg<'a>(args: &'a Map<String, Value>, name: &str) -> Result<&'a str> {
    args.get(name)
        .and_then(Value::as_str)
        .with_context(|| format!("missing required string argument `{name}`"))
}

fn optional_arg<'a>(args: &'a Map<String, Value>, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

fn artifact_args(args: &Map<String, Value>) -> Result<(&str, &str, &str)> {
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
            "List local and mesh-discovered A2A agents that are enabled.",
            json!({}),
        ),
        tool(
            "get_agent",
            "Get one local or mesh-discovered A2A Agent Card.",
            agent_schema(),
        ),
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
#[allow(clippy::unwrap_used)]
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

    #[tokio::test]
    async fn reject_mode_denies_second_concurrent_task() {
        let fixture = AgentFixture::new("reject-limit");
        let mut agent = fixture.load_agent("reject-limit");
        agent.runtime.runtime.queue.mode = QueueMode::Reject;
        agent.runtime.runtime.max_concurrent_tasks = 1;
        let gates = AgentGates::default();

        let _first = gates.acquire(&agent).await.unwrap();
        let error = gates.acquire(&agent).await.unwrap_err().to_string();

        assert!(error.contains("concurrency limit"), "{error}");
    }

    #[tokio::test]
    async fn queue_mode_rejects_when_pending_queue_is_full() {
        let fixture = AgentFixture::new("queued-limit");
        let mut agent = fixture.load_agent("queued-limit");
        agent.runtime.runtime.queue.mode = QueueMode::Queue;
        agent.runtime.runtime.queue.max_pending_tasks = 1;
        agent.runtime.runtime.max_concurrent_tasks = 1;
        let gates = AgentGates::default();

        let first = gates.acquire(&agent).await.unwrap();
        let waiting = tokio::spawn({
            let gates = gates.clone();
            let agent = agent.clone();
            async move { gates.acquire(&agent).await }
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let error = gates.acquire(&agent).await.unwrap_err().to_string();
        drop(first);
        let _second = waiting.await.unwrap().unwrap();

        assert!(error.contains("task queue is full"), "{error}");
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

        fn load_agent(&self, agent_id: &str) -> AgentDefinition {
            AgentRegistry::load_from_dir(&self.agents_dir)
                .unwrap()
                .get(agent_id)
                .unwrap()
                .clone()
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
