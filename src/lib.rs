use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path as FsPath, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

const MAX_IO_RECORDS: usize = 2000;
const MAX_RAW_BYTES: usize = 1024 * 1024; // 1 MiB per stream buffer

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: IpAddr,
    #[serde(default = "default_port")]
    pub port: u16,
}

fn default_host() -> IpAddr {
    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
}

fn default_port() -> u16 {
    3000
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct SectionConfig {
    host: Option<IpAddr>,
    port: Option<u16>,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    default: SectionConfig,
    host: Option<IpAddr>,
    port: Option<u16>,
}

impl FileConfig {
    fn resolved_host(&self) -> Option<IpAddr> {
        self.default.host.or(self.host)
    }

    fn resolved_port(&self) -> Option<u16> {
        self.default.port.or(self.port)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliOutcome {
    Run(ServerConfig),
    Help(String),
    Version(String),
}

impl ServerConfig {
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }

    pub fn usage() -> String {
        concat!(
            "Usage: agent-gateway [OPTIONS]\n\n",
            "Options:\n",
            "  -p, --port <PORT>      Port number to listen on (default: 3000, env: AGENT_GATEWAY_PORT / PORT)\n",
            "  -H, --host <HOST>      Host IP to bind (default: 0.0.0.0, env: AGENT_GATEWAY_HOST / HOST)\n",
            "  -c, --config <PATH>    Path to TOML config file (default: ~/.config/agent-gateway/config.toml on Mac/Linux, %USERPROFILE%\\.config\\agent-gateway\\config.toml on Windows)\n",
            "  -h, --help             Print help\n",
            "  -V, --version          Print version\n"
        )
        .to_string()
    }

    pub fn default_config_path(env_lookup: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
        let base_dir = if cfg!(windows) {
            env_lookup("USERPROFILE")
                .or_else(|| env_lookup("USER_PROFILE"))
                .or_else(|| env_lookup("HOME"))
        } else {
            env_lookup("HOME")
                .or_else(|| env_lookup("USERPROFILE"))
                .or_else(|| env_lookup("USER_PROFILE"))
        }?;
        Some(
            PathBuf::from(base_dir)
                .join(".config")
                .join("agent-gateway")
                .join("config.toml"),
        )
    }

    pub fn from_args_and_env<I, S>(
        args: I,
        env_lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<CliOutcome, String>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut iter = args.into_iter().map(Into::into);
        let _program = iter.next();

        let mut cli_port: Option<u16> = None;
        let mut cli_host: Option<IpAddr> = None;
        let mut cli_config: Option<PathBuf> = None;

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "-h" | "--help" => return Ok(CliOutcome::Help(Self::usage())),
                "-V" | "--version" => {
                    return Ok(CliOutcome::Version(format!(
                        "{} {}",
                        env!("CARGO_PKG_NAME"),
                        env!("CARGO_PKG_VERSION")
                    )))
                }
                "-p" | "--port" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "missing value for --port".to_string())?;
                    cli_port = Some(
                        val.parse::<u16>()
                            .map_err(|_| format!("invalid port number: '{val}'"))?,
                    );
                }
                _ if arg.starts_with("--port=") => {
                    let val = &arg["--port=".len()..];
                    cli_port = Some(
                        val.parse::<u16>()
                            .map_err(|_| format!("invalid port number: '{val}'"))?,
                    );
                }
                "-H" | "--host" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "missing value for --host".to_string())?;
                    cli_host = Some(
                        val.parse::<IpAddr>()
                            .map_err(|_| format!("invalid host IP address: '{val}'"))?,
                    );
                }
                _ if arg.starts_with("--host=") => {
                    let val = &arg["--host=".len()..];
                    cli_host = Some(
                        val.parse::<IpAddr>()
                            .map_err(|_| format!("invalid host IP address: '{val}'"))?,
                    );
                }
                "-c" | "--config" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "missing value for --config".to_string())?;
                    cli_config = Some(PathBuf::from(val));
                }
                _ if arg.starts_with("--config=") => {
                    let val = &arg["--config=".len()..];
                    cli_config = Some(PathBuf::from(val));
                }
                other => return Err(format!("unknown option: '{other}'\n\n{}", Self::usage())),
            }
        }

        let config_path = cli_config.or_else(|| {
            env_lookup("AGENT_GATEWAY_CONFIG").map(PathBuf::from)
        });

        let file_cfg = if let Some(path) = config_path {
            Self::load_file(&path)?
        } else {
            Self::load_default_file(&env_lookup)?
        };

        let env_port = env_lookup("AGENT_GATEWAY_PORT")
            .or_else(|| env_lookup("PORT"))
            .map(|v| {
                v.parse::<u16>()
                    .map_err(|_| format!("invalid port in environment variable: '{v}'"))
            })
            .transpose()?;

        let env_host = env_lookup("AGENT_GATEWAY_HOST")
            .or_else(|| env_lookup("HOST"))
            .map(|v| {
                v.parse::<IpAddr>()
                    .map_err(|_| format!("invalid host IP in environment variable: '{v}'"))
            })
            .transpose()?;

        let cfg = ServerConfig {
            host: cli_host
                .or(env_host)
                .or_else(|| file_cfg.resolved_host())
                .unwrap_or_else(default_host),
            port: cli_port
                .or(env_port)
                .or_else(|| file_cfg.resolved_port())
                .unwrap_or_else(default_port),
        };

        Ok(CliOutcome::Run(cfg))
    }

    fn load_file(path: &FsPath) -> Result<FileConfig, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read config file '{}': {e}", path.display()))?;
        toml::from_str(&content)
            .map_err(|e| format!("failed to parse TOML config file '{}': {e}", path.display()))
    }

    fn load_default_file(env_lookup: &impl Fn(&str) -> Option<String>) -> Result<FileConfig, String> {
        if let Some(cfg_path) = Self::default_config_path(env_lookup) {
            if cfg_path.is_file() {
                return Self::load_file(&cfg_path);
            }
        }
        Ok(FileConfig::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Stdin,
    Stdout,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IoRecord {
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    pub stream: StreamKind,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Running,
    Exited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub id: Uuid,
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub pid: Option<u32>,
    pub status: AgentStatus,
    pub exit_code: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub exited_at: Option<DateTime<Utc>>,
    pub rows: u16,
    pub cols: u16,
}

struct IoBuffer {
    records: VecDeque<IoRecord>,
    stdin_raw: VecDeque<u8>,
    stdout_raw: VecDeque<u8>,
}

impl IoBuffer {
    fn new() -> Self {
        Self {
            records: VecDeque::new(),
            stdin_raw: VecDeque::new(),
            stdout_raw: VecDeque::new(),
        }
    }

    fn push(&mut self, record: IoRecord, raw: &[u8]) {
        let target = match record.stream {
            StreamKind::Stdin => &mut self.stdin_raw,
            StreamKind::Stdout => &mut self.stdout_raw,
        };
        target.extend(raw.iter().copied());
        while target.len() > MAX_RAW_BYTES {
            target.pop_front();
        }

        self.records.push_back(record);
        while self.records.len() > MAX_IO_RECORDS {
            self.records.pop_front();
        }
    }

    fn stdin_string(&self) -> String {
        let bytes: Vec<u8> = self.stdin_raw.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn stdout_string(&self) -> String {
        let bytes: Vec<u8> = self.stdout_raw.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

pub struct AgentSession {
    meta: RwLock<AgentSummary>,
    io_buffer: Mutex<IoBuffer>,
    next_seq: AtomicU64,
    writer: Mutex<Box<dyn Write + Send>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    tx: broadcast::Sender<IoRecord>,
}

impl AgentSession {
    pub async fn summary(&self) -> AgentSummary {
        self.meta.read().await.clone()
    }

    pub fn write_stdin(&self, input: &[u8]) -> Result<IoRecord, String> {
        {
            let mut writer = self.writer.lock().map_err(|_| "writer lock poisoned")?;
            writer
                .write_all(input)
                .map_err(|e| format!("failed to write to pty stdin: {e}"))?;
            writer
                .flush()
                .map_err(|e| format!("failed to flush pty stdin: {e}"))?;
        }
        Ok(self.record_io(StreamKind::Stdin, input))
    }

    fn record_io(&self, stream: StreamKind, raw: &[u8]) -> IoRecord {
        let seq = self.next_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let record = IoRecord {
            seq,
            timestamp: Utc::now(),
            stream,
            data: String::from_utf8_lossy(raw).into_owned(),
        };
        if let Ok(mut buf) = self.io_buffer.lock() {
            buf.push(record.clone(), raw);
        }
        let _ = self.tx.send(record.clone());
        record
    }

    pub async fn resize(&self, rows: u16, cols: u16) -> Result<AgentSummary, String> {
        {
            let master = self.master.lock().map_err(|_| "master lock poisoned")?;
            master
                .resize(PtySize {
                    rows,
                    cols,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(|e| format!("failed to resize pty: {e}"))?;
        }
        let mut meta = self.meta.write().await;
        meta.rows = rows;
        meta.cols = cols;
        Ok(meta.clone())
    }

    pub fn kill(&self) -> Result<(), String> {
        let mut killer = self.killer.lock().map_err(|_| "killer lock poisoned")?;
        killer
            .kill()
            .map_err(|e| format!("failed to kill process: {e}"))
    }
}

#[derive(Clone, Default)]
pub struct AgentManager {
    sessions: Arc<RwLock<HashMap<Uuid, Arc<AgentSession>>>>,
}

#[derive(Debug, Deserialize)]
pub struct SpawnAgentRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub name: Option<String>,
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub rows: Option<u16>,
    pub cols: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StatusFilter {
    Running,
    Exited,
    All,
}

#[derive(Debug, Deserialize)]
pub struct ListAgentsQuery {
    pub status: Option<StatusFilter>,
}

#[derive(Debug, Deserialize)]
pub struct IoQuery {
    pub stream: Option<StreamKind>,
    pub since_seq: Option<u64>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentIoResponse {
    pub id: Uuid,
    pub status: AgentStatus,
    pub stdin: String,
    pub stdout: String,
    pub records: Vec<IoRecord>,
}

#[derive(Debug, Deserialize)]
pub struct SendInputRequest {
    pub data: String,
}

#[derive(Debug, Deserialize)]
pub struct ResizePtyRequest {
    pub rows: u16,
    pub cols: u16,
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
    Error {
        message: String,
    },
}

impl AgentManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn spawn(&self, req: SpawnAgentRequest) -> Result<AgentSummary, String> {
        if req.command.trim().is_empty() {
            return Err("command must not be empty".to_string());
        }

        let rows = req.rows.unwrap_or(24);
        let cols = req.cols.unwrap_or(80);
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("failed to open pty: {e}"))?;

        let mut cmd = CommandBuilder::new(&req.command);
        cmd.args(&req.args);
        if let Some(ref cwd) = req.cwd {
            cmd.cwd(cwd);
        }
        if !req.env.contains_key("TERM") {
            cmd.env("TERM", "xterm-256color");
        }
        for (k, v) in &req.env {
            cmd.env(k, v);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("failed to spawn command '{}': {e}", req.command))?;
        drop(pair.slave);

        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("failed to clone pty reader: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("failed to take pty writer: {e}"))?;

        let id = Uuid::new_v4();
        let name = req.name.unwrap_or_else(|| req.command.clone());
        let summary = AgentSummary {
            id,
            name,
            command: req.command,
            args: req.args,
            cwd: req.cwd,
            pid,
            status: AgentStatus::Running,
            exit_code: None,
            created_at: Utc::now(),
            exited_at: None,
            rows,
            cols,
        };

        let (tx, _) = broadcast::channel(1024);
        let session = Arc::new(AgentSession {
            meta: RwLock::new(summary.clone()),
            io_buffer: Mutex::new(IoBuffer::new()),
            next_seq: AtomicU64::new(0),
            writer: Mutex::new(writer),
            master: Mutex::new(pair.master),
            killer: Mutex::new(killer),
            tx,
        });

        self.sessions.write().await.insert(id, session.clone());

        // Spawn reader thread for PTY stdout
        let reader_session = session.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        reader_session.record_io(StreamKind::Stdout, &buf[..n]);
                    }
                    Err(_) => break,
                }
            }
        });

        // Wait for child process termination
        let wait_session = session;
        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let exit_code = child.wait().ok().map(|status| status.exit_code());
            handle.block_on(async move {
                let mut meta = wait_session.meta.write().await;
                meta.status = AgentStatus::Exited;
                meta.exit_code = exit_code;
                meta.exited_at = Some(Utc::now());
            });
        });

        Ok(summary)
    }

    pub async fn list(&self, status_filter: Option<AgentStatus>) -> Vec<AgentSummary> {
        let sessions: Vec<Arc<AgentSession>> = {
            let map = self.sessions.read().await;
            map.values().cloned().collect()
        };
        let mut list = Vec::with_capacity(sessions.len());
        for s in sessions {
            let summary = s.summary().await;
            if let Some(ref filter) = status_filter {
                if &summary.status != filter {
                    continue;
                }
            }
            list.push(summary);
        }
        list.sort_by_key(|s| s.created_at);
        list
    }

    pub async fn get(&self, id: Uuid) -> Option<Arc<AgentSession>> {
        self.sessions.read().await.get(&id).cloned()
    }

    pub async fn remove(&self, id: Uuid) -> Option<Arc<AgentSession>> {
        self.sessions.write().await.remove(&id)
    }
}

pub fn app(manager: AgentManager) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/agents", get(list_agents).post(spawn_agent))
        .route("/agents/:id", get(get_agent).delete(stop_agent))
        .route("/agents/:id/io", get(get_agent_io))
        .route("/agents/:id/input", post(send_agent_input))
        .route("/agents/:id/resize", post(resize_agent_pty))
        .route("/agents/:id/ws", get(ws_agent))
        .with_state(manager)
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

fn error_response(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(ErrorResponse { error: msg.into() })).into_response()
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

async fn get_agent_io(
    State(manager): State<AgentManager>,
    Path(id): Path<Uuid>,
    Query(query): Query<IoQuery>,
) -> Response {
    let Some(session) = manager.get(id).await else {
        return error_response(StatusCode::NOT_FOUND, "agent not found");
    };

    let summary = session.summary().await;
    let (stdin, stdout, records) = {
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
        (buf.stdin_string(), buf.stdout_string(), filtered)
    };

    Json(AgentIoResponse {
        id: summary.id,
        status: summary.status,
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
                // Accept either structured JSON command or raw text as stdin
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
