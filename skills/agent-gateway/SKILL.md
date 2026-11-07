---
name: agent-gateway
description: >
  Operate agent-gateway (lightweight Rust HTTP/WebSocket PTY gateway on port 3000)
  to spawn, list, inspect stdin/stdout, send interactive input, resize PTY, and
  stop AI coding agents (Claude Code, Codex, Copilot CLI, Gemini CLI, or shell commands).
  Use when asked to launch or manage agents through agent-gateway, check running
  agents, read an agent's terminal output/input history, or send prompts/keys to
  a running agent via HTTP or WebSocket.
---

# agent-gateway

`agent-gateway` is a lightweight single-binary Rust server (default `http://localhost:3000`) that manages interactive CLI agents inside pseudo-terminals (PTY) and exposes their `stdin` / `stdout` over HTTP and WebSocket APIs.

Base URL defaults to `GATEWAY_URL="${AGENT_GATEWAY_URL:-http://localhost:3000}"`.

## 1. Pre-flight: Check or start the gateway

Before calling `/agents` endpoints, check if the gateway is already listening:

```bash
curl -sf http://localhost:3000/health
```

- If it returns `{"status":"ok"}`, proceed directly.
- If it is not running, start the binary in the background from the repository (`/home/kazto/Work/agent-gateway` or `agent-gateway` in `PATH`) and verify readiness via `/health`:
  ```bash
  cargo run --manifest-path /home/kazto/Work/agent-gateway/Cargo.toml --release
  ```

## 2. API Reference & Common Workflows

### List running agents (`GET /agents`)

By default, `GET /agents` returns only `running` agents. Pass `?status=all` or `?status=exited` to inspect terminated processes.

```bash
# Running agents only
curl -s http://localhost:3000/agents | jq .

# All agents (running + exited)
curl -s "http://localhost:3000/agents?status=all" | jq .
```

Each entry contains `id` (UUID), `name`, `command`, `args`, `cwd`, `pid`, `status` (`running` | `exited`), `exit_code`, `rows`, `cols`, `created_at`, and `exited_at`.

### Spawn an agent in a PTY (`POST /agents`)

Always set a descriptive `name` and explicit `cwd` so multiple agents can be distinguished easily:

```bash
curl -s -X POST http://localhost:3000/agents \
  -H "Content-Type: application/json" \
  -d '{
    "name": "reviewer-codex",
    "command": "codex",
    "args": [],
    "cwd": "/path/to/repo",
    "env": {},
    "rows": 36,
    "cols": 120
  }' | jq .
```

Save the returned `.id` (UUID) for subsequent operations.

### Read `stdin` and `stdout` (`GET /agents/:id/io`)

Returns both cumulative raw buffers (`stdin`, `stdout`) and sequenced `records` (`seq`, `timestamp`, `stream`, `data`).

```bash
# Full snapshot
curl -s "http://localhost:3000/agents/${AGENT_ID}/io" | jq .

# Read only new stdout chunks since a known sequence number
curl -s "http://localhost:3000/agents/${AGENT_ID}/io?stream=stdout&since_seq=${LAST_SEQ}" | jq .

# Read only the latest 20 records
curl -s "http://localhost:3000/agents/${AGENT_ID}/io?limit=20" | jq .
```

**Tip for Incremental Polling**:
Record the highest `.records[].seq` value from each response and pass it as `since_seq` on the next request to avoid re-reading entire terminal transcripts.

### Send input to `stdin` (`POST /agents/:id/input`)

Writes raw text/control sequences to the agent's PTY master.

```bash
# Send a line + Enter
curl -s -X POST "http://localhost:3000/agents/${AGENT_ID}/input" \
  -H "Content-Type: application/json" \
  -d '{"data": "Please review src/lib.rs\n"}' | jq .
```

**PTY Input Notes**:
- Interactive shells and line-oriented prompts expect `\n` (or `\r`) at the end of `data` to submit.
- TUI applications (such as Claude Code, Codex TUI, or Copilot CLI) in raw mode often treat `\r` as Enter, `\u001b` as `Esc`, and `\u0003` as `Ctrl+C`.
- Returns `409 Conflict` if the target agent has already exited.

### Resize the PTY (`POST /agents/:id/resize`)

If TUI output wraps narrowly or clips, resize the PTY before reading:

```bash
curl -s -X POST "http://localhost:3000/agents/${AGENT_ID}/resize" \
  -H "Content-Type: application/json" \
  -d '{"rows": 45, "cols": 140}' | jq .
```

### Stop an agent (`DELETE /agents/:id`)

Terminates the process and marks its status as `exited`:

```bash
curl -s -X DELETE "http://localhost:3000/agents/${AGENT_ID}" | jq .
```

### Real-time WebSocket stream (`GET /agents/:id/ws`)

Connect to `ws://localhost:3000/agents/<id>/ws`:
- On connect, the server replays buffered `{"type":"io","seq":...,"stream":"stdout|stdin","data":"..."}` messages followed by `{"type":"status","status":"running|exited","exit_code":...}`, then streams live `io` events.
- Send `{"type":"input","data":"...\n"}` (or raw text/binary frames) to write to `stdin`, or `{"type":"resize","rows":40,"cols":120}` to resize.

## 3. Operational & Safety Rules

1. **Verify target ID before sending input or stopping**: Always resolve the exact UUID from `GET /agents` or the `POST /agents` creation response.
2. **Do not stop agents you did not spawn** unless the user explicitly asks to terminate them.
3. **Strip ANSI escape codes when summarizing text**: PTY output (`stdout`) includes terminal escape sequences (colors, cursor movement). When extracting plain text in bash, pipe `.stdout` through `sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g'` if needed.
