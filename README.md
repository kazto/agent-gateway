# AIエージェントゲートウェイ (agent-gateway)

Rust製の軽量・シングルバイナリなAIエージェント管理ゲートウェイです。PTY（擬似端末）を介して対話型CLIエージェントを起動・管理し、HTTP / WebSocket API経由で標準入力・標準出力の取得や操作を行えます。

## コンセプト・特徴

- デフォルトで `3000` ポートで待ち受ける（環境変数 `PORT` で変更可能）
- PTY（擬似端末）対応により、Claude Code・Copilot CLIなどの対話型TUIエージェントもそのまま動作
- 現在実行中のAIエージェントを一覧できる
- 任意のAIエージェントの標準入力・標準出力の取得および標準入力への送信（HTTP / WebSocket双方向通信）ができる

## 起動方法

```bash
# 開発実行（デフォルト: 0.0.0.0:3000）
cargo run

# リリースビルド（シングルバイナリ生成）
cargo build --release
./target/release/agent-gateway
```

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


