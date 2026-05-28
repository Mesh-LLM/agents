use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use http_body_util::BodyExt;
use mesh_agents_a2a::{
    jsonrpc_methods, AgentDefinition, AgentRegistry, Artifact, JsonRpcId, JsonRpcRequest,
    JsonRpcResponse, LocalAgentService, Part, QueueMode, Task, TaskStore,
};
use mesh_agents_acp_bridge::AcpAgentExecutor;
use mesh_llm_plugin::{
    capability, plugin_server_info, PluginContext, PluginMetadata, PluginRuntime,
    PluginStartupPolicy,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::AsyncReadExt;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tower::ServiceExt as TowerServiceExt;

use crate::mesh::{
    local_advertisements, local_agent_summaries, now_ms, remote_agent_summaries,
    MeshProtocolMessage, RemoteAgentCache, RemoteSendMessageRequest, RemoteSendMessageResponse,
    RemoteTaskCache, RemoteTaskRecord, CHANNEL, KIND_ADVERTISE, KIND_SEND_MESSAGE_REQUEST,
    KIND_SEND_MESSAGE_RESPONSE,
};

const DEFAULT_PLUGIN_NAME: &str = "agents";
const MCP_ENDPOINT_ID: &str = "mcp";

#[derive(Clone, Debug)]
struct PluginState {
    agents_dir: PathBuf,
    data_dir: PathBuf,
    gates: AgentGates,
}

impl PluginState {
    fn new() -> Result<Self> {
        Ok(Self {
            agents_dir: resolve_agents_dir()?,
            data_dir: resolve_data_dir()?,
            gates: AgentGates::default(),
        })
    }
}

#[derive(Debug, Default, Deserialize, JsonSchema)]
struct EmptyArgs {}

#[derive(Debug, Deserialize, JsonSchema)]
struct AgentArgs {
    agent_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SendMessageArgs {
    agent_id: String,
    message: String,
    context_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TaskArgs {
    agent_id: String,
    task_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ArtifactArgs {
    agent_id: String,
    task_id: String,
    artifact_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum MeshStreamRequest {
    SendMessage {
        agent_id: String,
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        context_id: Option<String>,
    },
}

pub(crate) async fn run_plugin_from_env() -> Result<()> {
    let plugin_name = std::env::var("MESH_LLM_PLUGIN_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_PLUGIN_NAME.to_string());
    PluginRuntime::run(build_plugin(plugin_name)?).await
}

fn build_plugin(name: String) -> Result<mesh_llm_plugin::SimplePlugin> {
    let command = current_agents_command();
    build_plugin_with_command(name, command, PluginState::new()?)
}

fn build_plugin_with_command(
    name: String,
    command: String,
    state: PluginState,
) -> Result<mesh_llm_plugin::SimplePlugin> {
    let init_state = state.clone();
    let mesh_state = state.clone();
    let channel_state = state.clone();
    let get_agents_state = state.clone();
    let get_agent_state = state.clone();
    let send_message_state = state.clone();
    let get_task_state = state.clone();
    let view_text_artifact_state = state.clone();
    let view_data_artifact_state = state.clone();
    let open_stream_state = state;

    let plugin = mesh_llm_plugin::plugin! {
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
            capability("agents:mesh-discovery"),
        ],
        mesh: [
            mesh_llm_plugin::mesh::channel(CHANNEL),
        ],
        events: [
            mesh_llm_plugin::events::peer_up(),
            mesh_llm_plugin::events::peer_updated(),
            mesh_llm_plugin::events::local_accepting(),
            mesh_llm_plugin::events::mesh_id_updated(),
        ],
        mcp: [
            mesh_llm_plugin::mcp::external_stdio(MCP_ENDPOINT_ID, command)
                .arg("mcp")
                .namespace("a2a"),
            mesh_llm_plugin::mcp::tool("get_agents")
                .description("List local and mesh-discovered A2A agents.")
                .input::<EmptyArgs>()
                .handle(move |_args, context| {
                    let state = get_agents_state.clone();
                    Box::pin(async move {
                        plugin_get_agents(&state, context)
                            .await
                            .map_err(Into::into)
                    })
                }),
            mesh_llm_plugin::mcp::tool("get_agent")
                .description("Get one local or mesh-discovered A2A Agent Card.")
                .input::<AgentArgs>()
                .handle(move |args, _context| {
                    let state = get_agent_state.clone();
                    Box::pin(async move {
                        plugin_get_agent(&state, &args.agent_id).map_err(Into::into)
                    })
                }),
            mesh_llm_plugin::mcp::tool("send_message")
                .description("Send a user message to a local or mesh-discovered A2A agent.")
                .input::<SendMessageArgs>()
                .handle(move |args, context| {
                    let state = send_message_state.clone();
                    Box::pin(async move {
                        plugin_send_message(&state, args, context)
                            .await
                            .map_err(Into::into)
                    })
                }),
            mesh_llm_plugin::mcp::tool("get_task")
                .description("Read one persisted local or remote A2A task.")
                .input::<TaskArgs>()
                .handle(move |args, _context| {
                    let state = get_task_state.clone();
                    Box::pin(async move {
                        plugin_get_task(&state, &args.agent_id, &args.task_id)
                            .await
                            .map_err(Into::into)
                    })
                }),
            mesh_llm_plugin::mcp::tool("view_text_artifact")
                .description("Read text content from an A2A task artifact.")
                .input::<ArtifactArgs>()
                .handle(move |args, _context| {
                    let state = view_text_artifact_state.clone();
                    Box::pin(async move {
                        plugin_view_text_artifact(
                            &state,
                            &args.agent_id,
                            &args.task_id,
                            &args.artifact_id,
                        )
                        .await
                        .map_err(Into::into)
                    })
                }),
            mesh_llm_plugin::mcp::tool("view_data_artifact")
                .description("Read structured data for an A2A task artifact.")
                .input::<ArtifactArgs>()
                .handle(move |args, _context| {
                    let state = view_data_artifact_state.clone();
                    Box::pin(async move {
                        plugin_view_data_artifact(
                            &state,
                            &args.agent_id,
                            &args.task_id,
                            &args.artifact_id,
                        )
                        .await
                        .map_err(Into::into)
                    })
                }),
        ],
        health: |_context| {
            Box::pin(async move { Ok("mcp=agents mcp mesh_channel=agents.discovery.v1".to_string()) })
        },
        on_initialized: move |context| {
            let state = init_state.clone();
            Box::pin(async move { advertise_local_agents(&state, context).await })
        },
        on_channel_message: move |message, context| {
            let state = channel_state.clone();
            Box::pin(async move { handle_channel_message(&state, message, context).await })
        },
        on_mesh_event: move |_event, context| {
            let state = mesh_state.clone();
            Box::pin(async move { advertise_local_agents(&state, context).await })
        },
    };

    Ok(plugin.on_open_stream(move |request, _context| {
        let state = open_stream_state.clone();
        Box::pin(async move {
            handle_open_stream(&state, request)
                .await
                .map(Some)
                .map_err(Into::into)
        })
    }))
}

async fn advertise_local_agents(
    state: &PluginState,
    context: &mut PluginContext<'_>,
) -> Result<()> {
    let agents = local_advertisements(&state.agents_dir, "")?;
    if agents.is_empty() {
        return Ok(());
    }
    let message = MeshProtocolMessage::Advertise { agents };
    context
        .send_json_channel(CHANNEL, "", KIND_ADVERTISE, &message)
        .await
}

async fn handle_channel_message(
    state: &PluginState,
    message: mesh_llm_plugin::proto::ChannelMessage,
    context: &mut PluginContext<'_>,
) -> Result<()> {
    if message.channel != CHANNEL {
        return Ok(());
    }
    let decoded: MeshProtocolMessage = serde_json::from_slice(&message.body)?;
    match decoded {
        MeshProtocolMessage::Advertise { agents } => {
            let mut cache = RemoteAgentCache::load(&state.data_dir)?;
            let source_peer_id = message.source_peer_id;
            let agents = agents
                .into_iter()
                .map(|mut agent| {
                    if agent.peer_id.is_empty() {
                        agent.peer_id = source_peer_id.clone();
                    }
                    agent
                })
                .collect();
            cache.upsert_many(agents);
            cache.save(&state.data_dir)
        }
        MeshProtocolMessage::SendMessageRequest(request) => {
            let response = execute_local_send_message(state, &request).await;
            let response = match response {
                Ok(result) => {
                    let task = result.get("task").unwrap_or(&result);
                    RemoteSendMessageResponse {
                        agent_id: request.agent_id,
                        task_id: task
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        result,
                        error: None,
                    }
                }
                Err(error) => RemoteSendMessageResponse {
                    agent_id: request.agent_id,
                    task_id: String::new(),
                    result: json!({}),
                    error: Some(error.to_string()),
                },
            };
            let reply = MeshProtocolMessage::SendMessageResponse(response);
            let message = mesh_llm_plugin::json_reply_channel_message(
                &message,
                KIND_SEND_MESSAGE_RESPONSE,
                &reply,
            )?;
            context.send_channel_message(message).await
        }
        MeshProtocolMessage::SendMessageResponse(response) => {
            if let Some(error) = response.error {
                bail!(
                    "remote agent `{}` returned an error: {error}",
                    response.agent_id
                );
            }
            let mut cache = RemoteTaskCache::load(&state.data_dir)?;
            cache.upsert(RemoteTaskRecord {
                agent_id: response.agent_id,
                task_id: response.task_id,
                peer_id: message.source_peer_id,
                correlation_id: message.correlation_id,
                result: response.result,
                updated_at_ms: now_ms(),
            });
            cache.save(&state.data_dir)
        }
    }
}

async fn plugin_get_agents(state: &PluginState, context: &mut PluginContext<'_>) -> Result<Value> {
    advertise_local_agents(state, context).await?;
    let mut agents = local_agent_summaries(&state.agents_dir)?;
    agents.extend(remote_agent_summaries(&state.data_dir)?);
    Ok(json!({ "agents": agents }))
}

fn plugin_get_agent(state: &PluginState, agent_id: &str) -> Result<Value> {
    if let Some(agent) = AgentRegistry::load_from_dir(&state.agents_dir)?.get(agent_id) {
        if !agent.runtime.enabled {
            bail!("agent `{agent_id}` is disabled");
        }
        return Ok(json!({
            "agent_id": agent.id,
            "location": "local",
            "agent_card": agent.card,
        }));
    }
    let remote = RemoteAgentCache::load(&state.data_dir)?
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

async fn plugin_send_message(
    state: &PluginState,
    args: SendMessageArgs,
    context: &mut PluginContext<'_>,
) -> Result<Value> {
    if let Some(agent) = AgentRegistry::load_from_dir(&state.agents_dir)?.get(&args.agent_id) {
        if !agent.runtime.enabled {
            bail!("agent `{}` is disabled", args.agent_id);
        }
        let _permit = state.gates.acquire(agent).await?;
        let result = execute_local_agent(
            agent.clone(),
            &state.data_dir,
            &args.message,
            args.context_id.as_deref(),
        )
        .await?;
        let task = result.get("task").unwrap_or(&result);
        return Ok(json!({
            "agent_id": args.agent_id,
            "location": "local",
            "task_id": task.get("id").and_then(Value::as_str),
            "context_id": task.get("contextId").and_then(Value::as_str),
            "result": result,
        }));
    }

    let remote = RemoteAgentCache::load(&state.data_dir)?
        .get(&args.agent_id)
        .cloned()
        .with_context(|| format!("agent `{}` was not found", args.agent_id))?;
    let stream_attempt = tokio::time::timeout(
        Duration::from_secs(10),
        send_remote_message_stream(state, &remote, &args, context),
    )
    .await;
    if let Ok(Ok(result)) = stream_attempt {
        return Ok(result);
    }

    let request = MeshProtocolMessage::SendMessageRequest(RemoteSendMessageRequest {
        agent_id: args.agent_id.clone(),
        message: args.message,
        context_id: args.context_id,
    });
    let correlation_id = format!("remote-{}", now_ms());
    let mut message = mesh_llm_plugin::json_channel_message(
        CHANNEL,
        remote.peer_id.clone(),
        KIND_SEND_MESSAGE_REQUEST,
        &request,
    )?;
    message.correlation_id = correlation_id.clone();
    context.send_channel_message(message).await?;
    let result = pending_remote_task_result(&args.agent_id, &correlation_id, &remote.peer_id);
    let mut cache = RemoteTaskCache::load(&state.data_dir)?;
    cache.upsert_pending(
        args.agent_id.clone(),
        remote.peer_id.clone(),
        correlation_id.clone(),
        result.clone(),
    );
    cache.save(&state.data_dir)?;
    Ok(json!({
        "agent_id": args.agent_id,
        "location": "remote",
        "peer_id": remote.peer_id,
        "task_id": correlation_id.clone(),
        "correlation_id": correlation_id,
        "status": "submitted",
        "result": result,
        "note": "task_id is a followable pending task reference; agents.get_task will resolve it to the final A2A task after the owner node replies",
    }))
}

async fn plugin_get_task(state: &PluginState, agent_id: &str, task_id: &str) -> Result<Value> {
    let path = mesh_agents_a2a::agent_task_store_path(&state.data_dir, agent_id);
    if path.exists() {
        let store = mesh_agents_a2a::PersistentTaskStore::open(&path)
            .map_err(|error| anyhow!("failed to open task store {}: {error}", path.display()))?;
        if let Some(task) = store.get(task_id).await? {
            return Ok(json!({ "agent_id": agent_id, "location": "local", "task": task }));
        }
    }
    let remote = RemoteTaskCache::load(&state.data_dir)?
        .get(agent_id, task_id)
        .cloned()
        .with_context(|| format!("task `{task_id}` was not found for agent `{agent_id}`"))?;
    Ok(json!({
        "agent_id": agent_id,
        "location": "remote",
        "peer_id": remote.peer_id,
        "task_id": remote.task_id,
        "correlation_id": remote.correlation_id,
        "task": remote.result,
    }))
}

async fn plugin_view_text_artifact(
    state: &PluginState,
    agent_id: &str,
    task_id: &str,
    artifact_id: &str,
) -> Result<Value> {
    let artifact = plugin_load_artifact(state, agent_id, task_id, artifact_id).await?;
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

async fn plugin_view_data_artifact(
    state: &PluginState,
    agent_id: &str,
    task_id: &str,
    artifact_id: &str,
) -> Result<Value> {
    let artifact = plugin_load_artifact(state, agent_id, task_id, artifact_id).await?;
    let data = artifact_data(&artifact)?;
    Ok(json!({
        "agent_id": agent_id,
        "task_id": task_id,
        "artifact_id": artifact.artifact_id,
        "data": data,
        "artifact": artifact,
    }))
}

async fn plugin_load_artifact(
    state: &PluginState,
    agent_id: &str,
    task_id: &str,
    artifact_id: &str,
) -> Result<Artifact> {
    let task = plugin_load_task(state, agent_id, task_id).await?;
    let artifacts = task.artifacts.unwrap_or_default();
    artifacts
        .into_iter()
        .find(|artifact| artifact.artifact_id == artifact_id)
        .with_context(|| format!("artifact `{artifact_id}` was not found on task `{task_id}`"))
}

async fn plugin_load_task(state: &PluginState, agent_id: &str, task_id: &str) -> Result<Task> {
    let path = mesh_agents_a2a::agent_task_store_path(&state.data_dir, agent_id);
    if path.exists() {
        let store = mesh_agents_a2a::PersistentTaskStore::open(&path)
            .map_err(|error| anyhow!("failed to open task store {}: {error}", path.display()))?;
        if let Some(task) = store.get(task_id).await? {
            return Ok(task);
        }
    }

    let remote = RemoteTaskCache::load(&state.data_dir)?
        .get(agent_id, task_id)
        .cloned()
        .with_context(|| format!("task `{task_id}` was not found for agent `{agent_id}`"))?;
    decode_task_value(remote.result)
        .with_context(|| format!("failed to decode remote task `{task_id}`"))
}

fn decode_task_value(value: Value) -> Result<Task> {
    if let Some(task) = value.get("task") {
        return serde_json::from_value(task.clone()).context("failed to decode wrapped task");
    }
    serde_json::from_value(value).context("failed to decode task")
}

fn artifact_data(artifact: &Artifact) -> Result<Value> {
    let data = artifact
        .parts
        .iter()
        .find_map(part_data)
        .with_context(|| format!("artifact `{}` has no data parts", artifact.artifact_id))?;
    Ok(data)
}

fn part_data(part: &Part) -> Option<Value> {
    let value = serde_json::to_value(part).ok()?;
    value.get("data").cloned()
}

fn pending_remote_task_result(agent_id: &str, correlation_id: &str, peer_id: &str) -> Value {
    json!({
        "task": {
            "id": correlation_id,
            "contextId": correlation_id,
            "status": {
                "state": "TASK_STATE_SUBMITTED",
            },
            "metadata": {
                "mesh_llm_pending_remote": true,
                "agent_id": agent_id,
                "peer_id": peer_id,
                "correlation_id": correlation_id,
            },
        },
    })
}

async fn send_remote_message_stream(
    state: &PluginState,
    remote: &crate::mesh::RemoteAgentAd,
    args: &SendMessageArgs,
    context: &mut PluginContext<'_>,
) -> Result<Value> {
    let stream_id = format!("a2a-{}-{}", args.agent_id, now_ms());
    let correlation_id = format!("remote-{}", now_ms());
    let metadata = MeshStreamRequest::SendMessage {
        agent_id: args.agent_id.clone(),
        message: args.message.clone(),
        context_id: args.context_id.clone(),
    };
    let mut stream = context
        .connect_mesh_stream(mesh_llm_plugin::proto::OpenMeshStreamRequest {
            stream_id,
            target_peer_id: remote.peer_id.clone(),
            plugin_id: String::new(),
            channel: CHANNEL.to_string(),
            purpose: mesh_llm_plugin::proto::StreamPurpose::Generic as i32,
            mode: mesh_llm_plugin::proto::StreamMode::EventStream as i32,
            bidirectional: false,
            content_type: Some("text/event-stream".to_string()),
            correlation_id: Some(correlation_id.clone()),
            metadata_json: Some(serde_json::to_string(&metadata)?),
            expected_bytes: None,
            idle_timeout_ms: Some(300_000),
        })
        .await?;

    let body = read_local_stream_to_end(&mut stream).await?;
    let events = parse_jsonrpc_sse_events(&body)?;
    let final_result = events
        .iter()
        .rev()
        .find_map(stream_event_task)
        .or_else(|| events.last().cloned())
        .context("remote A2A stream completed without events")?;
    let task_id = final_result
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if !task_id.is_empty() {
        let mut cache = RemoteTaskCache::load(&state.data_dir)?;
        cache.upsert(RemoteTaskRecord {
            agent_id: args.agent_id.clone(),
            task_id: task_id.clone(),
            peer_id: remote.peer_id.clone(),
            correlation_id: correlation_id.clone(),
            result: final_result.clone(),
            updated_at_ms: now_ms(),
        });
        cache.save(&state.data_dir)?;
    }

    Ok(json!({
        "agent_id": args.agent_id,
        "location": "remote",
        "peer_id": remote.peer_id,
        "transport": "mesh_stream",
        "task_id": task_id,
        "correlation_id": correlation_id,
        "result": final_result,
        "events": events,
    }))
}

async fn handle_open_stream(
    state: &PluginState,
    request: mesh_llm_plugin::proto::OpenStreamRequest,
) -> Result<mesh_llm_plugin::proto::OpenStreamResponse> {
    if request.mode != mesh_llm_plugin::proto::StreamMode::EventStream as i32 {
        bail!("agents only accepts event stream mesh streams");
    }
    let metadata_json = request
        .metadata_json
        .as_deref()
        .context("agents mesh stream request is missing metadata")?;
    let metadata: MeshStreamRequest = serde_json::from_str(metadata_json)?;
    let listener =
        mesh_llm_plugin::bind_side_stream(DEFAULT_PLUGIN_NAME, &request.stream_id).await?;
    let response = listener.open_stream_response(&request);
    let state = state.clone();
    tokio::spawn(async move {
        let result = async {
            let stream = listener.accept().await?;
            serve_mesh_stream_request(&state, metadata, stream).await
        }
        .await;
        if let Err(error) = result {
            eprintln!("agents mesh stream failed: {error}");
        }
    });
    Ok(response)
}

async fn serve_mesh_stream_request(
    state: &PluginState,
    request: MeshStreamRequest,
    stream: mesh_llm_plugin::LocalStream,
) -> Result<()> {
    match request {
        MeshStreamRequest::SendMessage {
            agent_id,
            message,
            context_id,
        } => {
            let agent = AgentRegistry::load_from_dir(&state.agents_dir)?
                .get(&agent_id)
                .cloned()
                .with_context(|| format!("agent `{agent_id}` was not found"))?;
            if !agent.runtime.enabled {
                bail!("agent `{agent_id}` is disabled");
            }
            let _permit = state.gates.acquire(&agent).await?;
            stream_local_agent_response(
                agent,
                &state.data_dir,
                &message,
                context_id.as_deref(),
                stream,
            )
            .await
        }
    }
}

async fn execute_local_send_message(
    state: &PluginState,
    request: &RemoteSendMessageRequest,
) -> Result<Value> {
    let agent = AgentRegistry::load_from_dir(&state.agents_dir)?
        .get(&request.agent_id)
        .cloned()
        .with_context(|| format!("agent `{}` was not found", request.agent_id))?;
    if !agent.runtime.enabled {
        bail!("agent `{}` is disabled", request.agent_id);
    }
    let _permit = state.gates.acquire(&agent).await?;
    execute_local_agent(
        agent,
        &state.data_dir,
        &request.message,
        request.context_id.as_deref(),
    )
    .await
}

async fn execute_local_agent(
    agent: AgentDefinition,
    data_dir: &Path,
    message: &str,
    context_id: Option<&str>,
) -> Result<Value> {
    let service = LocalAgentService::new(
        agent.clone(),
        data_dir,
        AcpAgentExecutor::new(agent, data_dir.to_path_buf()),
    )
    .map_err(|error| anyhow!("failed to create local A2A service: {error}"))?;
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
    response
        .result
        .context("A2A response did not include a result")
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
            "messageId": format!("mesh-{}", now_ms()),
            "role": "ROLE_USER",
            "parts": [{ "text": message }],
        }
    });
    if let Some(context_id) = context_id {
        params["contextId"] = Value::String(context_id.to_string());
    }
    JsonRpcRequest::new(JsonRpcId::Number(1), "SendMessage", Some(params))
}

fn send_streaming_message_request(message: &str, context_id: Option<&str>) -> JsonRpcRequest {
    let mut params = json!({
        "message": {
            "messageId": format!("mesh-stream-{}", now_ms()),
            "role": "ROLE_USER",
            "parts": [{ "text": message }],
        }
    });
    if let Some(context_id) = context_id {
        params["contextId"] = Value::String(context_id.to_string());
    }
    JsonRpcRequest::new(
        JsonRpcId::Number(1),
        jsonrpc_methods::SEND_STREAMING_MESSAGE,
        Some(params),
    )
}

async fn stream_local_agent_response(
    agent: AgentDefinition,
    data_dir: &Path,
    message: &str,
    context_id: Option<&str>,
    mut stream: mesh_llm_plugin::LocalStream,
) -> Result<()> {
    let service = LocalAgentService::new(
        agent.clone(),
        data_dir,
        AcpAgentExecutor::new(agent, data_dir.to_path_buf()),
    )
    .map_err(|error| anyhow!("failed to create local A2A service: {error}"))?;
    let body = serde_json::to_string(&send_streaming_message_request(message, context_id))?;
    let request = axum::http::Request::builder()
        .uri("/")
        .method("POST")
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(Body::from(body))?;
    let response = service.jsonrpc_router().oneshot(request).await?;
    let mut body = response.into_body();
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            stream.write_all_bytes(data).await?;
        }
    }
    Ok(())
}

async fn read_local_stream_to_end(stream: &mut mesh_llm_plugin::LocalStream) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    match stream {
        #[cfg(unix)]
        mesh_llm_plugin::LocalStream::Unix(stream) => {
            stream.read_to_end(&mut body).await?;
        }
        #[cfg(windows)]
        mesh_llm_plugin::LocalStream::PipeClient(stream) => {
            stream.read_to_end(&mut body).await?;
        }
        #[cfg(windows)]
        mesh_llm_plugin::LocalStream::PipeServer(stream) => {
            stream.read_to_end(&mut body).await?;
        }
    }
    Ok(body)
}

fn parse_jsonrpc_sse_events(body: &[u8]) -> Result<Vec<Value>> {
    let text = String::from_utf8_lossy(body);
    let mut events = Vec::new();
    let mut data = String::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            flush_sse_event(&mut events, &mut data)?;
            continue;
        }
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }
    flush_sse_event(&mut events, &mut data)?;
    Ok(events)
}

fn flush_sse_event(events: &mut Vec<Value>, data: &mut String) -> Result<()> {
    if data.trim().is_empty() {
        data.clear();
        return Ok(());
    }
    let response: JsonRpcResponse = serde_json::from_str(data)?;
    if let Some(error) = response.error {
        bail!("remote A2A stream error {}: {}", error.code, error.message);
    }
    if let Some(result) = response.result {
        events.push(result);
    }
    data.clear();
    Ok(())
}

fn stream_event_task(event: &Value) -> Option<Value> {
    event.get("task").cloned().or_else(|| {
        event
            .get("result")
            .and_then(|value| value.get("task"))
            .cloned()
    })
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

fn resolve_agents_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".mesh-llm").join("agents"))
}

fn resolve_data_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join(".mesh-llm"))
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("could not determine home directory")
}

#[cfg(test)]
mod tests {
    use mesh_llm_plugin::Plugin;

    use super::*;

    #[test]
    fn manifest_advertises_agents_mcp_stdio_endpoint_and_mesh_channel() {
        let plugin = build_plugin_with_command(
            "agents".to_string(),
            "agents".to_string(),
            PluginState {
                agents_dir: PathBuf::from("/tmp/agents"),
                data_dir: PathBuf::from("/tmp/mesh"),
                gates: AgentGates::default(),
            },
        )
        .expect("build plugin");
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
        assert_eq!(manifest.mesh_channels[0].name, CHANNEL);
        assert!(manifest.operations.iter().any(|op| op.name == "get_agents"));
        assert!(manifest
            .operations
            .iter()
            .any(|op| op.name == "send_message"));
        assert!(manifest
            .operations
            .iter()
            .any(|op| op.name == "view_text_artifact"));
        assert!(manifest
            .operations
            .iter()
            .any(|op| op.name == "view_data_artifact"));
    }

    fn command_basename(command: &str) -> &str {
        command.rsplit('/').next().unwrap_or(command)
    }
}
