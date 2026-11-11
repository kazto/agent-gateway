use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path as FsPath, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct AgentProfile {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: IpAddr,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_agent_name")]
    pub default_agent: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub agents: HashMap<String, AgentProfile>,
}

pub(crate) fn default_host() -> IpAddr {
    IpAddr::V4(Ipv4Addr::UNSPECIFIED)
}

pub(crate) fn default_port() -> u16 {
    3000
}

pub(crate) fn default_agent_name() -> String {
    "claude".to_string()
}

pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            default_agent: default_agent_name(),
            token: None,
            agents: HashMap::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct SectionConfig {
    host: Option<IpAddr>,
    port: Option<u16>,
    agent: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    default: SectionConfig,
    host: Option<IpAddr>,
    port: Option<u16>,
    agent: Option<String>,
    token: Option<String>,
    #[serde(default)]
    agents: HashMap<String, AgentProfile>,
}

impl FileConfig {
    fn resolved_host(&self) -> Option<IpAddr> {
        self.default.host.or(self.host)
    }

    fn resolved_port(&self) -> Option<u16> {
        self.default.port.or(self.port)
    }

    fn resolved_agent(&self) -> Option<String> {
        self.default.agent.clone().or_else(|| self.agent.clone())
    }

    fn resolved_token(&self) -> Option<String> {
        self.default
            .token
            .clone()
            .or_else(|| self.token.clone())
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
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
            "  -a, --agent <AGENT>    Default agent name/command (default: claude, env: AGENT_GATEWAY_DEFAULT_AGENT)\n",
            "  -t, --token <TOKEN>    Auth token (env: AGENT_GATEWAY_TOKEN; auto-generated if omitted)\n",
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

    pub fn default_token_path(env_lookup: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
        Self::default_config_path(env_lookup).map(|p| p.with_file_name("token"))
    }

    pub fn write_token_file(path: &FsPath, token: &str) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "failed to create directory '{}' for token file: {e}",
                    parent.display()
                )
            })?;
        }
        std::fs::write(path, format!("{token}\n"))
            .map_err(|e| format!("failed to write token file '{}': {e}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(path, perms).map_err(|e| {
                format!(
                    "failed to set 0600 permissions on token file '{}': {e}",
                    path.display()
                )
            })?;
        }
        Ok(())
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
        let mut cli_agent: Option<String> = None;
        let mut cli_token: Option<String> = None;
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
                "-a" | "--agent" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "missing value for --agent".to_string())?;
                    cli_agent = Some(val);
                }
                _ if arg.starts_with("--agent=") => {
                    let val = &arg["--agent=".len()..];
                    cli_agent = Some(val.to_string());
                }
                "-t" | "--token" => {
                    let val = iter
                        .next()
                        .ok_or_else(|| "missing value for --token".to_string())?;
                    let trimmed = val.trim().to_string();
                    if trimmed.is_empty() {
                        return Err("--token must not be empty".to_string());
                    }
                    cli_token = Some(trimmed);
                }
                _ if arg.starts_with("--token=") => {
                    let val = arg["--token=".len()..].trim().to_string();
                    if val.is_empty() {
                        return Err("--token must not be empty".to_string());
                    }
                    cli_token = Some(val);
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

        let env_agent = env_lookup("AGENT_GATEWAY_DEFAULT_AGENT")
            .or_else(|| env_lookup("AGENT_GATEWAY_AGENT"));

        let env_token = env_lookup("AGENT_GATEWAY_TOKEN")
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());

        let cfg = ServerConfig {
            host: cli_host
                .or(env_host)
                .or_else(|| file_cfg.resolved_host())
                .unwrap_or_else(default_host),
            port: cli_port
                .or(env_port)
                .or_else(|| file_cfg.resolved_port())
                .unwrap_or_else(default_port),
            default_agent: cli_agent
                .or(env_agent)
                .or_else(|| file_cfg.resolved_agent())
                .unwrap_or_else(default_agent_name),
            token: cli_token
                .or(env_token)
                .or_else(|| file_cfg.resolved_token()),
            agents: file_cfg.agents,
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
