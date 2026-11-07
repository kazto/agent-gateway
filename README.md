# AIエージェントゲートウェイ (agent-gateway)

Rust製の軽量・シングルバイナリなAIエージェント管理ゲートウェイです。PTY（擬似端末）を介して対話型CLIエージェントを起動・管理し、HTTP / WebSocket API経由で標準入力・標準出力の取得や操作を行えます。

## コンセプト・特徴

- デフォルトで `3000` ポートで待ち受ける（CLIオプション `-p`/`--port`、環境変数 `AGENT_GATEWAY_PORT`/`PORT`、設定ファイルで変更可能）
- PTY（擬似端末）対応により、Claude Code・Copilot CLIなどの対話型TUIエージェントもそのまま動作
- 現在実行中のAIエージェントを一覧できる
- 任意のAIエージェントの標準入力・標準出力の取得および標準入力への送信（HTTP / WebSocket双方向通信）ができる

## 起動方法・設定

```bash
# デフォルト起動（0.0.0.0:3000）
cargo run

# CLIオプションでポート・ホストを指定
cargo run -- -p 4000 --host 127.0.0.1

# 環境変数で指定
PORT=4000 ./target/release/agent-gateway
AGENT_GATEWAY_PORT=4000 AGENT_GATEWAY_HOST=127.0.0.1 ./target/release/agent-gateway

# 設定ファイル（TOML）で指定
# --config未指定時は以下のパスを自動読込:
#   - Mac / Linux: ~/.config/agent-gateway/config.toml
#   - Windows:     %USERPROFILE%\.config\agent-gateway\config.toml (%USER_PROFILE% も対応)
./target/release/agent-gateway --config ~/.config/agent-gateway/config.toml
```

設定ファイル例 (`config.toml`):
```toml
[default]
port = 12345
host = "127.0.0.1"
```
優先順位は **CLIオプション (`-p`/`--port`, `-H`/`--host`) > 環境変数 (`AGENT_GATEWAY_PORT`/`PORT`) > 設定ファイル (`config.toml`) > デフォルト (`0.0.0.0:3000`)** です。

## systemd によるログイン時自動起動 (`systemctl --user`)

`systemd/agent-gateway.service` を使って、ユーザーログイン時に自動起動させることができます。

```bash
# 1. バイナリを ~/.cargo/bin/agent-gateway にインストール
cargo install --path .

# 2. ユーザー用 systemd ディレクトリへユニットファイルを配置
mkdir -p ~/.config/systemd/user
cp systemd/agent-gateway.service ~/.config/systemd/user/

# 3. ユニットを再読込して自動起動を有効化＆即時起動
# （※ 非PAM経由のシェル等で $XDG_RUNTIME_DIR / $DBUS_SESSION_BUS_ADDRESS が未定義の場合は以下を付与）
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=${XDG_RUNTIME_DIR}/bus}"

systemctl --user daemon-reload
systemctl --user enable --now agent-gateway.service

# 4. ステータスとログの確認
systemctl --user status agent-gateway.service
journalctl --user -u agent-gateway.service -f
```
（※ ポート番号などを変更したい場合は、`~/.config/agent-gateway/config.toml` に記載することでサービス再起動時に自動で反映されます）

## API エンドポイント

### 1. エージェントの起動 (`POST /agents`)

```bash
curl -X POST http://localhost:3000/agents \
  -H "Content-Type: application/json" \
  -d '{
    "name": "claude-agent",
    "command": "claude",
    "args": [],
    "cwd": "/path/to/workspace",
    "rows": 24,
    "cols": 80
  }'
```

### 2. 実行中エージェントの一覧取得 (`GET /agents`)

```bash
# 実行中のみ（デフォルト）
curl http://localhost:3000/agents

# 終了済みも含めてすべて取得
curl "http://localhost:3000/agents?status=all"
```

### 3. エージェントの標準入力・標準出力を取得 (`GET /agents/:id/io`)

```bash
# 累積された stdin / stdout 文字列と時系列レコードを取得
curl http://localhost:3000/agents/<id>/io

# クエリパラメータによる絞り込み（stream=stdin|stdout, since_seq, limit）
curl "http://localhost:3000/agents/<id>/io?stream=stdout&since_seq=10&limit=50"
```

### 4. 標準入力への送信 (`POST /agents/:id/input`)

```bash
curl -X POST http://localhost:3000/agents/<id>/input \
  -H "Content-Type: application/json" \
  -d '{"data": "hello\n"}'
```

### 5. PTYウィンドウサイズの変更 (`POST /agents/:id/resize`)

```bash
curl -X POST http://localhost:3000/agents/<id>/resize \
  -H "Content-Type: application/json" \
  -d '{"rows": 40, "cols": 120}'
```

### 6. エージェントの停止 (`DELETE /agents/:id`)

```bash
curl -X DELETE http://localhost:3000/agents/<id>
```

### 7. WebSocketによるリアルタイム双方向ストリーム (`GET /agents/:id/ws`)

- 接続時に既存の入出力履歴とステータスを送信し、以降の `stdin` / `stdout` イベントをリアルタイム配信します。
- クライアントからは以下のJSONまたは生テキスト・バイナリを送信して操作できます:
  - 入力送信: `{"type": "input", "data": "yes\n"}`（または生テキスト送信）
  - リサイズ: `{"type": "resize", "rows": 40, "cols": 120}`


