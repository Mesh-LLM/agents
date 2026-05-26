# A2A Agents Over Mesh

This repository is the extracted home for mesh-llm's mesh-native agent system.
It owns the A2A registry/server, ACP bridge, generic Codex skill setup, and the
`mesh-agents` CLI. mesh-llm should consume it as an external plugin/command
surface instead of carrying the agent implementation in the core workspace.

This document sketches the Agent2Agent (A2A) integration for mesh-llm. It is
part design note and part implementation tracker; the first local A2A MCP slice
exists, while mesh-routed discovery and full remote A2A are still planned.

The goal is to make mesh-llm a mesh-native substrate for discovering and
calling A2A agents while keeping the execution harness pluggable. In the first
useful shape, Codex or OpenCode calls mesh-llm MCP tools, mesh-llm uses official
A2A client/server semantics, and local coding agents run behind an A2A/ACP
bridge.

## Protocol Roles

The protocols should have clear ownership boundaries:

| Protocol | Role in mesh-llm |
|---|---|
| A2A | Agent discovery, task submission, task state, streaming, artifacts, remote agent delegation |
| ACP | Local coding-agent harness control, especially for agents like OpenCode |
| MCP | Tool surface exposed to Codex, OpenCode, Claude, and other clients |

The intended flow is:

```text
Codex / OpenCode / Claude
  -> mesh-llm MCP tools
  -> official A2A client calls
  -> mesh-discovered A2A agent
  -> A2A/ACP bridge
  -> ACP agent process
  -> mesh inference and agent-local tools
```

A2A agents advertise skills and capabilities. They should not expose their
internal tools, prompts, ACP details, or model-routing implementation.

## Required Rust Crates

The implementation must use the official Rust crates for the protocol
boundaries:

| Protocol | Required crate/source | Use |
|---|---|---|
| A2A | `a2aproject/a2a-rs` | Agent Card types, A2A client, A2A server bindings, task/message/artifact types, streaming support |
| ACP | `agent-client-protocol` | ACP client implementation for local harnesses such as OpenCode |

These are required dependencies, not optional references. mesh-llm may wrap
them with local domain types where useful, but protocol parsing, request/response
types, and wire behavior should come from the official crates.

If either crate is temporarily missing a required feature, the implementation
should document the blocker and isolate any compatibility shim behind a narrow
module with tests. A shim should be treated as transitional and removed once the
official crate covers the needed surface.

## Workspace Crates

The A2A agent host should live in its own workspace-owned crates inside the
existing mesh-llm workspace. The host-runtime crate should wire those crates
into CLI, HTTP, MCP, and mesh lifecycle, but it should not absorb the protocol
or agent-runtime implementation.

Proposed crates:

```text
crates/mesh-agents-a2a/
  A2A domain types, agent directory loading, Agent Card validation, local and
  remote registries, task model, artifact model, and official A2A client/server
  integration.

crates/mesh-agents-acp-bridge/
  ACP client adapter, OpenCode runtime expansion, ACP session lifecycle, permission
  and event mapping, workspace setup, and A2A task-to-ACP run translation.

crates/mesh-agents-skills/
  Skill discovery, validation, install/uninstall/status, and target-specific
  install behavior such as Codex skills.
```

Ownership boundaries:

| Crate | Owns | Does not own |
|---|---|---|
| `mesh-agents-a2a` | Agent directory model, A2A server/client adapters, mesh agent registry, task/artifact state | CLI parsing, harness-specific ACP behavior |
| `mesh-agents-acp-bridge` | ACP harness execution and event translation | A2A HTTP routing, mesh discovery |
| `mesh-agents-skills` | Skill filesystem behavior and target installers | A2A task execution |
| `mesh-llm-host-runtime` | CLI commands, config loading integration, HTTP/MCP projection, mesh lifecycle hooks | Protocol internals and harness internals |

The first implementation can keep these crates small. They should exist to keep
protocol/runtime boundaries honest, not to create premature abstraction.

## Goals

- Support full A2A semantics for local and remote agents.
- Use the official Rust crates for both A2A and ACP. This is a hard
  requirement.
- Discover A2A agents across a mesh.
- Expose MCP tools for finding and calling A2A agents.
- Let Codex, OpenCode, and similar clients use those MCP tools to talk to agents
  over A2A.
- Support local ACP-backed agents, with OpenCode as the first practical harness.
- Keep agent identity and A2A cards in native A2A JSON.
- Keep runtime policy, local commands, environment, and workspace behavior out
  of A2A cards.
- Make agents available to clients through the mesh A2A MCP tools. Do not
  require per-agent client installation.

## Non-Goals

- Do not make ACP the mesh-wide agent protocol.
- Do not make A2A the tool protocol; MCP remains the tool-facing protocol.
- Do not hand-roll A2A or ACP protocol implementations when official Rust
  crates cover the needed surface.
- Do not require users to understand ACP when configuring an OpenCode-backed agent.
- Do not gossip full Agent Cards when compact summaries are enough.
- Do not expose raw local tools through A2A Agent Cards.

## Agent Directory

Agents live in directory form only:

```text
~/.mesh-llm/
  config.toml
  agents/
    pr-review/
      agent-card.json
      runtime.toml
      instructions.md
      skill/
        SKILL.md
        scripts/
        references/
```

Rules:

- Each immediate child directory under `~/.mesh-llm/agents/` is one agent.
- The directory name is the local `agent_id`.
- `agent-card.json` is required and must be native A2A Agent Card JSON.
- `runtime.toml` is required and contains mesh-llm-specific execution policy.
- Loose `*.json` cards directly under `agents/` are not supported.
- Remote A2A agents also use directory form, with `runtime.type = "remote"`.
- Optional Codex skills live under `skill/`.

The global mesh-llm config only enables the subsystem and sets defaults:

```toml
# ~/.mesh-llm/config.toml

[a2a]
enabled = true
agents_dir = "~/.mesh-llm/agents"
mcp_enabled = true
mesh_discovery = true
default_visibility = "private"
```

## Agent Card

`agent-card.json` should stay as close as possible to the A2A spec:

```json
{
  "name": "PR Review Agent",
  "description": "Reviews GitHub pull requests for correctness bugs, regressions, and missing tests.",
  "version": "1.0.0",
  "supportedInterfaces": [
    {
      "url": "http://localhost:3131/a2a/agents/pr-review",
      "protocolBinding": "JSONRPC",
      "protocolVersion": "1.0"
    }
  ],
  "capabilities": {
    "streaming": true,
    "pushNotifications": false
  },
  "defaultInputModes": ["text/plain", "application/json"],
  "defaultOutputModes": ["text/markdown", "application/json"],
  "skills": [
    {
      "id": "github_pr_review",
      "name": "GitHub PR review",
      "description": "Review a pull request and return structured findings.",
      "tags": ["github", "review"]
    }
  ]
}
```

The served card may need runtime normalization. For example, mesh-llm should
validate or rewrite the `supportedInterfaces` URLs when exposing the card
through a different local port, mesh route, or public endpoint.

## Runtime Config

`runtime.toml` is private local configuration. It should not be copied into
mesh advertisements or shared Agent Cards.

Example OpenCode-backed local agent:

```toml
enabled = true
visibility = "private"

[runtime]
type = "opencode"
model = "mesh"
mode = "smart_approve"
session_policy = "per_task"
max_concurrent_tasks = 1

[runtime.workspace]
mode = "temp_per_task"
prefix = "mesh-pr-review-"
keep = "on_failure"

[instructions]
file = "instructions.md"
delivery = "first_prompt"

[[tools.extra]]
name = "github"
type = "mcp"
command = "github-mcp-server"
env_keys = ["GITHUB_PERSONAL_ACCESS_TOKEN"]
available_tools = [
  "get_pull_request",
  "get_pull_request_diff",
  "create_review_comment"
]

[[tools.extra]]
name = "developer"
type = "mcp"
available_tools = ["shell", "text_editor"]

[policy]
approval = "prompt"
filesystem = "workspace"
network = "allow"
max_task_seconds = 1800
advertise_on_mesh = true
public_mesh = false
```

The simple `type = "opencode"` runtime is user-facing. mesh-llm expands it
internally to an ACP subprocess, normally `opencode acp`.

OpenCode's ACP command communicates over stdin/stdout using newline-delimited
JSON, which matches the official `agent-client-protocol` client transport.

Advanced users can bypass the OpenCode-specific shorthand:

```toml
enabled = true
visibility = "private"

[runtime]
type = "acp"
command = "opencode"
args = ["acp"]
session_policy = "per_task"
max_concurrent_tasks = 1

[runtime.env]
OPENCODE_CONFIG = "/path/to/opencode.json"
```

Example remote A2A agent:

```toml
enabled = true
visibility = "private"

[runtime]
type = "remote"
card_url = "https://agents.example.com/research/.well-known/agent-card.json"

[auth]
bearer_token_env = "RESEARCH_A2A_TOKEN"
```

## Concurrency

Each agent can set its own task concurrency limit:

```toml
[runtime]
type = "opencode"
max_concurrent_tasks = 1
```

The default is `1`. That is the right default for ACP-backed coding agents
because they usually have mutable session state, workspace state, tool
permissions, and command execution side effects.

When an agent is at its concurrency limit, mesh-llm should either queue or
reject new tasks according to policy:

```toml
[runtime.queue]
mode = "queue"
max_pending_tasks = 16
```

Supported queue modes:

| Mode | Behavior |
|---|---|
| `queue` | Accept the task and hold it in `submitted` or `working` once a slot opens. |
| `reject` | Return an A2A busy/capacity error immediately. |

Initial defaults:

```toml
[runtime]
max_concurrent_tasks = 1

[runtime.queue]
mode = "queue"
max_pending_tasks = 16
```

Remote A2A agents may expose their own capacity separately. For remote agents,
mesh-llm should treat local concurrency limits as client-side throttles, not as
authoritative remote capacity.

## Workspaces

Agent tasks need a working directory policy. The first implementation should
support fixed paths and task-scoped temp directories:

```toml
[runtime.workspace]
mode = "path"
path = "/Users/jdumay/src/mesh-llm"
```

```toml
[runtime.workspace]
mode = "temp_per_task"
prefix = "mesh-agent-task-"
keep = "on_failure"
```

Supported modes:

| Mode | Behavior |
|---|---|
| `path` | Use a fixed configured directory. |
| `temp_per_task` | Create a fresh temp directory for every A2A task. |
| `agent_dir` | Use the agent directory as the working directory. |
| `none` | Start without setting a working directory. |

Supported temp retention values:

| Value | Behavior |
|---|---|
| `never` | Delete temp workspace after task completion. |
| `on_failure` | Keep temp workspace only when the task fails. |
| `always` | Keep temp workspace for inspection. |

Avoid bootstrap command templating in the first implementation. The agent can
clone repositories or prepare files itself from a clean temp workspace. mesh-llm
should record the resolved workspace path in task metadata.

## A2A/ACP Bridge

For ACP-backed local agents, mesh-llm is the A2A server and the ACP client.
The ACP client side must use the official `agent-client-protocol` Rust crate.

```text
A2A message/send
  -> create mesh-llm task
  -> create workspace
  -> start or reuse ACP process
  -> create ACP session with cwd and tool servers
  -> send prompt turn
  -> translate ACP updates to A2A task events
  -> publish final artifacts
```

Mapping:

| A2A | ACP-backed implementation |
|---|---|
| Agent Card | `agent-card.json` plus runtime-normalized endpoint URL |
| Task | ACP session turn or run |
| Message | User prompt and agent response content |
| Artifact | Final response, structured output, diffs, files, logs |
| Streaming | ACP notifications converted to A2A streaming events |
| Cancel | ACP session/run cancellation |
| Input required | ACP permission/input request |

Permission requests from the ACP agent should move the A2A task to
`input-required` unless local policy has already approved that operation.

## Instructions

Per-agent behavior should live outside the Agent Card:

```text
~/.mesh-llm/agents/pr-review/instructions.md
```

`runtime.toml` selects how instructions are delivered:

```toml
[instructions]
file = "instructions.md"
delivery = "first_prompt"
```

Delivery modes:

| Mode | Behavior |
|---|---|
| `first_prompt` | Prefix the first ACP prompt with the instructions. |
| `top_of_mind` | Use harness-specific persistent instruction support when available. |
| `none` | Do not inject mesh-llm-managed instructions. |

For harnesses with persistent instruction support, `top_of_mind` may map to
their native mechanism. The default should be `first_prompt` because it is
explicit and portable.

## Tool Configuration

Users configure agent tools in `runtime.toml`. They should not have to know
that mesh-llm passes MCP servers through ACP session creation.

Every local agent should automatically receive mesh-llm's own MCP server tools
unless explicitly disabled. This gives agents a built-in way to discover and
delegate to other mesh A2A agents without requiring every agent config to repeat
the same MCP server stanza.

```toml
[tools.mesh]
enabled = true
available_tools = [
  "get_agents",
  "get_agent",
  "send_message",
  "get_task",
  "view_text_artifact",
  "view_data_artifact"
]
```

Users can narrow or disable those defaults per agent:

```toml
[tools.mesh]
enabled = false
```

```toml
[[tools.extra]]
name = "github"
type = "mcp"
command = "github-mcp-server"
args = []
timeout_secs = 60
env_keys = ["GITHUB_PERSONAL_ACCESS_TOKEN"]
available_tools = [
  "get_pull_request",
  "get_pull_request_diff",
  "create_review_comment"
]
```

For an OpenCode-backed agent, mesh-llm translates this into the MCP server
config OpenCode receives over ACP. For a future non-ACP runtime, the same tool
policy can be translated to that runtime's native tool wiring.

Tools are implementation details. A2A discovery should expose skills such as
`github_pr_review`, not raw tools such as `shell` or `get_pull_request_diff`.

## A2A Server Surface

Each local agent gets a stable A2A endpoint:

```text
/a2a/agents/{agent_id}
/a2a/agents/{agent_id}/.well-known/agent-card.json
```

The implementation must use the official A2A Rust SDK for protocol types,
client behavior, server bindings, and streaming semantics. The initial
transport should support JSON-RPC or REST request/response operations plus SSE
streaming for `message/stream`. gRPC support can follow if needed for full
transport parity.

Required behavior:

- Agent Card discovery.
- Message send.
- Message stream.
- Task get/list/cancel.
- Artifact retrieval.
- Push notification compatibility can be deferred when webhook-style callbacks
  are needed.
- Auth metadata and per-agent auth policy.
- A2A-compliant error handling.

## MCP Tools

mesh-llm exposes an MCP tool surface from `mesh-llm a2a mcp` that closely
matches existing A2A MCP bridges. The MCP server is a practical client adapter
for Codex, OpenCode, Claude, and similar tools; it is not an administrative
surface.

The MCP server currently backs the tools with the local agent-card registry and
local per-agent A2A service/task store. Mesh-discovered remote agents should
join the same tool surface later without adding new MCP tool names.

| Tool | Purpose |
|---|---|
| `get_agents` | Get enabled local agents, and later mesh-discovered agents. |
| `get_agent` | Get one enabled agent's native Agent Card plus runtime summary. |
| `send_message` | Send a message to an agent through local A2A JSON-RPC. |
| `get_task` | Fetch one persisted task from the per-agent SQLite task store. |
| `view_text_artifact` | Read text parts from a persisted task artifact. |
| `view_data_artifact` | Read the structured artifact object from a persisted task. |

This intentionally mirrors the six-tool bridge surface used by
`a2anet/a2a-mcp`: `get_agents`, `get_agent`, `send_message`, `get_task`,
`view_text_artifact`, and `view_data_artifact`. The mesh-llm difference is
behind the tools: `get_agents` searches the local and mesh-discovered agent
registry instead of only a static list of configured Agent Card URLs.

Full A2A support still belongs in the A2A server/client implementation. It does
not require exposing every A2A operation as an MCP tool. Task cancellation, task
listing, push notification configuration, and streaming can be available through
the A2A protocol and CLI where needed, but they should not be part of the
assistant-facing MCP surface.

Responses should be structured JSON. Large artifacts should be minimized in
`send_message` and `get_task` results, then navigated through
`view_text_artifact` or `view_data_artifact`.

## Task Persistence

A2A task state should be persistent in v1. The accepting node owns task state
and should be able to recover enough data after a mesh-llm restart for clients
to inspect completed, failed, canceled, or interrupted tasks.

Persisted task data should include:

- task ID, context ID, agent ID, owner peer ID, and creation/update times
- current A2A task status
- message history needed by A2A task inspection
- artifact metadata and local artifact references
- workspace path when retained by policy
- terminal failure/interruption reason when a task cannot be resumed

For ACP-backed agents, restart recovery does not imply transparent process
resume in the first version. If mesh-llm restarts while an OpenCode run is active,
the recovered task may move to a terminal interrupted/failed state with the
available events and artifacts preserved. True ACP session resume can be added
later if the harness supports it reliably.

## CLI Surface

Agent directory management should live under `mesh-llm agents ...`.

```bash
mesh-llm agents list
mesh-llm agents init <agent-id> --runtime opencode
mesh-llm agents validate <agent-id>
mesh-llm agents show <agent-id>
mesh-llm agents enable <agent-id>
mesh-llm agents disable <agent-id>
mesh-llm agents remove <agent-id>
```

The `agents` command owns local filesystem operations against
`~/.mesh-llm/agents/`. It should not send A2A tasks.

Protocol and client utilities stay under `mesh-llm a2a ...`:

```bash
mesh-llm a2a serve
mesh-llm a2a mcp
mesh-llm a2a discover
mesh-llm a2a get-card <agent-id>
mesh-llm a2a send <agent-id> <message>
mesh-llm a2a task <task-id>
mesh-llm a2a cancel <task-id>
```

Client commands own launching or configuring a local assistant with mesh-llm as
its inference and MCP substrate:

```bash
mesh-llm codex setup
mesh-llm goose
mesh-llm opencode
mesh-llm claude
```

These commands are not definition-management commands and they do not install a
specific agent. They launch or configure the named client with mesh inference,
mesh-owned MCP tools, configured extra MCP tools, and the A2A client tools.
Agents then appear dynamically through `get_agents` and are invoked through
`send_message`.

This keeps the user model clear:

| Command | Responsibility |
|---|---|
| `mesh-llm agents ...` | Create, inspect, validate, enable, and remove local agent definitions. |
| `mesh-llm a2a ...` | Serve A2A, expose MCP, discover remote agents, and call A2A tasks. |
| `mesh-llm codex setup` | Configure Codex with the mesh A2A MCP server and install Codex skills for using it. |
| `mesh-llm goose|opencode|claude` | Launch a client with mesh inference and the mesh A2A MCP tools. |
| `mesh-llm skills ...` | Low-level skill plumbing used by setup commands, not per-agent installation. |

## Mesh Discovery

Nodes discover agents by advertising compact summaries across the mesh and
fetching full Agent Cards only on demand.

```text
local agent dir loaded
  -> validate agent-card.json and runtime.toml
  -> create AgentAdvertisement summary
  -> publish summary to mesh
  -> peers cache summary
  -> clients call get_agents
  -> full AgentCard fetched from owning node when needed
```

Example advertisement:

```json
{
  "kind": "a2a_agent.v1",
  "agent_id": "pr-review",
  "name": "PR Review Agent",
  "description": "Reviews GitHub pull requests.",
  "owner_peer_id": "12D3...",
  "visibility": "private",
  "card_hash": "sha256:...",
  "card_route": "mesh://12D3.../a2a/agents/pr-review/.well-known/agent-card.json",
  "skills": [
    {
      "id": "github_pr_review",
      "name": "GitHub PR review"
    }
  ],
  "capabilities": {
    "streaming": true,
    "push_notifications": false
  },
  "updated_at": "2026-05-26T00:00:00Z"
}
```

Visibility rules:

| Mesh | Behavior |
|---|---|
| Private mesh | Advertise enabled private and public agents to joined peers. |
| Public mesh | Advertise only agents explicitly marked public. |

Discovery should be additive and mixed-version safe. The first implementation
should use a plugin mesh channel such as `a2a.discovery.v1`. Older nodes ignore
unknown plugin channels. If A2A becomes a core product surface, the summaries
can later move into optional protobuf gossip fields.

Cache behavior:

- Agent summaries expire by TTL.
- Full Agent Cards are cached by `card_hash`.
- Remote agents are removed when the owning peer leaves.
- Full cards are refetched when `card_hash` changes.
- Shutdown publishes best-effort tombstones.

## Remote Invocation

When a local client sends a task to a remote mesh agent:

```text
send_message(agent_id = "pr-review")
  -> look up owner_peer_id in the remote registry
  -> fetch and validate AgentCard if needed
  -> create official A2A client request
  -> route request over mesh to owner node
  -> return task state, events, and artifacts
```

The mesh discovers and reaches agents. Invocation should still be official A2A
at the semantic boundary.

## Task Events Over Mesh

For mesh-routed tasks, v1 should not depend on webhook-style push
notifications. The mesh-native mechanism is targeted task-event return from the
executor node to the owner/proxy node.

```text
A2A client
  -> node A receives request and owns client-facing task state
  -> node A routes execution to agent on node B
  -> node B runs the local harness
  -> node B sends task events back to node A over a targeted mesh channel
  -> node A persists task state and serves get_task/SSE/artifact reads
```

For OpenCode-backed agents, ACP is the local event source:

```text
OpenCode process
  -> ACP events over stdio
  -> mesh-llm ACP bridge on executor node
  -> normalized TaskEvent values
  -> local executor task store
  -> targeted mesh event return to owner node when owner != executor
```

The harness should not talk to the mesh directly. OpenCode speaks ACP. The
A2A/ACP bridge maps ACP events into internal task events, and mesh-llm's task
event router decides whether those events stay local or are returned to a
remote owner.

Conceptual internal sink:

```rust
trait TaskEventSink {
    async fn emit(&self, event: TaskEvent) -> Result<()>;
}
```

Conceptual events:

```rust
enum TaskEvent {
    StatusChanged { task_id: TaskId, status: TaskStatus },
    MessageDelta { task_id: TaskId, text: String },
    ArtifactCreated { task_id: TaskId, artifact_id: ArtifactId, kind: ArtifactKind },
    ArtifactUpdated { task_id: TaskId, artifact_id: ArtifactId },
    InputRequired { task_id: TaskId, request: InputRequest },
    Completed { task_id: TaskId },
    Failed { task_id: TaskId, error: String },
}
```

For remote execution, node A forwards the task to node B with:

- `task_id`
- `owner_peer_id`
- `executor_peer_id`
- per-task return token or signed envelope
- expected event sequence start

Node B returns task events to node A with sequence numbers. Node A validates the
event source, token/signature, and ordering before persisting the event and
waking local SSE subscribers.

Artifacts should follow the same ownership split:

- small text/data artifacts can be replicated back to the owner node
- large artifacts can stay on the executor and be fetched on demand over a
  mesh artifact route
- the owner node remains the client-facing A2A server in both cases

A2A push notification configuration can still be supported later for external
clients that require webhook-style callbacks. It should not be the primary
mesh-routed task update mechanism and should not be part of the default MCP
tool surface.

## Skills

Skill behavior belongs in a new workspace crate:

```text
crates/mesh-agents-skills/
```

The crate owns:

- optional client guidance skill discovery
- validation of `skill/SKILL.md`
- target-specific setup behavior
- install, uninstall, list, and status reports

It should not know how to run A2A tasks.

CLI surface:

```bash
mesh-llm skills list
mesh-llm skills install mesh-agents
mesh-llm skills uninstall mesh-agents
mesh-llm skills status
```

Skills are not how agents become available. Agents become available through the
mesh A2A MCP registry and `get_agents`. Skills are optional client guidance for
how to use that registry well.

A `--link` mode can be added for development.

Agent-provided skills should teach Codex when and how to use the mesh A2A MCP
tools. They should discover agents first, inspect Agent Cards, send structured
requests, stream or poll tasks, fetch artifacts, and present results. They
should not hide approval-sensitive actions.

There may also be a built-in generic `mesh-agents` Codex skill. Its job would be
to teach Codex the general delegation workflow without knowing about a specific
agent:

```text
discover agents -> inspect matching Agent Card -> send message -> poll task ->
view artifacts -> summarize result
```

Arguments for a generic skill:

- It makes any newly discovered mesh agent usable from Codex without installing
  an agent-specific skill first.
- It teaches safe defaults once, especially around task polling, artifact
  fetching, and approval-sensitive actions.
- It gives users a baseline experience before agent authors provide richer
  skills.

Arguments against making it automatic:

- Agent-specific skills can provide better task schemas and output handling.
- A generic skill may encourage Codex to delegate too broadly unless discovery
  and Agent Card inspection are very explicit.
- It may duplicate instructions from agent-specific skills.

Recommended approach: provide `mesh-agents` as a built-in Codex skill installed by
`mesh-llm codex setup`. `mesh-llm skills install mesh-agents` remains the
low-level primitive for scripting and tests. The skill should teach clients the
generic delegation workflow: discover agents, inspect Agent Cards, send
messages, poll tasks, and fetch artifacts. It should not represent a specific
agent.

## User Setup

User-facing client setup should be client-named. The local authoring path is:

```bash
mesh-llm agents init pr-review --runtime opencode
```

That makes the agent available to the local mesh A2A registry. Client setup is
separate and is not per-agent:

```bash
mesh-llm codex setup
mesh-llm goose
mesh-llm opencode
mesh-llm claude
```

`mesh-llm codex setup` should prepare Codex's mesh inference and MCP config,
expose the mesh A2A MCP tools, and install the generic `mesh-agents` Codex skill.
Launchable clients should be started with the same affordances:

```bash
mesh-llm goose
mesh-llm opencode
mesh-llm claude
```

For Codex, the generated or updated config should include a server like this
when `mesh-llm` is on `PATH`:

```toml
[mcp_servers.mesh_a2a]
command = "mesh-llm"
args = ["a2a", "mcp"]
enabled_tools = [
  "get_agents",
  "get_agent",
  "send_message",
  "get_task",
  "view_text_artifact",
  "view_data_artifact"
]
default_tools_approval_mode = "prompt"
```

For local development, the command should be able to render or write the same
config with the checked-out binary path instead:

```toml
[mcp_servers.mesh_a2a]
command = "/path/to/mesh-llm/target/debug/mesh-llm"
args = ["a2a", "mcp", "--agents-dir", "/path/to/agents", "--data-dir", "/path/to/data"]
enabled_tools = [
  "get_agents",
  "get_agent",
  "send_message",
  "get_task",
  "view_text_artifact",
  "view_data_artifact"
]
default_tools_approval_mode = "prompt"
```

The lower-level primitives remain useful for scripting and debugging:

```bash
mesh-llm a2a mcp
mesh-llm skills install mesh-agents
```

`mesh-llm codex setup` is the normal user command and should compose both
lower-level actions: write or render the MCP config and install the `mesh-agents`
skill.

OpenCode can participate in both directions:

```text
OpenCode as client:
  OpenCode uses mesh-llm's A2A MCP tools to call mesh agents.

OpenCode as worker:
  mesh-llm runs OpenCode through ACP behind an A2A agent endpoint.
```

## Worked Example: PR Review Agent

The first realistic OpenCode-backed example should be a dry-run PR review agent.
It is useful by itself and exercises the core integration points: GitHub, temp
workspaces, local shell/filesystem tools, mesh inference, structured artifacts,
streaming progress, and approval policy.

Directory:

```text
~/.mesh-llm/agents/pr-review/
  agent-card.json
  runtime.toml
  instructions.md
  skill/
    SKILL.md
```

`agent-card.json`:

```json
{
  "name": "PR Review Agent",
  "description": "Reviews GitHub pull requests for correctness bugs, regressions, missing tests, and project convention violations.",
  "version": "1.0.0",
  "supportedInterfaces": [
    {
      "url": "http://localhost:3131/a2a/agents/pr-review",
      "protocolBinding": "JSONRPC",
      "protocolVersion": "1.0"
    }
  ],
  "capabilities": {
    "streaming": true,
    "pushNotifications": false
  },
  "defaultInputModes": ["text/plain", "application/json"],
  "defaultOutputModes": ["text/markdown", "application/json"],
  "skills": [
    {
      "id": "github_pr_review",
      "name": "GitHub PR review",
      "description": "Review a pull request and return prioritized findings with file and line references.",
      "tags": ["github", "review"]
    }
  ]
}
```

`runtime.toml`:

```toml
enabled = true
visibility = "private"

[runtime]
type = "opencode"
model = "mesh"
mode = "smart_approve"
session_policy = "per_task"
max_concurrent_tasks = 1

[runtime.queue]
mode = "queue"
max_pending_tasks = 8

[runtime.workspace]
mode = "temp_per_task"
prefix = "mesh-pr-review-"
keep = "on_failure"

[instructions]
file = "instructions.md"
delivery = "first_prompt"

[tools.mesh]
enabled = true

[[tools.extra]]
name = "developer"
type = "mcp"
available_tools = ["shell", "text_editor"]

[[tools.extra]]
name = "github"
type = "mcp"
command = "github-mcp-server"
env_keys = ["GITHUB_PERSONAL_ACCESS_TOKEN"]
available_tools = [
  "get_pull_request",
  "get_pull_request_diff",
  "get_pull_request_files",
  "get_pull_request_comments"
]

[policy]
approval = "prompt"
filesystem = "workspace"
network = "allow"
max_task_seconds = 1800
advertise_on_mesh = true
public_mesh = false
```

`instructions.md`:

```markdown
You are a pull request review agent.

Default to dry-run review. Do not post comments, approve, request changes,
merge, close, label, or mutate GitHub state unless the task explicitly asks for
that and permission is granted.

For each review:

1. Identify the repository, pull request number, base branch, head branch,
   changed files, and CI state if available.
2. Create or use the provided workspace. Fetch the PR branch and inspect the
   diff plus nearby code.
3. Prefer deterministic facts from GitHub, git, filesystem, and tests over
   model guesses.
4. Run focused validation only when useful and reasonably bounded.
5. Review primarily for correctness bugs, regressions, compatibility breaks,
   security issues, missing tests, and project convention violations.
6. Ignore pure style issues unless they create real maintenance or correctness
   risk.
7. Return findings first, ordered by severity.

Output both:

- Markdown summary for humans.
- JSON artifact with fields: decision, findings, tests_run, residual_risk.

Finding format:

- severity: P0, P1, P2, or P3
- title
- file
- line
- body
- confidence

If there are no actionable findings, say so clearly and list any validation
that was not run.
```

Example text task:

```text
Review https://github.com/Mesh-LLM/mesh-llm/pull/123. Dry run only.
```

Example structured task:

```json
{
  "repo": "Mesh-LLM/mesh-llm",
  "pull_request": 123,
  "mode": "dry_run",
  "focus": ["correctness", "tests", "protocol_compatibility"],
  "run_tests": "focused"
}
```

The default must be dry run. Posting review comments, approvals, labels, or any
other GitHub mutation should require explicit task intent and policy approval.

## Security And Privacy

- Do not advertise private agents on public meshes unless explicitly opted in.
- Do not place local commands, environment names, credentials, or paths in
  Agent Cards or mesh summaries unless the user explicitly intends to share
  them.
- Read bearer tokens and other secrets from environment variables.
- Treat remote Agent Cards as untrusted until fetched and validated.
- Prefer allowlisted tools per agent.
- Map harness permission requests to A2A `input-required`.
- Keep public mesh agent advertisements compact and non-sensitive.
- Sign public Agent Card advertisements with mesh identity.

## ACP Event Preservation

The A2A/ACP bridge should preserve ACP detail in two layers:

1. A2A task status and artifacts should stay clean and user-facing.
2. Debug/trace artifacts should preserve enough ACP event detail to diagnose
   what the harness did.

Default user-facing artifacts should include:

- final Markdown/text answer
- structured JSON result when the agent produces one
- generated files or file references
- concise test/command summaries
- permission/input-required events that affected task flow

Detailed ACP events should be retained as an optional trace artifact, not mixed
into the main response:

```text
artifact: acp-trace.jsonl
```

That trace can include tool calls, tool results, permission requests, session
events, and timing metadata. It should redact secrets and avoid storing raw
large payloads unless the task policy enables verbose trace retention. This
keeps A2A artifacts useful for normal clients while preserving enough detail for
debugging OpenCode/ACP behavior.

## Implementation Plan

Implement this in slices that each leave a testable user or developer surface.
The first milestone should prove local A2A + OpenCode over ACP before adding mesh
routing.

### 1. Workspace Crates And Dependency Pins

Add the workspace crates:

```text
crates/mesh-agents-a2a/
crates/mesh-agents-acp-bridge/
crates/mesh-agents-skills/
```

Initial deliverables:

- workspace membership and package metadata
- dependency on official `a2aproject/a2a-rs` crates from `mesh-agents-a2a`
- dependency on official `agent-client-protocol` crate from
  `mesh-agents-acp-bridge`
- crate README files documenting ownership boundaries
- repo-consistency updates for crate lists, CI filters, and publish scripts

Validation:

- `cargo check -p mesh-agents-a2a`
- `cargo check -p mesh-agents-acp-bridge`
- `cargo check -p mesh-agents-skills`
- repo-consistency checks for crate-list changes

### 2. Agent Directory Loader

Implement directory-only agent discovery in `mesh-agents-a2a`.

Initial deliverables:

- load `~/.mesh-llm/agents/<agent-id>/agent-card.json`
- load `~/.mesh-llm/agents/<agent-id>/runtime.toml`
- reject loose JSON files directly under `agents/`
- validate `agent_id` from directory name
- validate required files
- normalize paths relative to the agent directory
- parse workspace config, queue config, tool config, visibility, and runtime
  type
- expose a small `AgentRegistry` API for host-runtime

Validation:

- unit tests with fixture directories
- validation tests for missing card, missing runtime, invalid runtime type,
  invalid workspace mode, duplicate agent IDs, and loose card files

### 3. Persistent Task Store

Add durable task state before remote execution.

Initial deliverables:

- SQLite-backed task store under the mesh-llm runtime data area
- task IDs and context IDs
- task lifecycle states
- message/event history needed for A2A `get_task`
- artifact metadata
- retained workspace path metadata
- terminal interruption state for tasks active during restart
- SQLite transaction boundaries for crash-safe updates

Validation:

- create task, append events, restart/reload store, fetch task
- recover active task as interrupted after simulated restart
- artifact metadata survives restart

### 4. Local A2A Server For One Agent

Expose one configured local agent over A2A without mesh routing.

Initial deliverables:

- Agent Card endpoint under `/a2a/agents/{agent_id}`
- SDK-native JSON-RPC and REST routers backed by official `a2a-server-lf`
- official A2A send-message handling
- official A2A task inspection
- SSE streaming for `message/stream`
- artifact retrieval hooks
- task state backed by the persistent task store
- host-runtime HTTP wiring with minimal route ownership in host-runtime
- fake local executor for end-to-end protocol and persistence tests before ACP
  execution is introduced
- first host-runtime mount:
  - `GET /a2a/agents/{agent_id}` returns the native Agent Card
  - `POST /a2a/agents/{agent_id}` dispatches to the official JSON-RPC router
    with the local executor selected by host-runtime

Validation:

- fetch Agent Card
- send message to a fake in-process agent
- stream events over SSE
- fetch task after completion
- fetch artifacts

### 5. ACP Bridge And OpenCode Runtime

Implement `mesh-agents-acp-bridge` as the first real harness adapter.

Initial deliverables:

- `runtime.type = "opencode"` expands to `opencode acp`
- advanced `runtime.type = "acp"` command/args path
- A2A executor implementation that creates one ACP session per task through
  the official `agent-client-protocol` client
- host-runtime local A2A dispatch wired to the ACP executor, with the fake
  executor retained only for local route tests
- runtime environment variables passed to the ACP subprocess
- workspace creation for `path`, `temp_per_task`, `agent_dir`, and `none`
- temp workspace cleanup according to `keep`
- instruction injection through `first_prompt`
- mesh MCP tools injected by default through `[tools.mesh]`
- per-agent MCP tool config passed to the ACP session
- ACP events translated into normalized `TaskEvent`s
- permission/input requests mapped to A2A `input-required`
- optional `acp-trace.jsonl` artifact with redaction

Validation:

- fake ACP server tests for event mapping
- temp workspace creation/cleanup tests
- OpenCode smoke test behind a local A2A task when OpenCode is installed
- cancellation and input-required behavior tests

### 6. MCP Bridge Surface

Expose the six-tool A2A MCP bridge from `mesh-llm a2a mcp`.

Initial deliverables:

- `get_agents`
- `get_agent`
- `send_message`
- `get_task`
- `view_text_artifact`
- `view_data_artifact`
- MCP tool responses are structured JSON
- large artifacts are navigated through view tools instead of embedded in task
  responses

Validation:

- MCP smoke test against local fake agent
- client-command setup smoke that verifies `mesh-llm codex setup` renders or
  writes Codex MCP config including the A2A MCP server

### 7. Agent And Skill CLI

Add user-facing definition and client setup/launch commands.

Initial deliverables:

- `mesh-llm agents list`
- `mesh-llm agents init <agent-id> --runtime opencode`
- `mesh-llm agents validate <agent-id>`
- `mesh-llm agents show <agent-id>`
- `mesh-llm agents enable <agent-id>`
- `mesh-llm agents disable <agent-id>`
- `mesh-llm codex setup`
- `mesh-llm goose`
- `mesh-llm opencode`
- `mesh-llm claude`
- `mesh-llm skills list`
- `mesh-llm skills install mesh-agents`
- `mesh-llm skills status`

Validation:

- CLI fixture tests for generated `pr-review` agent directory
- optional `mesh-agents` skill setup tests using temp Codex skill directories
- client-command tests verifying mesh inference config, agent MCP config, and
  A2A MCP tools are included for each supported client
- generated PR Review Agent files validate with the agent loader

### 8. Mesh Agent Discovery

Advertise compact agent summaries across the mesh.

Initial deliverables:

- plugin mesh channel, for example `a2a.discovery.v1`
- local advertisement from enabled agent directories
- tombstones on disable/shutdown where possible
- remote agent summary cache with TTL
- full Agent Card fetch by card hash
- public mesh filtering
- public advertisements signed by mesh identity
- `get_agents` includes local and mesh-discovered agents

Validation:

- two-node private mesh sees remote agent summary
- full card fetch works across mesh
- older nodes ignore the discovery channel
- private agents are not advertised on public meshes
- public advertisements reject invalid signatures

### 9. Remote Mesh Invocation

Route A2A task execution to remote mesh agents while keeping the local node as
the client-facing owner/proxy.

Initial deliverables:

- owner node creates persistent task record
- executor node receives routed A2A task request
- executor runs local agent harness
- executor sends targeted task events back to owner node
- owner persists returned events and wakes local SSE subscribers
- small artifacts replicate to owner
- large artifacts remain fetchable over a mesh artifact route

Validation:

- two-node send-message to remote OpenCode/fake agent
- owner node `get_task` reflects executor progress
- owner node SSE stream receives remote executor events
- event sequence validation rejects replay/out-of-order events
- artifacts can be viewed from the owner node

### 10. Remote External A2A Agents

Support directory-defined external A2A agents.

Initial deliverables:

- `runtime.type = "remote"`
- `card_url`
- auth from environment variables
- card fetch/cache/validation
- local MCP bridge can call external remote agents

Validation:

- fake external A2A server fixture
- auth header injection test
- card hash refresh test

### 11. Hardening And Compliance

Close protocol, privacy, and operational gaps.

Initial deliverables:

- A2A compatibility tests against official examples or inspector where useful
- structured error mapping
- task-store retention policy
- artifact retention policy
- trace redaction tests
- telemetry that does not leak prompts, credentials, private paths, or artifact
  bodies
- docs for config, setup, and the PR Review Agent

Validation:

- `cargo test -p mesh-agents-a2a --lib`
- `cargo test -p mesh-agents-acp-bridge --lib`
- `cargo test -p mesh-agents-skills --lib`
- `cargo test -p mesh-llm-host-runtime --lib` for host wiring
- targeted two-node mesh smoke test for discovery and remote invocation

### 12. Later Compatibility Work

Defer until required by real clients:

- gRPC transport parity
- webhook-style A2A push notification compatibility
- transparent ACP session resume after mesh-llm restart
- richer artifact replication policies

## Testing Notes

A2A over mesh touches routing, plugin channels, task state, and external tool
execution. Before treating a branch as ready:

- Validate local Agent Card serving and A2A compliance.
- Run MCP tool smoke tests against a local agent.
- Run an OpenCode-backed PR review fixture with temp workspaces.
- Verify cancellation and permission/input-required behavior.
- Run two-node private mesh discovery and remote invocation.
- Verify older mesh nodes ignore A2A discovery traffic.
- For public mesh behavior, verify private agents are not advertised.

## Open Questions

- Should gRPC be required before calling the feature "full A2A", or is
  JSON-RPC/REST plus SSE enough for the first full milestone?
- Which persistent store should back A2A task state?
- Which mesh stream/channel should carry targeted task-event return frames?
- When, if ever, should webhook-style A2A push notifications be supported?
- What retention policy should apply to optional `acp-trace.jsonl` artifacts?
- Should `mesh-llm codex setup` mutate `~/.codex/config.toml` by default, or
  print a patch unless `--write` is passed?
