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
GATEWAY_URL="${AGENT_GATEWAY_URL:-http://localhost:3000}"
curl -sf "${GATEWAY_URL}/health"
```

- If it returns `{"status":"ok"}`, proceed directly.
- If it is not running, check if a custom port is configured in `~/.config/agent-gateway/config.toml` (`%USERPROFILE%\.config\agent-gateway\config.toml` on Windows) under `[default]`, or start the binary in the background from `/home/kazto/Work/agent-gateway` (or `agent-gateway` in `PATH`) and verify readiness via `/health`. Port/host can be customized via `-p <PORT>` / `--port <PORT>`, `--host <HOST>`, `--config <PATH>`, `AGENT_GATEWAY_PORT` / `PORT`, or `config.toml`:
  ```bash
  cargo run --manifest-path /home/kazto/Work/agent-gateway/Cargo.toml --release -- --port 3000
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

Pass `"agent"` (`"claude"`, `"codex"`, `"copilot"`, `"gemini"`, etc.) to explicitly select the agent, or omit `"agent"` to launch the gateway's configured default agent (`[default] agent` in `config.toml`, default `"claude"`):

```bash
# Spawn using the default agent configured in config.toml
curl -s -X POST "${GATEWAY_URL}/agents" \
  -H "Content-Type: application/json" \
  -d '{
    "name": "default-worker",
    "cwd": "/path/to/repo"
  }' | jq .

# Explicitly specify the agent to launch
curl -s -X POST "${GATEWAY_URL}/agents" \
  -H "Content-Type: application/json" \
  -d '{
    "agent": "codex",
    "name": "reviewer-codex",
    "cwd": "/path/to/repo",
    "args": [],
    "env": {},
    "rows": 36,
    "cols": 120
  }' | jq .
```

Save the returned `.id` (UUID) for subsequent operations.

### Read the current rendered terminal screen (`GET /agents/:id/screen`) — **Preferred for AI judgment**

Because interactive TUIs (Claude Code, Copilot CLI, Codex, Gemini CLI) emit tens of kilobytes of raw ANSI cursor/color codes, **always prefer `GET /agents/:id/screen`** to inspect what is currently displayed on the terminal screen. It uses a `vt100` virtual terminal emulator to return clean, ANSI-free plain text and cursor coordinates:

```bash
curl -s "${GATEWAY_URL}/agents/${AGENT_ID}/screen" | jq -r '.screen'
```

### Read raw `stdin` and `stdout` history (`GET /agents/:id/io`)

Returns `.screen` (rendered screen), cumulative raw buffers (`stdin`, `stdout`), and sequenced `records` (`seq`, `timestamp`, `stream`, `data`).

```bash
# Read only new stdout chunks since a known sequence number
curl -s "${GATEWAY_URL}/agents/${AGENT_ID}/io?stream=stdout&since_seq=${LAST_SEQ}" | jq .

# Read only the latest 20 records
curl -s "${GATEWAY_URL}/agents/${AGENT_ID}/io?limit=20" | jq .
```

### Send input to `stdin` (`POST /agents/:id/input`)

Writes raw text/control sequences to the agent's PTY master.

```bash
# Send a prompt + Enter (\r)
curl -s -X POST "${GATEWAY_URL}/agents/${AGENT_ID}/input" \
  -H "Content-Type: application/json" \
  -d '{"data": "Please review src/lib.rs\r"}' | jq .
```

**PTY Key & Control Sequence Reference**:
- `Enter` (submit / confirm selection): `\r` (preferred for raw-mode TUIs) or `\n`
- `Down arrow`: `\u001b[B`
- `Up arrow`: `\u001b[A`
- `Escape`: `\u001b`
- `Ctrl+C`: `\u0003`
- `Tab`: `\t` / `Shift+Tab`: `\u001b[Z`

### Autonomous Dialog & Prompt Resolution Loop

After spawning an agent (`POST /agents`) or sending a prompt (`POST /agents/:id/input`), **always inspect `GET /agents/:id/screen`** after 1–3 seconds and judge the screen state before proceeding:

1. **Workspace / Folder Trust Dialogs** (safe to auto-confirm when working in the requested project directory):
   - **Copilot CLI**: Shows `Confirm folder trust` with `❯ 1. Yes` selected by default → send `{"data": "\r"}` to confirm and advance.
   - **Claude Code**: Shows `Quick safety check: Is this a project you created or one you trust?` with `❯ No, exit` on the first line and `Yes, I trust this folder` on the second line → send `{"data": "\u001b[B\r"}` (Down arrow + Enter) to select "Yes, I trust this folder".
   - **Codex / Gemini CLI**: If a trust/approval menu appears, inspect which line has the cursor (`❯` or `>`) and send arrow keys + `\r` (or `y\r`) accordingly.
2. **Wait until Ready Prompt**:
   - Confirm via `GET /agents/:id/screen` that the TUI has reached its main input prompt (e.g. `❯`) before sending the task prompt.
3. **Tool / Command Approval Prompts during Execution**:
   - While the spawned agent is working, periodically check `GET /agents/:id/screen`.
   - If the spawned agent pauses at a confirmation prompt (e.g., asking to run a safe read/test command or file edit within the target workspace), evaluate whether the action is safe and within the user's requested scope. If safe, send the confirmation key (`\r` or `y\r`). If destructive or outside scope, stop and ask the user.

### Resize the PTY (`POST /agents/:id/resize`)

If TUI output wraps narrowly or clips, resize the PTY before reading:

```bash
curl -s -X POST "http://localhost:3000/agents/${AGENT_ID}/resize" \
  -H "Content-Type: application/json" \
  -d '{"rows": 45, "cols": 140}' | jq .
```

### Stop an agent (`POST /agents/:id/stop` or `DELETE /agents/:id`)

Terminates the process and marks its status as `exited`:

```bash
curl -s -X POST "http://localhost:3000/agents/${AGENT_ID}/stop" | jq .
```

### Real-time WebSocket stream (`GET /agents/:id/ws`)

Connect to `ws://localhost:3000/agents/<id>/ws`:
- On connect, the server replays buffered `{"type":"io","seq":...,"stream":"stdout|stdin","data":"..."}` messages followed by `{"type":"status","status":"running|exited","exit_code":...}`, then streams live `io` events.
- Send `{"type":"input","data":"...\n"}` (or raw text/binary frames) to write to `stdin`, or `{"type":"resize","rows":40,"cols":120}` to resize.

## 3. Operational & Safety Rules

1. **Verify target ID before sending input or stopping**: Always resolve the exact UUID from `GET /agents` or the `POST /agents` creation response.
2. **Do not stop agents you did not spawn** unless the user explicitly asks to terminate them.
3. **Strip ANSI escape codes when summarizing text**: PTY output (`stdout`) includes terminal escape sequences (colors, cursor movement). When extracting plain text in bash, pipe `.stdout` through `sed 's/\x1b\[[0-9;?]*[a-zA-Z]//g'` if needed.
