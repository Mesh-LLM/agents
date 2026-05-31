use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use mesh_agents_a2a::{AgentCard, AgentDefinition, AgentRegistry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(crate) const CHANNEL: &str = "agents.discovery.v1";
pub(crate) const KIND_ADVERTISE: &str = "advertise";
pub(crate) const KIND_SEND_MESSAGE_REQUEST: &str = "send_message_request";
pub(crate) const KIND_SEND_MESSAGE_RESPONSE: &str = "send_message_response";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum MeshProtocolMessage {
    Advertise { agents: Vec<RemoteAgentAd> },
    SendMessageRequest(RemoteSendMessageRequest),
    SendMessageResponse(RemoteSendMessageResponse),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RemoteAgentAd {
    pub agent_id: String,
    pub peer_id: String,
    pub name: String,
    pub description: Option<String>,
    pub version: String,
    pub card: AgentCard,
    pub updated_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RemoteSendMessageRequest {
    pub agent_id: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RemoteSendMessageResponse {
    pub agent_id: String,
    pub task_id: String,
    pub result: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct RemoteAgentCache {
    pub agents: Vec<RemoteAgentAd>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct RemoteTaskCache {
    pub tasks: Vec<RemoteTaskRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct RemoteTaskRecord {
    pub agent_id: String,
    pub task_id: String,
    pub peer_id: String,
    pub correlation_id: String,
    pub result: Value,
    pub updated_at_ms: u64,
}

impl RemoteAgentCache {
    pub(crate) fn load(data_dir: &Path) -> Result<Self> {
        let path = cache_path(data_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub(crate) fn save(&self, data_dir: &Path) -> Result<()> {
        let path = cache_path(data_dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let raw = serde_json::to_vec_pretty(self)?;
        fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))
    }

    pub(crate) fn upsert_many(&mut self, agents: Vec<RemoteAgentAd>) {
        for agent in agents {
            self.upsert(agent);
        }
        self.agents.sort_by(|left, right| {
            left.agent_id
                .cmp(&right.agent_id)
                .then(left.peer_id.cmp(&right.peer_id))
        });
    }

    fn upsert(&mut self, agent: RemoteAgentAd) {
        if let Some(existing) = self.agents.iter_mut().find(|existing| {
            existing.agent_id == agent.agent_id && existing.peer_id == agent.peer_id
        }) {
            *existing = agent;
        } else {
            self.agents.push(agent);
        }
    }

    pub(crate) fn get(&self, agent_id: &str) -> Option<&RemoteAgentAd> {
        self.agents.iter().find(|agent| agent.agent_id == agent_id)
    }
}

impl RemoteTaskCache {
    pub(crate) fn load(data_dir: &Path) -> Result<Self> {
        let path = task_cache_path(data_dir);
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("failed to parse {}", path.display()))
    }

    pub(crate) fn save(&self, data_dir: &Path) -> Result<()> {
        let path = task_cache_path(data_dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let raw = serde_json::to_vec_pretty(self)?;
        fs::write(&path, raw).with_context(|| format!("failed to write {}", path.display()))
    }

    pub(crate) fn upsert(&mut self, task: RemoteTaskRecord) {
        if let Some(existing) = self.tasks.iter_mut().find(|existing| {
            existing.agent_id == task.agent_id
                && (existing.task_id == task.task_id
                    || same_nonempty(&existing.correlation_id, &task.correlation_id))
        }) {
            *existing = task;
        } else {
            self.tasks.push(task);
        }
    }

    pub(crate) fn upsert_pending(
        &mut self,
        agent_id: String,
        peer_id: String,
        correlation_id: String,
        result: Value,
    ) {
        self.upsert(RemoteTaskRecord {
            agent_id,
            task_id: correlation_id.clone(),
            peer_id,
            correlation_id,
            result,
            updated_at_ms: now_ms(),
        });
    }

    pub(crate) fn get(&self, agent_id: &str, task_id: &str) -> Option<&RemoteTaskRecord> {
        self.tasks.iter().find(|task| {
            task.agent_id == agent_id
                && (task.task_id == task_id || same_nonempty(&task.correlation_id, task_id))
        })
    }
}

fn same_nonempty(left: &str, right: &str) -> bool {
    !left.is_empty() && left == right
}

pub(crate) fn local_advertisements(
    agents_dir: &Path,
    source_peer_id: &str,
) -> Result<Vec<RemoteAgentAd>> {
    let registry = AgentRegistry::load_from_dir(agents_dir)?;
    Ok(registry
        .agents()
        .iter()
        .filter(|agent| agent.runtime.enabled && agent.runtime.policy.advertise_on_mesh)
        .map(|agent| local_advertisement(agent, source_peer_id))
        .collect())
}

pub(crate) fn local_agent_summaries(agents_dir: &Path) -> Result<Vec<Value>> {
    let registry = AgentRegistry::load_from_dir(agents_dir)?;
    Ok(registry
        .agents()
        .iter()
        .filter(|agent| agent.runtime.enabled)
        .map(|agent| {
            json!({
                "agent_id": agent.id,
                "name": agent.card.name,
                "description": agent.card.description,
                "version": agent.card.version,
                "runtime": agent.runtime.runtime.kind,
                "max_concurrent_tasks": agent.runtime.runtime.max_concurrent_tasks,
                "card_url": format!("mesh://agents/{}", agent.id),
                "location": "local",
            })
        })
        .collect())
}

pub(crate) fn remote_agent_summaries(data_dir: &Path) -> Result<Vec<Value>> {
    Ok(RemoteAgentCache::load(data_dir)?
        .agents
        .into_iter()
        .map(|agent| {
            json!({
                "agent_id": agent.agent_id,
                "name": agent.name,
                "description": agent.description,
                "version": agent.version,
                "card_url": format!("mesh://agents/{}/{}", agent.peer_id, agent.agent_id),
                "location": "remote",
                "peer_id": agent.peer_id,
            })
        })
        .collect())
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

fn local_advertisement(agent: &AgentDefinition, source_peer_id: &str) -> RemoteAgentAd {
    RemoteAgentAd {
        agent_id: agent.id.clone(),
        peer_id: source_peer_id.to_string(),
        name: agent.card.name.clone(),
        description: Some(agent.card.description.clone()),
        version: agent.card.version.clone(),
        card: agent.card.clone(),
        updated_at_ms: now_ms(),
    }
}

fn cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("a2a").join("remote-agents.json")
}

fn task_cache_path(data_dir: &Path) -> PathBuf {
    data_dir.join("a2a").join("remote-tasks.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("mesh-agents-mesh-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create temp root");
        root
    }

    fn write_agent(root: &Path, id: &str, advertise_on_mesh: bool) {
        let agent_dir = root.join(id);
        fs::create_dir_all(&agent_dir).expect("create agent dir");
        fs::write(
            agent_dir.join("agent-card.json"),
            format!(
                r#"{{
                  "name": "{id}",
                  "description": "Test agent {id}.",
                  "version": "1.0.0",
                  "supportedInterfaces": [
                    {{
                      "url": "http://localhost:3131/a2a/agents/{id}",
                      "protocolBinding": "JSONRPC",
                      "protocolVersion": "1.0"
                    }}
                  ],
                  "capabilities": {{ "streaming": true }},
                  "defaultInputModes": ["text/plain"],
                  "defaultOutputModes": ["text/markdown"],
                  "skills": []
                }}"#
            ),
        )
        .expect("write agent card");
        fs::write(
            agent_dir.join("runtime.toml"),
            format!(
                r#"
enabled = true
visibility = "private"

[runtime]
type = "opencode"
max_concurrent_tasks = 1

[runtime.workspace]
mode = "temp_per_task"

[tools.mesh]
enabled = true

[policy]
advertise_on_mesh = {advertise_on_mesh}
public_mesh = false
"#
            ),
        )
        .expect("write runtime");
    }

    #[test]
    fn local_advertisements_include_only_agents_opted_into_mesh_advertising() {
        let root = temp_root("advertise-policy");
        write_agent(&root, "published", true);
        write_agent(&root, "private", false);

        let ads = local_advertisements(&root, "peer-a").expect("load advertisements");

        assert_eq!(ads.len(), 1);
        assert_eq!(ads[0].agent_id, "published");
        assert_eq!(ads[0].peer_id, "peer-a");
    }

    #[test]
    fn local_agent_summaries_still_show_private_local_agents() {
        let root = temp_root("local-summary-private");
        write_agent(&root, "private", false);

        let summaries = local_agent_summaries(&root).expect("load summaries");

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0]["agent_id"], "private");
        assert_eq!(summaries[0]["location"], "local");
    }

    #[test]
    fn remote_tasks_can_be_read_by_correlation_id() {
        let mut cache = RemoteTaskCache::default();
        cache.upsert(RemoteTaskRecord {
            agent_id: "pr-review".to_string(),
            task_id: "task-1".to_string(),
            peer_id: "peer-a".to_string(),
            correlation_id: "remote-1".to_string(),
            result: json!({ "task": { "id": "task-1" } }),
            updated_at_ms: 1,
        });

        let task = cache
            .get("pr-review", "remote-1")
            .expect("correlation id should resolve");

        assert_eq!(task.task_id, "task-1");
    }

    #[test]
    fn remote_task_response_replaces_pending_correlation_record() {
        let mut cache = RemoteTaskCache::default();
        cache.upsert_pending(
            "pr-review".to_string(),
            "peer-a".to_string(),
            "remote-1".to_string(),
            json!({ "task": { "id": "remote-1" } }),
        );

        cache.upsert(RemoteTaskRecord {
            agent_id: "pr-review".to_string(),
            task_id: "task-1".to_string(),
            peer_id: "peer-a".to_string(),
            correlation_id: "remote-1".to_string(),
            result: json!({ "task": { "id": "task-1" } }),
            updated_at_ms: 2,
        });

        assert_eq!(cache.tasks.len(), 1);
        assert_eq!(
            cache
                .get("pr-review", "remote-1")
                .expect("correlation id should resolve to completed task")
                .task_id,
            "task-1"
        );
        assert_eq!(
            cache
                .get("pr-review", "task-1")
                .expect("task id should resolve to completed task")
                .correlation_id,
            "remote-1"
        );
    }
}
