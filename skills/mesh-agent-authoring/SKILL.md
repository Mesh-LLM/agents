---
name: mesh-agent-authoring
description: Use when creating, editing, or validating mesh-llm agent definitions, A2A Agent Cards, runtime.toml files, or ACP runtime command substitutions.
---

# Mesh Agent Authoring

Use this skill when the user wants to define or modify a Mesh agent.

Mesh agent definitions live under `~/.mesh-llm/agents/<agent-id>/` and usually
contain:

- `agent-card.json`: the public A2A Agent Card.
- `runtime.toml`: private Mesh execution policy.
- `instructions.md`: the operating brief sent into the harness.

## Start From The CLI

Create a starter definition through the mesh-llm plugin CLI:

```bash
mesh-llm agents init pr-review --runtime opencode
```

Validate after edits:

```bash
mesh-llm agents validate pr-review
mesh-llm agents show pr-review
```

## Agent Card

The Agent Card is the public contract. It should describe what the agent does,
which skills it provides, supported input/output modes, and how A2A clients
should understand the agent.

When editing `agent-card.json`:

- Keep it valid native A2A Agent Card JSON.
- Make descriptions specific enough for another AI client to choose the agent.
- Use stable skill ids such as `pr-review`, `docs-maintainer`, or
  `release-notes`.
- Do not put local secrets, private paths, or harness-only details in the card.

## Runtime Policy

`runtime.toml` is local execution policy. It is not the public A2A contract.

Named runtimes such as `opencode`, `goose`, and `pi` are ACP presets. Use the
generic ACP runtime when the user needs explicit command control:

```toml
[runtime]
type = "acp"
command = "$HOME/bin/my-agent-harness"
args = [
  "acp",
  "--cwd", "{{ task.workspace }}",
  "--mcp", "{{ mesh.mcp_url }}",
  "--instructions", "{{ instructions.file }}"
]
max_concurrent_tasks = 1
```

Substitutions happen when a task starts. Useful variables include:

- `{{ agent.id }}`
- `{{ agent.dir }}`
- `{{ agent.card_path }}`
- `{{ task.id }}`
- `{{ task.workspace }}`
- `{{ task.prompt_path }}`
- `{{ task.artifacts_dir }}`
- `{{ task.logs_dir }}`
- `{{ instructions.file }}`
- `{{ mesh.mcp_url }}`
- `{{ mesh.api_url }}`
- `{{ mesh.openai_url }}`
- `{{ mesh.model }}`
- `{{ mesh.data_dir }}`
- `$HOME`, `${HOME}`, and `{{ env.HOME }}`

## Safety

- Default `max_concurrent_tasks = 1` for coding agents that mutate workspaces.
- Prefer `workspace.mode = "temp_per_task"` unless the user explicitly wants a
  fixed checkout.
- Keep `policy.advertise_on_mesh = false` and `policy.public_mesh = false`
  until the user intentionally wants discovery by other nodes.
- Validate the agent definition before telling the user it is ready.
