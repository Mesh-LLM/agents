# Mesh Agents

Mesh Agents makes A2A agents available to Codex, OpenCode, Goose, Claude, and
other MCP-capable clients through mesh-llm.

Agents are not installed into each client one by one. They are discovered at
runtime through the Mesh Agents MCP server, inspected through their native A2A
Agent Cards, and invoked through A2A task calls.

## What It Provides

- A directory-backed local A2A agent registry.
- A stdio MCP server exposing A2A client tools.
- Persistent A2A task state backed by SQLite.
- An ACP bridge for local coding-agent harnesses such as OpenCode.
- Codex setup for the generic `mesh-agents` skill and MCP config.
- CLI tools for authoring local agent definitions.

## Status

This repository currently implements the local foundation:

- local agent directory loading
- local A2A JSON-RPC serving
- local A2A MCP tools
- local SQLite task persistence
- OpenCode-over-ACP task execution
- Codex setup scaffolding

Mesh-wide discovery and remote routing are planned next.

## Install

Install the released agents plugin binary through mesh-llm:

```bash
mesh-llm plugins install agents
```

If the catalog entry is not available yet, install directly from GitHub:

```bash
mesh-llm plugins install Mesh-LLM/agents
```

The installed command is `mesh-agents`.

## Build

Build the CLI:

```bash
cargo build -p mesh-agents
```

Release builds publish native archives for Linux, macOS, and Windows from this
repository's GitHub release workflow.

## Configure

Configure Codex:

```bash
mesh-agents codex setup
```

For development, preview the Codex changes without writing files:

```bash
mesh-agents codex setup --dry-run
```

Codex setup installs the generic `mesh-agents` skill and writes MCP config for
the A2A client tools.

## Quickstart

Create a local agent definition:

```bash
mesh-agents agents init pr-review --runtime opencode
```

Run the A2A MCP server directly:

```bash
mesh-agents a2a mcp
```

## Client Model

Codex and other clients talk to Mesh Agents through MCP. The MCP server exposes
six A2A client tools:

| Tool | Purpose |
|---|---|
| `get_agents` | List available agents. |
| `get_agent` | Read one agent's native Agent Card and runtime summary. |
| `send_message` | Send a message to an agent through A2A. |
| `get_task` | Fetch persisted task state. |
| `view_text_artifact` | Read text parts from a task artifact. |
| `view_data_artifact` | Read a structured task artifact. |

Codex setup writes an MCP config block like:

```toml
[mcp_servers.mesh_a2a]
command = "mesh-agents"
args = ["a2a", "mcp"]
enabled_tools = [
  "get_agents",
  "get_agent",
  "send_message",
  "get_task",
  "view_text_artifact",
  "view_data_artifact",
]
default_tools_approval_mode = "prompt"
```

It also installs the generic `mesh-agents` Codex skill, which teaches the client
to discover agents, inspect Agent Cards, submit tasks, poll task state, and read
artifacts.

## Agent Directory

Local agents live under `~/.mesh-llm/agents/`:

```text
~/.mesh-llm/
  agents/
    pr-review/
      agent-card.json
      runtime.toml
      instructions.md
```

`agent-card.json` is native A2A Agent Card JSON. `runtime.toml` is private local
execution policy for Mesh Agents.

Example runtime config:

```toml
enabled = true
visibility = "private"

[runtime]
type = "opencode"
max_concurrent_tasks = 1

[runtime.workspace]
mode = "temp_per_task"
keep = "on_failure"

[instructions]
file = "instructions.md"
delivery = "first_prompt"

[tools.mesh]
enabled = true

[policy]
advertise_on_mesh = false
public_mesh = false
```

## CLI

Agent authoring:

```bash
mesh-agents agents list
mesh-agents agents init <agent-id> --runtime opencode
mesh-agents agents validate [agent-id]
mesh-agents agents show <agent-id>
mesh-agents agents enable <agent-id>
mesh-agents agents disable <agent-id>
```

A2A and MCP:

```bash
mesh-agents a2a mcp
```

Codex:

```bash
mesh-agents codex setup
mesh-agents codex setup --dry-run
```

Low-level skill plumbing:

```bash
mesh-agents skills list
mesh-agents skills install mesh-agents
mesh-agents skills uninstall mesh-agents
mesh-agents skills status
```

## Release

Releases are built by GitHub Actions. Push a version tag:

```bash
git tag v0.1.0
git push origin v0.1.0
```

Or run the `Release` workflow manually with a `v*` version. The workflow builds
and uploads:

- `mesh-agents-linux-x86_64.tar.gz`
- `mesh-agents-macos-aarch64.tar.gz`
- `mesh-agents-windows-x86_64.zip`

## Workspace

```text
crates/mesh-agents-a2a/
  A2A Agent Card loading, local registry, local service, and SQLite task store.

crates/mesh-agents-acp-bridge/
  ACP harness bridge, OpenCode command expansion, workspace setup, and task execution.

crates/mesh-agents-cli/
  User-facing CLI and MCP stdio server.

crates/mesh-agents-skills/
  Skill filesystem helpers.
```

## Development

Run checks:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
```

The live OpenCode ACP tests are ignored by default because they require an
installed and configured OpenCode ACP agent.
