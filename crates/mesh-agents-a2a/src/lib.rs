//! A2A agent registry and protocol boundary types for mesh-llm.
//!
//! This crate intentionally depends on the official A2A Rust crates. Local
//! mesh-llm types can wrap those crates, but protocol types and wire-facing
//! behavior should stay anchored to the upstream SDK.

mod registry;
mod service;
mod task_store;

pub use a2a::{
    A2AError, AgentCard, Artifact, JsonRpcId, JsonRpcRequest, JsonRpcResponse, ListTasksRequest,
    ListTasksResponse, Message, Part, Role, SendMessageResponse, StreamResponse, Task, TaskState,
    TaskStatus,
};
pub use a2a_client::{A2AClient, A2AClientFactory, Transport, TransportFactory};
pub use a2a_server::{
    AgentExecutor, DefaultRequestHandler, ExecutorContext, RequestHandler, TaskStore,
};
pub use registry::{
    AgentDefinition, AgentRegistry, AgentRuntimeConfig, InstructionDelivery, InstructionsConfig,
    MeshToolsConfig, QueueConfig, QueueMode, RuntimeConfig, RuntimeKind, ToolConfig, ToolKind,
    ToolsConfig, Visibility, WorkspaceConfig, WorkspaceKeep, WorkspaceMode,
};
pub use service::{
    agent_task_store_path, local_jsonrpc_router, local_rest_router, EchoAgentExecutor,
    LocalAgentService,
};
pub use task_store::PersistentTaskStore;
