#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
AGENTS_BIN="${AGENTS_BIN:-$ROOT/target/debug/agents}"
MESH_LLM_BIN="${MESH_LLM_BIN:-$(command -v mesh-llm || true)}"
WORKDIR="${WORKDIR:-$(mktemp -d /tmp/mesh-agents-e2e.XXXXXX)}"

NODE_A_API_PORT="${NODE_A_API_PORT:-19337}"
NODE_A_CONSOLE_PORT="${NODE_A_CONSOLE_PORT:-13131}"
NODE_B_API_PORT="${NODE_B_API_PORT:-19338}"
NODE_B_CONSOLE_PORT="${NODE_B_CONSOLE_PORT:-13132}"

NODE_A_HOME="$WORKDIR/node-a-home"
NODE_B_HOME="$WORKDIR/node-b-home"
NODE_A_RUNTIME="$WORKDIR/node-a-runtime"
NODE_B_RUNTIME="$WORKDIR/node-b-runtime"
LOG_DIR="$WORKDIR/logs"
PIDS=()

fail() {
  echo "error: $*" >&2
  exit 1
}

cleanup() {
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  wait >/dev/null 2>&1 || true
}
trap cleanup EXIT

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "missing required command: $1"
}

wait_status() {
  local port="$1"
  local output="$2"
  for _ in $(seq 1 90); do
    if curl -fsS "http://127.0.0.1:$port/api/status" >"$output" 2>/dev/null; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}

mcp_init() {
  local port="$1"
  local output="$WORKDIR/mcp-init.out"
  curl -i -sS -X POST "http://127.0.0.1:$port/mcp" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"mesh-agents-e2e","version":"0.0.0"}}}' \
    >"$output"
  awk 'tolower($0) ~ /^mcp-session-id:/ {gsub("\r", "", $2); print $2}' "$output"
}

mcp_call() {
  local port="$1"
  local session="$2"
  local id="$3"
  local name="$4"
  local args="$5"
  local output="$6"
  jq -n \
    --argjson id "$id" \
    --arg name "$name" \
    --argjson arguments "$args" \
    '{jsonrpc:"2.0", id:$id, method:"tools/call", params:{name:$name, arguments:$arguments}}' |
    curl -sS -X POST "http://127.0.0.1:$port/mcp" \
      -H "Content-Type: application/json" \
      -H "Accept: application/json, text/event-stream" \
      -H "mcp-session-id: $session" \
      --data @- \
      >"$output"
  awk '/^data: \{/{sub(/^data: /, ""); print}' "$output" | tail -1 >"$output.json"
}

require_cmd curl
require_cmd jq
require_cmd awk

if [[ ! -x "$AGENTS_BIN" ]]; then
  echo "building agents binary..."
  (cd "$ROOT" && cargo build --locked)
fi
[[ -x "$AGENTS_BIN" ]] || fail "agents binary not found: $AGENTS_BIN"
[[ -n "$MESH_LLM_BIN" && -x "$MESH_LLM_BIN" ]] || fail "set MESH_LLM_BIN to a mesh-llm binary or put mesh-llm on PATH"

mkdir -p "$NODE_A_HOME/.mesh-llm/agents" "$NODE_B_HOME/.mesh-llm" "$LOG_DIR"
cp -R "$ROOT/examples/pr-review" "$NODE_A_HOME/.mesh-llm/agents/pr-review"
perl -0pi -e 's/advertise_on_mesh = false/advertise_on_mesh = true/' \
  "$NODE_A_HOME/.mesh-llm/agents/pr-review/runtime.toml"

cat >"$NODE_A_HOME/.mesh-llm/config.toml" <<EOF
[[plugin]]
name = "agents"
command = "$AGENTS_BIN"
EOF

cat >"$NODE_B_HOME/.mesh-llm/config.toml" <<EOF
[[plugin]]
name = "agents"
command = "$AGENTS_BIN"
EOF

"$AGENTS_BIN" agents validate pr-review --dir "$NODE_A_HOME/.mesh-llm/agents"

echo "workdir: $WORKDIR"
echo "starting node A..."
HOME="$NODE_A_HOME" MESH_LLM_RUNTIME_ROOT="$NODE_A_RUNTIME" \
  "$MESH_LLM_BIN" client \
    --port "$NODE_A_API_PORT" \
    --console "$NODE_A_CONSOLE_PORT" \
    --name node-a \
    --log-format json \
    >"$LOG_DIR/node-a.log" 2>&1 &
PIDS+=("$!")

wait_status "$NODE_A_CONSOLE_PORT" "$WORKDIR/node-a-status.json" || {
  tail -120 "$LOG_DIR/node-a.log" >&2 || true
  fail "node A did not become ready"
}

TOKEN="$(jq -r '.token' "$WORKDIR/node-a-status.json")"
[[ -n "$TOKEN" && "$TOKEN" != "null" ]] || fail "node A did not expose an invite token"

echo "verifying node A local agents MCP surface..."
SESSION_A="$(mcp_init "$NODE_A_CONSOLE_PORT")"
[[ -n "$SESSION_A" ]] || fail "node A MCP initialize did not return a session id"
curl -sS -X POST "http://127.0.0.1:$NODE_A_CONSOLE_PORT/mcp" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "mcp-session-id: $SESSION_A" \
  --data '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  >/dev/null
for i in $(seq 1 20); do
  mcp_call "$NODE_A_CONSOLE_PORT" "$SESSION_A" "$((50 + i))" "agents.get_agents" '{}' "$WORKDIR/node-a-get-agents.out"
  if jq -e '.result.structuredContent.agents[]? | select(.agent_id == "pr-review" and .location == "local")' \
    "$WORKDIR/node-a-get-agents.out.json" >/dev/null; then
    break
  fi
  sleep 0.5
done
jq -e '.result.structuredContent.agents[]? | select(.agent_id == "pr-review" and .location == "local")' \
  "$WORKDIR/node-a-get-agents.out.json" >/dev/null || fail "node A did not expose local pr-review"

echo "starting node B..."
HOME="$NODE_B_HOME" MESH_LLM_RUNTIME_ROOT="$NODE_B_RUNTIME" \
  "$MESH_LLM_BIN" client \
    --port "$NODE_B_API_PORT" \
    --console "$NODE_B_CONSOLE_PORT" \
    --join "$TOKEN" \
    --name node-b \
    --log-format json \
    >"$LOG_DIR/node-b.log" 2>&1 &
PIDS+=("$!")

for _ in $(seq 1 90); do
  if curl -fsS "http://127.0.0.1:$NODE_B_CONSOLE_PORT/api/status" >"$WORKDIR/node-b-status.json" 2>/dev/null; then
    if [[ "$(jq '.peers | length' "$WORKDIR/node-b-status.json")" -eq 1 ]]; then
      break
    fi
  fi
  sleep 0.5
done

[[ "$(jq '.peers | length' "$WORKDIR/node-b-status.json")" -eq 1 ]] || {
  tail -120 "$LOG_DIR/node-b.log" >&2 || true
  fail "node B did not join node A"
}

echo "refreshing node A agent advertisement..."
mcp_call "$NODE_A_CONSOLE_PORT" "$SESSION_A" 80 "agents.get_agents" '{}' "$WORKDIR/node-a-refresh-agents.out"

SESSION="$(mcp_init "$NODE_B_CONSOLE_PORT")"
[[ -n "$SESSION" ]] || fail "MCP initialize did not return a session id"
curl -sS -X POST "http://127.0.0.1:$NODE_B_CONSOLE_PORT/mcp" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "mcp-session-id: $SESSION" \
  --data '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  >/dev/null

echo "waiting for remote pr-review advertisement..."
for i in $(seq 1 30); do
  mcp_call "$NODE_B_CONSOLE_PORT" "$SESSION" "$((100 + i))" "agents.get_agents" '{}' "$WORKDIR/get-agents.out"
  if jq -e '.result.structuredContent.agents[]? | select(.agent_id == "pr-review" and .location == "remote")' \
    "$WORKDIR/get-agents.out.json" >/dev/null; then
    break
  fi
  sleep 1
done

jq -e '.result.structuredContent.agents[]? | select(.agent_id == "pr-review" and .location == "remote")' \
  "$WORKDIR/get-agents.out.json" >/dev/null || fail "node B did not discover remote pr-review"

echo "sending remote PR review smoke task..."
TASK_MESSAGE="Smoke test only. Reply with a one sentence status that the PR review agent received this task. Do not inspect files and do not call external services."
mcp_call "$NODE_B_CONSOLE_PORT" "$SESSION" 200 "agents.send_message" \
  "$(jq -n --arg message "$TASK_MESSAGE" '{agent_id:"pr-review", message:$message}')" \
  "$WORKDIR/send-message.out"

TASK_ID="$(jq -r '.result.structuredContent.task_id' "$WORKDIR/send-message.out.json")"
[[ -n "$TASK_ID" && "$TASK_ID" != "null" ]] || fail "send_message did not return task_id"

echo "returned task_id: $TASK_ID"
for i in $(seq 1 45); do
  mcp_call "$NODE_B_CONSOLE_PORT" "$SESSION" "$((300 + i))" "agents.get_task" \
    "$(jq -n --arg task_id "$TASK_ID" '{agent_id:"pr-review", task_id:$task_id}')" \
    "$WORKDIR/get-task.out"
  STATE="$(jq -r '.result.structuredContent.task.task.status.state // .result.structuredContent.task.status.state // empty' "$WORKDIR/get-task.out.json")"
  if [[ "$STATE" == "TASK_STATE_COMPLETED" ]]; then
    break
  fi
  sleep 1
done

[[ "$STATE" == "TASK_STATE_COMPLETED" ]] || fail "task did not complete, final state: ${STATE:-unknown}"

mcp_call "$NODE_B_CONSOLE_PORT" "$SESSION" 500 "agents.view_text_artifact" \
  "$(jq -n --arg task_id "$TASK_ID" '{agent_id:"pr-review", task_id:$task_id, artifact_id:"summary.md"}')" \
  "$WORKDIR/view-summary.out"

mcp_call "$NODE_B_CONSOLE_PORT" "$SESSION" 501 "agents.view_data_artifact" \
  "$(jq -n --arg task_id "$TASK_ID" '{agent_id:"pr-review", task_id:$task_id, artifact_id:"findings.json"}')" \
  "$WORKDIR/view-findings.out"

echo "summary.md:"
jq -r '.result.structuredContent.text' "$WORKDIR/view-summary.out.json"
echo
echo "findings.json:"
jq '.result.structuredContent.data' "$WORKDIR/view-findings.out.json"
echo
echo "ok: private mesh PR review smoke passed"
