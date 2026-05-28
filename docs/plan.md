# A2A End-To-End Demo Plan

This plan tracks the work needed to make Mesh Agents work end to end and be
excellent to demo without relying on console visibility.

The demo prompt should be natural:

```text
Code review Mesh-LLM/mesh-llm#708.
```

The demo must not depend on the user knowing that agents exist, naming an
agent, or asking to discover agents. The AI client should infer that the task
is delegatable, use Mesh MCP to find the right agent, and return the result as
part of the normal answer.

The target flow is:

```text
AI client -> Mesh MCP -> agent discovery -> A2A task -> ACP harness -> artifacts -> AI client
```

## 1. Stack Prerequisites

Mesh-LLM-side work should use the `plugin-agents` stack:

```text
main
  -> Mesh-LLM/mesh-llm#725  plugin-provided skills
    -> Mesh-LLM/mesh-llm#702  plugin mesh streams
      -> future plugin-agents work
```

- Keep `#725` based on `main`.
- Keep `#702` stacked on `#725`.
- Stack future mesh-llm work on `#702`.
- Label all related mesh-llm PRs `plugin-agents`.
- Use GitHub merge queue for landing ready PRs in order.

## 2. Finish Agents Plugin Packaging

The agents plugin release archive should include:

- `agents` executable
- `plugin.toml`
- `README.md`
- `LICENSE`
- `skills/`

The skills packaging should follow the same pattern as the Blackboard plugin:
top-level `skills/<skill-name>/SKILL.md` files copied into release archives.

## 3. Add A Real `pr-review` Example

Add a realistic example agent:

```text
examples/pr-review/
  agent-card.json
  runtime.toml
  instructions.md
```

Requirements:

- The A2A Agent Card clearly describes pull request review.
- The Agent Card includes skills/tags such as `github`, `code-review`, and
  `pull-request` so the client skill can match ordinary review prompts.
- `runtime.toml` uses ACP with substitutions.
- `max_concurrent_tasks = 1`.
- `instructions.md` tells the harness to return actionable findings with file
  and line references.
- The example should be directly usable as the main demo agent.

## 4. Make Setup Simple

The happy path should be:

```bash
mesh-llm plugins install Mesh-LLM/agents
mesh-llm skills install
mesh-llm agents init pr-review --runtime opencode
mesh-llm agents validate pr-review
mesh-llm opencode
```

Constraints:

- No direct `agents mcp`.
- No manual MCP config.
- No console dependency.

## 5. Make MCP Tools Reliable

The hosted Mesh MCP endpoint must support the full user flow:

- `get_agents`
- `get_agent`
- `send_message`
- `get_task`
- `view_text_artifact`
- `view_data_artifact`

The user should not need to know these tool names or ask to find an agent. The
packaged skills should guide the AI client to recognize delegatable tasks,
discover matching agents, inspect, delegate, poll, and read artifacts.

For the demo, a prompt like `Code review Mesh-LLM/mesh-llm#708.` should be
enough for the client to choose `pr-review` through discovery.

## 6. Make Task Output Impressive

A completed `pr-review` task should return artifacts:

```text
summary.md
findings.json
```

Findings should include:

- severity
- file
- line
- issue
- recommendation

Task state should persist across mesh-llm restarts.

## 7. Select Mesh Models By Capability

Agents should not have to hard-code a specific mesh model when they really need
a capability. A `pr-review` agent usually wants a coding-capable text model
with tool calling. Other agents might need image, video, audio, multimodal,
reasoning, or long-context capability.

Plan:

- Let agent runtime policy express model intent by capability, not only by
  model name.
- Example capabilities: `text`, `coding`, `tool_calling`, `image`, `video`,
  `audio`, `multimodal`, `reasoning`, and `long_context`.
- Resolve capability requests to an available mesh model at task start.
- Expose the resolved model through `{{ mesh.model }}` for ACP command
  substitution.
- Keep explicit model selection available for agents that need a specific
  model.

Example direction:

```toml
[runtime.model]
capabilities = ["text", "coding", "tool_calling"]
fallback = "auto"
```

Open question:

- Verify whether Mesh's OpenAI-compatible `/v1/models` surface currently
  exposes model capabilities. If it does not, add a `plugin-agents` mesh-llm
  task to expose enough capability metadata for agents and client skills to
  choose models reliably.

## 8. Prove ACP Runtime Substitution

The demo agent should use the explicit ACP runtime path:

```toml
[runtime]
type = "acp"
command = "opencode"
args = [
  "acp",
  "--cwd", "{{ task.workspace }}",
  "--mcp", "{{ mesh.mcp_url }}",
  "--instructions", "{{ instructions.file }}"
]
max_concurrent_tasks = 1
```

Verify substitutions for:

- `{{ task.workspace }}`
- `{{ mesh.mcp_url }}`
- `{{ instructions.file }}`
- `{{ task.prompt_path }}`
- `{{ task.artifacts_dir }}`
- `{{ task.logs_dir }}`

## 9. Prove Mesh-Wide Discovery

After single-node success, run a two-node private mesh:

```text
Node A: user + AI client
Node B: owns pr-review
```

Show through MCP results:

- Node A discovers `pr-review`.
- Node A sends the A2A task.
- Node B runs the ACP harness.
- Node A receives final task state and artifacts.

No console visibility is required for the demo.

## 10. Harden Important Failures

Keep failure hardening scoped to task/debug quality:

- Invalid Agent Card points to file and field.
- Missing Mesh MCP endpoint names the URL.
- ACP process failure stores logs under `{{ task.logs_dir }}`.
- Failed task is persisted and readable through `get_task`.

Do not spend time on harness install guidance.

## 11. Demo Script

1. Start a private mesh node.
2. Install the agents plugin.
3. Install skills.
4. Create or load `pr-review`.
5. Launch `mesh-llm opencode`.
6. Prompt naturally:

   ```text
   Code review Mesh-LLM/mesh-llm#708.
   ```

   Do not mention agents, Mesh, MCP, A2A, or tool names in the prompt.

7. Ask a follow-up:

   ```text
   Show me the findings artifact.
   ```

8. Restart mesh-llm and ask for the prior task state.
9. Optional: repeat with `pr-review` hosted on Node B.

## Execution Order

1. Commit and push skills packaging in `agents`.
2. Add `examples/pr-review`.
3. Verify single-node hosted MCP flow.
4. Improve task artifacts.
5. Add model capability selection for mesh inference.
6. Add ACP logs and failure behavior.
7. Land `Mesh-LLM/mesh-llm#725`.
8. Rebase and land `Mesh-LLM/mesh-llm#702`.
9. Build and validate two-node remote discovery/routing.
10. Run the final no-console demo end to end.
