use crate::models::{AgentSummary, IoRecord, StreamKind};
use chrono::Utc;
use portable_pty::{ChildKiller, MasterPty, PtySize};
use std::{
    collections::VecDeque,
    io::Write,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};
use tokio::sync::{broadcast, RwLock};

const MAX_IO_RECORDS: usize = 2000;
const MAX_RAW_BYTES: usize = 1024 * 1024; // 1 MiB per stream buffer

pub(crate) struct IoBuffer {
    pub(crate) records: VecDeque<IoRecord>,
    stdin_raw: VecDeque<u8>,
    stdout_raw: VecDeque<u8>,
    vt_parser: vt100::Parser,
}

impl IoBuffer {
    pub(crate) fn new(rows: u16, cols: u16) -> Self {
        Self {
            records: VecDeque::new(),
            stdin_raw: VecDeque::new(),
            stdout_raw: VecDeque::new(),
            vt_parser: vt100::Parser::new(rows, cols, 1000),
        }
    }

    pub(crate) fn push(&mut self, record: IoRecord, raw: &[u8]) {
        if record.stream == StreamKind::Stdout {
            self.vt_parser.process(raw);
        }
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

    pub(crate) fn resize(&mut self, rows: u16, cols: u16) {
        self.vt_parser.set_size(rows, cols);
    }

    pub(crate) fn stdin_string(&self) -> String {
        let bytes: Vec<u8> = self.stdin_raw.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub(crate) fn stdout_string(&self) -> String {
        let bytes: Vec<u8> = self.stdout_raw.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    pub(crate) fn screen_contents(&self) -> (String, (u16, u16)) {
        let screen = self.vt_parser.screen();
        (screen.contents(), screen.cursor_position())
    }
}

pub struct AgentSession {
    pub(crate) meta: RwLock<AgentSummary>,
    pub(crate) io_buffer: Mutex<IoBuffer>,
    pub(crate) next_seq: AtomicU64,
    pub(crate) writer: Mutex<Box<dyn Write + Send>>,
    pub(crate) master: Mutex<Box<dyn MasterPty + Send>>,
    pub(crate) killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    pub(crate) tx: broadcast::Sender<IoRecord>,
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

    pub(crate) fn record_io(&self, stream: StreamKind, raw: &[u8]) -> IoRecord {
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
        if let Ok(mut buf) = self.io_buffer.lock() {
            buf.resize(rows, cols);
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
