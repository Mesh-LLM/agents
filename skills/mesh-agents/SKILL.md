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

1. For delegatable tasks, call `agents.get_agents` to list available mesh agents
   before attempting the work locally.
2. Pick the best agent for the task. For prompts like "code review <PR>",
   "review this PR", "review this branch", or "check this diff", prefer agents
   advertising `github`, `code-review`, `pull-request`, `branch-review`, or a
   matching review skill.
3. If unsure, call `agents.get_agent` to inspect the
   A2A Agent Card, skills, runtime summary, and output modes.
4. Call `agents.send_message` with a clear task brief and any required URLs,
   branch names, file paths, or constraints. Preserve the returned `task_id`;
   it is the follow-up handle for task polling and artifact reads.
5. Use `agents.get_task` with that `task_id` to check task status until
   completion. If the task is still submitted or working, poll again.
6. Prefer returned artifacts over raw task messages. For PR reviews, read
   `summary.md` with `agents.view_text_artifact` and `findings.json` with
   `agents.view_data_artifact` when they are present.
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
- Mesh's aggregate MCP endpoint prefixes tools by plugin name. If a client
  exposes unprefixed aliases for this plugin, the workflow is the same, but
  prefer the visible `agents.*` tools when they are present.
- If no suitable agent is available, say that directly and continue locally.
- Treat returned artifacts as the source of truth for the delegated task.
- Do not make the user ask for agent discovery first. A natural prompt such as
  `Code review Mesh-LLM/mesh-llm#708.` is enough to trigger this workflow when
  a matching agent is available.
- Mention that a mesh agent was used in the result summary, but keep the answer
  focused on the outcome.
- Do not expose secrets, private keys, tokens, or unrelated local files in the
  task prompt.
