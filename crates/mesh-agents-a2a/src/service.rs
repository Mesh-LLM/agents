use std::path::{Path, PathBuf};
use std::sync::Arc;

use a2a::{A2AError, Message, Part, Role, StreamResponse, Task, TaskState, TaskStatus};
use a2a_server::DefaultRequestHandler;
use futures::{stream, stream::BoxStream};

use crate::{AgentDefinition, AgentExecutor, PersistentTaskStore, TaskStore};

pub struct LocalAgentService {
    pub agent: AgentDefinition,
    pub handler: Arc<DefaultRequestHandler>,
    pub task_store_path: PathBuf,
}

impl LocalAgentService {
    pub fn new(
        agent: AgentDefinition,
        data_dir: impl AsRef<Path>,
        executor: impl AgentExecutor,
    ) -> Result<Self, A2AError> {
        let task_store_path = agent_task_store_path(data_dir, &agent.id);
        let task_store = PersistentTaskStore::open(&task_store_path)?;
        let handler = Arc::new(DefaultRequestHandler::new(executor, task_store));
        Ok(Self {
            agent,
            handler,
            task_store_path,
        })
    }

    #[must_use]
    pub fn jsonrpc_router(&self) -> axum::Router {
        a2a_server::jsonrpc::jsonrpc_router(self.handler.clone())
    }

    #[must_use]
    pub fn rest_router(&self) -> axum::Router {
        a2a_server::rest::rest_router(self.handler.clone())
    }
}

pub fn agent_task_store_path(data_dir: impl AsRef<Path>, agent_id: &str) -> PathBuf {
    data_dir
        .as_ref()
        .join("a2a")
        .join("agents")
        .join(agent_id)
        .join("tasks.sqlite")
}

pub fn local_jsonrpc_router(
    executor: impl AgentExecutor,
    task_store: impl TaskStore,
) -> axum::Router {
    let handler = Arc::new(DefaultRequestHandler::new(executor, task_store));
    a2a_server::jsonrpc::jsonrpc_router(handler)
}

pub fn local_rest_router(executor: impl AgentExecutor, task_store: impl TaskStore) -> axum::Router {
    let handler = Arc::new(DefaultRequestHandler::new(executor, task_store));
    a2a_server::rest::rest_router(handler)
}

#[derive(Clone, Debug, Default)]
pub struct EchoAgentExecutor;

impl AgentExecutor for EchoAgentExecutor {
    fn execute(
        &self,
        ctx: a2a_server::ExecutorContext,
    ) -> BoxStream<'static, Result<StreamResponse, A2AError>> {
        let response = Message::new(
            Role::Agent,
            vec![Part::text(format!(
                "Echo from mesh-llm A2A test executor: {}",
                ctx.message
                    .as_ref()
                    .and_then(Message::text)
                    .unwrap_or_default()
            ))],
        );
        let task = Task {
            id: ctx.task_id,
            context_id: ctx.context_id,
            status: TaskStatus {
                state: TaskState::Completed,
                message: Some(response),
                timestamp: None,
            },
            artifacts: None,
            history: None,
            metadata: None,
        };
        Box::pin(stream::once(async move { Ok(StreamResponse::Task(task)) }))
    }

    fn cancel(
        &self,
        ctx: a2a_server::ExecutorContext,
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

#[cfg(test)]
mod tests {
    use super::*;
    use a2a::{JsonRpcId, JsonRpcRequest, JsonRpcResponse, SendMessageResponse};
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[test]
    fn builds_per_agent_sqlite_path() {
        let path = agent_task_store_path("/tmp/mesh-data", "pr-review");

        assert_eq!(
            path,
            PathBuf::from("/tmp/mesh-data/a2a/agents/pr-review/tasks.sqlite")
        );
    }

    #[tokio::test]
    async fn jsonrpc_router_completes_and_persists_echo_task() {
        let path = temp_store_path("jsonrpc");
        let store = PersistentTaskStore::open(&path).unwrap();
        let app = local_jsonrpc_router(EchoAgentExecutor, store);

        let response = post_jsonrpc(
            app,
            "SendMessage",
            serde_json::json!({
                "message": {
                    "messageId": "m1",
                    "role": "ROLE_USER",
                    "parts": [{"text": "review this"}]
                }
            }),
        )
        .await;

        assert!(
            response.error.is_none(),
            "unexpected error: {:?}",
            response.error
        );
        let result: SendMessageResponse =
            serde_json::from_value(response.result.expect("jsonrpc result")).unwrap();
        let SendMessageResponse::Task(task) = result else {
            panic!("expected task response");
        };
        assert_eq!(task.status.state, TaskState::Completed);

        let reopened = PersistentTaskStore::open(&path).unwrap();
        let persisted = reopened.get(&task.id).await.unwrap().unwrap();
        assert_eq!(persisted.status.state, TaskState::Completed);
    }

    async fn post_jsonrpc(
        app: axum::Router,
        method: &str,
        params: serde_json::Value,
    ) -> JsonRpcResponse {
        let request = JsonRpcRequest::new(JsonRpcId::Number(1), method, Some(params));
        let body = serde_json::to_string(&request).unwrap();
        let request = axum::http::Request::builder()
            .uri("/")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    fn temp_store_path(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "mesh-agents-a2a-service-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root.join("tasks.sqlite")
    }
}
