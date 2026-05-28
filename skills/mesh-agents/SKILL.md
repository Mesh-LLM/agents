---
name: mesh-agents
description: Use when discovering, inspecting, or delegating work to mesh-llm A2A agents through Mesh's MCP tools.
---

# Mesh Agents

Use this skill when the user's task may be better handled by an agent available
through Mesh. The user does not need to explicitly ask to "find an agent" or
"use Mesh."

Mesh exposes agents through the node-hosted MCP endpoint. Do not assume an
agent is installed in the local coding client. Discover agents dynamically.

## Workflow

1. For delegatable tasks, call `get_agents` to list available mesh agents.
2. Pick the best agent for the task. For pull request or code review requests,
   prefer agents advertising `github`, `code-review`, `pull-request`, or a
   matching review skill.
3. If unsure, call `get_agent` to inspect the
   A2A Agent Card, skills, runtime summary, and output modes.
4. Call `send_message` with a clear task brief and any required URLs, branch
   names, file paths, or constraints.
5. Use `get_task` to check task status until completion.
6. Use `view_text_artifact` or `view_data_artifact` to read returned artifacts.
7. Summarize the agent result for the user and include any task or artifact ids
   that matter for follow-up.

## Good Delegation Prompts

Prefer concrete task briefs:

```text
Code review Mesh-LLM/mesh-llm#708.
```

```text
Review Mesh-LLM/mesh-llm#708. Return only actionable findings with file and line
references. Prioritize correctness, regressions, and missing tests.
```

```text
Update docs from this change summary. Preserve existing heading style and return
the patch summary plus any files changed.
```

Avoid vague delegation such as "use an agent for this" without first checking
which agents are available and what they advertise.

## Rules

- Use the Agent Card to understand what an agent can do.
- Do not invent agent ids. Discover them.
- If no suitable agent is available, say that directly and continue locally.
- Treat returned artifacts as the source of truth for the delegated task.
- Mention that a mesh agent was used in the result summary; do not require the
  user to ask for agent discovery first.
- Do not expose secrets, private keys, tokens, or unrelated local files in the
  task prompt.
