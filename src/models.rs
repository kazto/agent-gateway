use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

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
    pub agent: String,
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

#[derive(Debug, Default, Deserialize)]
pub struct SpawnAgentRequest {
    pub agent: Option<String>,
    pub command: Option<String>,
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
    pub screen: String,
    pub stdin: String,
    pub stdout: String,
    pub records: Vec<IoRecord>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AgentScreenResponse {
    pub id: Uuid,
    pub status: AgentStatus,
    pub rows: u16,
    pub cols: u16,
    pub cursor_row: u16,
    pub cursor_col: u16,
    pub seq: u64,
    pub screen: String,
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
