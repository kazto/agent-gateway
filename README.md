# AIエージェントゲートウェイ (agent-gateway)

Rust製の軽量・シングルバイナリなAIエージェント管理ゲートウェイです。PTY（擬似端末）を介して対話型CLIエージェントを起動・管理し、HTTP / WebSocket API経由で標準入力・標準出力の取得や操作を行えます。

## コンセプト・特徴

- デフォルトで `3000` ポートで待ち受ける（CLIオプション `-p`/`--port`、環境変数 `AGENT_GATEWAY_PORT`/`PORT`、設定ファイルで変更可能）
- **常時有効なトークン認証**（`GET /health` を除く全HTTP/WebSocket APIで `Authorization: Bearer <token>` / `X-API-Key: <token>` / `?token=<token>` が必須。未設定時は起動時に256bitランダムトークンを自動生成し、`~/.config/agent-gateway/token` に `0600` 権限で保存＆ログ出力）
- PTY（擬似端末）対応により、Claude Code・Copilot CLIなどの対話型TUIエージェントもそのまま動作
- 現在実行中のAIエージェントを一覧できる
- 任意のAIエージェントの標準入力・標準出力の取得および標準入力への送信（HTTP / WebSocket双方向通信）ができる

## 起動方法・設定

```bash
# デフォルト起動（0.0.0.0:3000、トークン未指定時はランダム生成して ~/.config/agent-gateway/token に保存）
cargo run

# CLIオプションでポート・ホスト・トークンを指定
cargo run -- -p 4000 --host 127.0.0.1 --token my-secret-token

# 環境変数で指定
AGENT_GATEWAY_PORT=4000 AGENT_GATEWAY_HOST=127.0.0.1 AGENT_GATEWAY_TOKEN=my-secret-token ./target/release/agent-gateway

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
agent = "claude" # POST /agents で agent 未指定時に使われるデフォルトエージェント
token = "your-secret-token" # 省略時は起動ごとにランダム生成され ~/.config/agent-gateway/token に保存されます

# 必要に応じてエージェントごとのコマンド・引数を定義可能（未定義でも PATH / mise / bun / cargo から自動解決）
[agents.claude]
command = "claude"
args = []

[agents.codex]
command = "codex"
args = []
```
優先順位は **CLIオプション (`-p`/`--port`, `-H`/`--host`, `-a`/`--agent`, `-t`/`--token`) > 環境変数 (`AGENT_GATEWAY_PORT`/`PORT`, `AGENT_GATEWAY_DEFAULT_AGENT`, `AGENT_GATEWAY_TOKEN`) > 設定ファイル (`config.toml`) > デフォルト (`0.0.0.0:3000`, `agent = "claude"`, トークン自動生成)** です。

## 認証について

`GET /health` を除くすべてのエンドポイントで認証が必要です。以下のいずれかの方法でトークンを渡してください：

1. `Authorization: Bearer <TOKEN>` ヘッダ（推奨）
2. `X-API-Key: <TOKEN>` ヘッダ
3. `?token=<TOKEN>` クエリパラメータ（WebSocket接続時などに便利）

同一マシン上のプロセス・AIエージェントからは、`~/.config/agent-gateway/token`（パーミッション `0600`）を読み取ることで現在有効なトークンを取得できます：
```bash
TOKEN="$(tr -d '\r\n' < ~/.config/agent-gateway/token)"
```

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

（※ 以下の例では `TOKEN="$(tr -d '\r\n' < ~/.config/agent-gateway/token)"` が設定されている前提です）

### 1. エージェントの起動 (`POST /agents`)

```bash
# デフォルトエージェント（config.toml の [default] agent）で起動
curl -X POST http://localhost:3000/agents \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{
    "cwd": "/path/to/workspace"
  }'

# エージェント種別（claude / codex / copilot / gemini など）を明示して起動
curl -X POST http://localhost:3000/agents \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{
    "agent": "codex",
    "name": "codex-reviewer",
    "cwd": "/path/to/workspace",
    "rows": 30,
    "cols": 100
  }'
```

### 2. 実行中エージェントの一覧取得 (`GET /agents`)

```bash
# 実行中のみ（デフォルト）
curl -H "Authorization: Bearer ${TOKEN}" http://localhost:3000/agents

# 終了済みも含めてすべて取得
curl -H "Authorization: Bearer ${TOKEN}" "http://localhost:3000/agents?status=all"
```

### 3. 現在のターミナル画面スナップショットの取得 (`GET /agents/:id/screen`)

ANSIエスケープシーケンス（カーソル移動・色コード等）を `vt100` で解釈し、**今画面に表示されているプレーンテキスト**とカーソル位置を軽量に取得します。呼び出し側のAIエージェントが画面状態（フォルダ信頼確認、ツール実行承認、入力待ちなど）を判断するのに最適です。

```bash
curl -H "Authorization: Bearer ${TOKEN}" http://localhost:3000/agents/<id>/screen
```

### 4. エージェントの標準入力・標準出力を取得 (`GET /agents/:id/io`)

```bash
# 描画済み画面 (screen) + 累積された生 stdin / stdout 文字列と時系列レコードを取得
curl -H "Authorization: Bearer ${TOKEN}" http://localhost:3000/agents/<id>/io

# クエリパラメータによる絞り込み（stream=stdin|stdout, since_seq, limit）
curl -H "Authorization: Bearer ${TOKEN}" "http://localhost:3000/agents/<id>/io?stream=stdout&since_seq=10&limit=50"
```

### 5. 標準入力への送信 (`POST /agents/:id/input`)

```bash
curl -X POST http://localhost:3000/agents/<id>/input \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"data": "hello\r"}'
```

### 6. PTYウィンドウサイズの変更 (`POST /agents/:id/resize`)

```bash
curl -X POST http://localhost:3000/agents/<id>/resize \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d '{"rows": 40, "cols": 120}'
```

### 7. エージェントの停止 (`POST /agents/:id/stop` または `DELETE /agents/:id`)

```bash
curl -X POST http://localhost:3000/agents/<id>/stop \
  -H "Authorization: Bearer ${TOKEN}"
```

### 8. WebSocketによるリアルタイム双方向ストリーム (`GET /agents/:id/ws`)

- `Authorization: Bearer <TOKEN>` ヘッダ、`X-API-Key: <TOKEN>` ヘッダ、または `ws://localhost:3000/agents/<id>/ws?token=<TOKEN>` クエリパラメータで接続します。
- 接続時に既存の入出力履歴とステータスを送信し、以降の `stdin` / `stdout` イベントをリアルタイム配信します。
- クライアントからは以下のJSONまたは生テキスト・バイナリを送信して操作できます:
  - 入力送信: `{"type": "input", "data": "yes\n"}`（または生テキスト送信）
  - リサイズ: `{"type": "resize", "rows": 40, "cols": 120}`


