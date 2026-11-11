use crate::{
    manager::AgentManager,
    models::{
        AgentIoResponse, AgentScreenResponse, AgentStatus, IoQuery, IoRecord, ListAgentsQuery,
        ResizePtyRequest, SendInputRequest, SpawnAgentRequest, StatusFilter,
    },
    session::AgentSession,
};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, Request, State,
    },
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{atomic::Ordering, Arc};
use uuid::Uuid;

pub fn app(manager: AgentManager) -> Router {
    let protected = Router::new()
        .route("/agents", get(list_agents).post(spawn_agent))
        .route("/agents/:id", get(get_agent).delete(stop_agent))
        .route("/agents/:id/stop", post(stop_agent))
        .route("/agents/:id/screen", get(get_agent_screen))
        .route("/agents/:id/io", get(get_agent_io))
        .route("/agents/:id/input", post(send_agent_input))
        .route("/agents/:id/resize", post(resize_agent_pty))
        .route("/agents/:id/ws", get(ws_agent))
        .route_layer(middleware::from_fn_with_state(
            manager.clone(),
            require_auth,
        ));

    Router::new()
        .route("/health", get(health))
        .merge(protected)
        .with_state(manager)
}

async fn require_auth(
    State(manager): State<AgentManager>,
    req: Request,
    next: Next,
) -> Response {
    let expected = manager.token();
    if let Some(provided) = extract_token(req.headers(), req.uri().query()) {
        if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
            return next.run(req).await;
        }
    }
    error_response(
        StatusCode::UNAUTHORIZED,
        "unauthorized: valid Bearer token or X-API-Key required",
    )
}

fn extract_token<'a>(headers: &'a HeaderMap, query: Option<&'a str>) -> Option<&'a str> {
    if let Some(auth_val) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        let trimmed = auth_val.trim();
        if trimmed.len() > 7 && trimmed[..7].eq_ignore_ascii_case("bearer ") {
            let token = trimmed[7..].trim();
            if !token.is_empty() {
                return Some(token);
            }
        }
    }

    if let Some(api_key) = headers.get("x-api-key").and_then(|v| v.to_str().ok()) {
        let token = api_key.trim();
        if !token.is_empty() {
            return Some(token);
        }
    }

    if let Some(q) = query {
        for pair in q.split('&') {
            if let Some(val) = pair.strip_prefix("token=") {
                let token = val.trim();
                if !token.is_empty() {
                    return Some(token);
                }
            }
        }
    }

    None
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (&x, &y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

fn error_response(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ErrorResponse { error: msg.into() })).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum WsClientCommand {
    Input { data: String },
    Resize { rows: u16, cols: u16 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum WsServerMessage {
    Io(IoRecord),
    Status {
        status: AgentStatus,
        exit_code: Option<u32>,
    },
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn list_agents(
    State(manager): State<AgentManager>,
    Query(query): Query<ListAgentsQuery>,
) -> impl IntoResponse {
    let status = match query.status.unwrap_or(StatusFilter::Running) {
        StatusFilter::Running => Some(AgentStatus::Running),
        StatusFilter::Exited => Some(AgentStatus::Exited),
        StatusFilter::All => None,
    };
    let agents = manager.list(status).await;
    Json(agents)
}

async fn spawn_agent(
    State(manager): State<AgentManager>,
    Json(req): Json<SpawnAgentRequest>,
) -> Response {
    match manager.spawn(req).await {
        Ok(summary) => (StatusCode::CREATED, Json(summary)).into_response(),
        Err(err) => error_response(StatusCode::BAD_REQUEST, err),
    }
}

async fn get_agent(
    State(manager): State<AgentManager>,
    Path(id): Path<Uuid>,
) -> Response {
    match manager.get(id).await {
        Some(session) => Json(session.summary().await).into_response(),
        None => error_response(StatusCode::NOT_FOUND, "agent not found"),
    }
}

async fn get_agent_screen(
    State(manager): State<AgentManager>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(session) = manager.get(id).await else {
        return error_response(StatusCode::NOT_FOUND, "agent not found");
    };

    let summary = session.summary().await;
    let seq = session.next_seq.load(Ordering::SeqCst);
    let (screen, (cursor_row, cursor_col)) = {
        let Ok(buf) = session.io_buffer.lock() else {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "io buffer lock poisoned");
        };
        buf.screen_contents()
    };

    Json(AgentScreenResponse {
        id: summary.id,
        status: summary.status,
        rows: summary.rows,
        cols: summary.cols,
        cursor_row,
        cursor_col,
        seq,
        screen,
    })
    .into_response()
}

async fn get_agent_io(
    State(manager): State<AgentManager>,
    Path(id): Path<Uuid>,
    Query(query): Query<IoQuery>,
) -> Response {
    let Some(session) = manager.get(id).await else {
        return error_response(StatusCode::NOT_FOUND, "agent not found");
    };

    let summary = session.summary().await;
    let (screen, stdin, stdout, records) = {
        let Ok(buf) = session.io_buffer.lock() else {
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "io buffer lock poisoned");
        };
        let since_seq = query.since_seq.unwrap_or(0);
        let mut filtered: Vec<IoRecord> = buf
            .records
            .iter()
            .filter(|r| r.seq > since_seq)
            .filter(|r| query.stream.map_or(true, |s| r.stream == s))
            .cloned()
            .collect();

        if let Some(limit) = query.limit {
            if filtered.len() > limit {
                filtered = filtered.split_off(filtered.len() - limit);
            }
        }
        let (screen, _) = buf.screen_contents();
        (screen, buf.stdin_string(), buf.stdout_string(), filtered)
    };

    Json(AgentIoResponse {
        id: summary.id,
        status: summary.status,
        screen,
        stdin,
        stdout,
        records,
    })
    .into_response()
}

async fn send_agent_input(
    State(manager): State<AgentManager>,
    Path(id): Path<Uuid>,
    Json(req): Json<SendInputRequest>,
) -> Response {
    let Some(session) = manager.get(id).await else {
        return error_response(StatusCode::NOT_FOUND, "agent not found");
    };

    if session.summary().await.status != AgentStatus::Running {
        return error_response(StatusCode::CONFLICT, "agent is no longer running");
    }

    match session.write_stdin(req.data.as_bytes()) {
        Ok(record) => Json(record).into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

async fn resize_agent_pty(
    State(manager): State<AgentManager>,
    Path(id): Path<Uuid>,
    Json(req): Json<ResizePtyRequest>,
) -> Response {
    let Some(session) = manager.get(id).await else {
        return error_response(StatusCode::NOT_FOUND, "agent not found");
    };

    match session.resize(req.rows, req.cols).await {
        Ok(summary) => Json(summary).into_response(),
        Err(err) => error_response(StatusCode::INTERNAL_SERVER_ERROR, err),
    }
}

async fn stop_agent(
    State(manager): State<AgentManager>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(session) = manager.get(id).await else {
        return error_response(StatusCode::NOT_FOUND, "agent not found");
    };

    let _ = session.kill();
    let mut meta = session.meta.write().await;
    meta.status = AgentStatus::Exited;
    if meta.exited_at.is_none() {
        meta.exited_at = Some(Utc::now());
    }
    Json(meta.clone()).into_response()
}

async fn ws_agent(
    ws: WebSocketUpgrade,
    State(manager): State<AgentManager>,
    Path(id): Path<Uuid>,
) -> Response {
    let Some(session) = manager.get(id).await else {
        return error_response(StatusCode::NOT_FOUND, "agent not found");
    };

    ws.on_upgrade(move |socket| handle_ws(socket, session))
}

async fn handle_ws(socket: WebSocket, session: Arc<AgentSession>) {
    let (mut sender, mut receiver) = socket.split();
    let mut rx = session.tx.subscribe();

    // Send existing history snapshot first
    let initial_records: Vec<IoRecord> = {
        if let Ok(buf) = session.io_buffer.lock() {
            buf.records.iter().cloned().collect()
        } else {
            Vec::new()
        }
    };
    let mut last_seq = 0u64;
    for rec in initial_records {
        last_seq = rec.seq;
        if let Ok(json) = serde_json::to_string(&WsServerMessage::Io(rec)) {
            if sender.send(Message::Text(json)).await.is_err() {
                return;
            }
        }
    }

    let summary = session.summary().await;
    if let Ok(json) = serde_json::to_string(&WsServerMessage::Status {
        status: summary.status,
        exit_code: summary.exit_code,
    }) {
        let _ = sender.send(Message::Text(json)).await;
    }

    let send_task = tokio::spawn(async move {
        while let Ok(record) = rx.recv().await {
            if record.seq <= last_seq {
                continue;
            }
            if let Ok(json) = serde_json::to_string(&WsServerMessage::Io(record)) {
                if sender.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        }
    });

    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(cmd) = serde_json::from_str::<WsClientCommand>(&text) {
                    match cmd {
                        WsClientCommand::Input { data } => {
                            let _ = session.write_stdin(data.as_bytes());
                        }
                        WsClientCommand::Resize { rows, cols } => {
                            let _ = session.resize(rows, cols).await;
                        }
                    }
                } else {
                    let _ = session.write_stdin(text.as_bytes());
                }
            }
            Message::Binary(bytes) => {
                let _ = session.write_stdin(&bytes);
            }
            Message::Close(_) => break,
            _ => {}
        }
    }

    send_task.abort();
}
