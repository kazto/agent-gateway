use crate::{
    config::{default_agent_name, generate_token, AgentProfile, ServerConfig},
    models::{AgentStatus, AgentSummary, SpawnAgentRequest, StreamKind},
    session::{AgentSession, IoBuffer},
};
use chrono::Utc;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path as FsPath, PathBuf},
    sync::{atomic::AtomicU64, Arc, Mutex},
};
use tokio::sync::{broadcast, RwLock};
use uuid::Uuid;

#[derive(Clone)]
pub struct AgentManager {
    sessions: Arc<RwLock<HashMap<Uuid, Arc<AgentSession>>>>,
    default_agent: String,
    token: Arc<String>,
    agent_profiles: Arc<HashMap<String, AgentProfile>>,
}

impl Default for AgentManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentManager {
    pub fn new() -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            default_agent: default_agent_name(),
            token: Arc::new(generate_token()),
            agent_profiles: Arc::new(HashMap::new()),
        }
    }

    pub fn with_config(config: &ServerConfig) -> Self {
        let token = config
            .token
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(ToOwned::to_owned)
            .unwrap_or_else(generate_token);
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            default_agent: config.default_agent.clone(),
            token: Arc::new(token),
            agent_profiles: Arc::new(config.agents.clone()),
        }
    }

    pub fn default_agent(&self) -> &str {
        &self.default_agent
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    fn resolve_agent_spec(
        &self,
        req: &SpawnAgentRequest,
    ) -> Result<(String, String, Vec<String>, HashMap<String, String>), String> {
        let agent_name = match (&req.agent, &req.command) {
            (Some(a), _) => a.trim().to_string(),
            (None, Some(c)) => c.trim().to_string(),
            (None, None) => self.default_agent.trim().to_string(),
        };

        if agent_name.is_empty() {
            return Err("agent or command must not be empty".to_string());
        }

        let mut env = HashMap::new();
        let (command, args) = if let Some(explicit_cmd) = &req.command {
            let trimmed = explicit_cmd.trim();
            if trimmed.is_empty() {
                return Err("command must not be empty".to_string());
            }
            (trimmed.to_string(), req.args.clone())
        } else if let Some(profile) = self.agent_profiles.get(&agent_name) {
            env.extend(profile.env.clone());
            let mut merged_args = profile.args.clone();
            merged_args.extend(req.args.clone());
            (profile.command.clone(), merged_args)
        } else {
            (agent_name.clone(), req.args.clone())
        };

        env.extend(req.env.clone());
        Ok((agent_name, command, args, env))
    }

    pub async fn spawn(&self, req: SpawnAgentRequest) -> Result<AgentSummary, String> {
        let (agent_name, raw_command, args, env) = self.resolve_agent_spec(&req)?;
        let augmented_path = build_augmented_path(env.get("PATH").map(String::as_str));
        let resolved_command = resolve_executable(&raw_command, &augmented_path);

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

        let mut cmd = CommandBuilder::new(&resolved_command);
        cmd.args(&args);
        if let Some(ref cwd) = req.cwd {
            cmd.cwd(cwd);
        }
        if !env.contains_key("TERM") {
            cmd.env("TERM", "xterm-256color");
        }
        if !env.contains_key("PATH") {
            if let Some(ref path_val) = augmented_path {
                cmd.env("PATH", path_val);
            }
        }
        for (k, v) in &env {
            cmd.env(k, v);
        }

        let mut child = pair.slave.spawn_command(cmd).map_err(|e| {
            format!(
                "failed to spawn agent '{}' (command '{}'): {e}",
                agent_name, resolved_command
            )
        })?;
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
        let name = req.name.unwrap_or_else(|| agent_name.clone());
        let summary = AgentSummary {
            id,
            name,
            agent: agent_name,
            command: resolved_command,
            args,
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
            io_buffer: Mutex::new(IoBuffer::new(rows, cols)),
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

fn build_augmented_path(explicit_path: Option<&str>) -> Option<String> {
    let base = explicit_path
        .map(String::from)
        .or_else(|| std::env::var("PATH").ok());
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(ref h) = home {
        let h_path = PathBuf::from(h);
        for rel in [
            ".local/share/mise/shims",
            ".bun/bin",
            ".local/bin",
            ".cargo/bin",
        ] {
            let candidate = h_path.join(rel);
            if candidate.is_dir() {
                dirs.push(candidate);
            }
        }
    }
    if let Some(ref b) = base {
        for p in std::env::split_paths(b) {
            if !dirs.contains(&p) {
                dirs.push(p);
            }
        }
    }
    std::env::join_paths(dirs)
        .ok()
        .and_then(|os| os.into_string().ok())
        .or(base)
}

fn resolve_executable(command: &str, search_path: &Option<String>) -> String {
    let cmd_path = FsPath::new(command);
    if cmd_path.components().count() > 1 {
        return command.to_string();
    }
    if let Some(ref path_str) = search_path {
        for dir in std::env::split_paths(path_str) {
            let candidate = dir.join(command);
            if candidate.is_file() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    command.to_string()
}
