//! Opt-in, local-only application performance measurements.
//!
//! Recording is disabled by default. When enabled, callers submit compact
//! operation timings and aggregate counts to a bounded channel. A dedicated
//! writer appends redacted JSONL without blocking parser, watcher, IPC, or UI
//! work. Logs rotate at a configurable size and never contain prompts, tool
//! arguments, session IDs, repository paths, or command output.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

const SCHEMA_VERSION: u32 = 1;
const CHANNEL_CAPACITY: usize = 4_096;
const MIN_LOG_MB: u64 = 1;
const MAX_LOG_MB: u64 = 1_024;
const CURRENT_LOG: &str = "performance-events-v1.jsonl";
const PREVIOUS_LOG: &str = "performance-events-v1.previous.jsonl";
const FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

enum WriterMessage {
    Event(Box<PerformanceEvent>),
    Flush(mpsc::Sender<()>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PerformanceEvent {
    pub schema_version: u32,
    pub app_version: String,
    pub platform: String,
    pub process_id: u32,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
    pub source: String,
    pub operation: String,
    pub duration_ms: f64,
    pub success: bool,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PerformanceStatus {
    pub enabled: bool,
    pub max_log_mb: u64,
    pub stored_bytes: u64,
    pub recorded_this_run: u64,
    pub dropped_this_run: u64,
}

pub struct PerformanceRecorder {
    enabled: AtomicBool,
    max_file_bytes: Arc<AtomicU64>,
    sender: Mutex<Option<mpsc::SyncSender<WriterMessage>>>,
    sequence: AtomicU64,
    recorded: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
}

impl Default for PerformanceRecorder {
    fn default() -> Self {
        Self {
            enabled: AtomicBool::new(false),
            max_file_bytes: Arc::new(AtomicU64::new(64 * 1024 * 1024)),
            sender: Mutex::new(None),
            sequence: AtomicU64::new(0),
            recorded: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl PerformanceRecorder {
    pub fn configure(&self, enabled: bool, max_log_mb: u64) {
        let max_log_mb = max_log_mb.clamp(MIN_LOG_MB, MAX_LOG_MB);
        self.max_file_bytes
            .store(max_log_mb * 1024 * 1024, Ordering::Release);
        if enabled {
            let available = self.ensure_writer();
            self.enabled.store(available, Ordering::Release);
        } else {
            // Stop accepting measurements before draining the writer so a
            // settings change cannot strand events behind the flush barrier.
            self.enabled.store(false, Ordering::Release);
            self.flush();
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    pub fn record_backend(
        &self,
        operation: impl Into<String>,
        started: Instant,
        success: bool,
        metadata: BTreeMap<String, String>,
    ) {
        self.record(
            "backend",
            operation.into(),
            started.elapsed().as_secs_f64() * 1_000.0,
            success,
            metadata,
        );
    }

    pub fn record_frontend(
        &self,
        operation: String,
        duration_ms: f64,
        success: bool,
        metadata: BTreeMap<String, String>,
    ) -> Result<(), String> {
        if !valid_operation(&operation) {
            return Err("invalid performance operation name".into());
        }
        if !duration_ms.is_finite() || !(0.0..=86_400_000.0).contains(&duration_ms) {
            return Err("invalid performance duration".into());
        }
        self.record(
            "frontend",
            operation,
            duration_ms,
            success,
            sanitize_metadata(metadata),
        );
        Ok(())
    }

    pub fn status(&self) -> PerformanceStatus {
        self.flush();
        let max_bytes = self.max_file_bytes.load(Ordering::Acquire);
        PerformanceStatus {
            enabled: self.is_enabled(),
            max_log_mb: max_bytes / (1024 * 1024),
            stored_bytes: log_paths()
                .into_iter()
                .filter_map(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len())
                .sum(),
            recorded_this_run: self.recorded.load(Ordering::Acquire),
            dropped_this_run: self.dropped.load(Ordering::Acquire),
        }
    }

    fn ensure_writer(&self) -> bool {
        let Ok(mut sender) = self.sender.lock() else {
            return false;
        };
        if sender.is_some() {
            return true;
        }
        let (tx, rx) = mpsc::sync_channel(CHANNEL_CAPACITY);
        let max_file_bytes = self.max_file_bytes.clone();
        let recorded = self.recorded.clone();
        let dropped = self.dropped.clone();
        match std::thread::Builder::new()
            .name("performance-writer".into())
            .spawn(move || writer_loop(rx, max_file_bytes, recorded, dropped))
        {
            Ok(_) => {
                *sender = Some(tx);
                true
            }
            Err(error) => {
                tracing::warn!("performance writer unavailable: {}", error);
                false
            }
        }
    }

    fn record(
        &self,
        source: &str,
        operation: String,
        duration_ms: f64,
        success: bool,
        metadata: BTreeMap<String, String>,
    ) {
        if !self.is_enabled() || !valid_operation(&operation) {
            return;
        }
        let event = PerformanceEvent {
            schema_version: SCHEMA_VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            platform: std::env::consts::OS.into(),
            process_id: std::process::id(),
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            timestamp: Utc::now(),
            source: source.into(),
            operation,
            duration_ms,
            success,
            metadata: sanitize_metadata(metadata),
        };
        let (was_dropped, disconnected) = match self.sender.lock() {
            Ok(mut sender) => match sender
                .as_ref()
                .map(|sender| sender.try_send(WriterMessage::Event(Box::new(event))))
            {
                Some(Ok(())) => (false, false),
                Some(Err(mpsc::TrySendError::Disconnected(_))) => {
                    *sender = None;
                    (true, true)
                }
                Some(Err(mpsc::TrySendError::Full(_))) | None => (true, false),
            },
            Err(_) => (true, false),
        };
        if disconnected {
            // The failed event is counted as dropped, but restore the writer
            // immediately so a transient writer failure does not permanently
            // disable all later measurements.
            let available = self.ensure_writer();
            self.enabled.store(available, Ordering::Release);
        }
        if was_dropped {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Wait briefly for all events accepted before this call to reach disk.
    /// This is used at settings, status, and export boundaries, never on the
    /// parser/watcher hot path.
    pub fn flush(&self) {
        let sender = self.sender.lock().ok().and_then(|value| value.clone());
        let Some(sender) = sender else {
            return;
        };
        let (ack_tx, ack_rx) = mpsc::channel();
        let mut message = WriterMessage::Flush(ack_tx);
        let deadline = Instant::now() + FLUSH_TIMEOUT;
        loop {
            match sender.try_send(message) {
                Ok(()) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    let _ = ack_rx.recv_timeout(remaining);
                    return;
                }
                Err(mpsc::TrySendError::Full(returned)) if Instant::now() < deadline => {
                    message = returned;
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => return,
            }
        }
    }
}

fn valid_operation(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn sanitize_metadata(metadata: BTreeMap<String, String>) -> BTreeMap<String, String> {
    metadata
        .into_iter()
        .filter(|(key, _)| valid_operation(key))
        .take(16)
        .map(|(key, value)| (key, value.chars().take(128).collect()))
        .collect()
}

fn performance_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|path| path.join("agent-odometer").join("performance"))
}

fn log_paths() -> Vec<PathBuf> {
    let Some(directory) = performance_dir() else {
        return Vec::new();
    };
    log_paths_in(&directory)
}

fn log_paths_in(directory: &Path) -> Vec<PathBuf> {
    [PREVIOUS_LOG, CURRENT_LOG]
        .into_iter()
        .map(|name| directory.join(name))
        .collect()
}

fn writer_loop(
    receiver: mpsc::Receiver<WriterMessage>,
    max_file_bytes: Arc<AtomicU64>,
    recorded: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
) {
    for message in receiver {
        match message {
            WriterMessage::Event(event) => {
                if let Err(error) = append_event(&event, max_file_bytes.load(Ordering::Acquire)) {
                    dropped.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!("could not persist performance event: {}", error);
                } else {
                    recorded.fetch_add(1, Ordering::Relaxed);
                }
            }
            WriterMessage::Flush(acknowledge) => {
                let _ = acknowledge.send(());
            }
        }
    }
}

fn append_event(event: &PerformanceEvent, max_file_bytes: u64) -> anyhow::Result<()> {
    let directory = performance_dir()
        .ok_or_else(|| anyhow::anyhow!("could not determine performance data directory"))?;
    append_event_in(&directory, event, max_file_bytes)
}

fn append_event_in(
    directory: &Path,
    event: &PerformanceEvent,
    max_file_bytes: u64,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(directory)?;
    let current = directory.join(CURRENT_LOG);
    let previous = directory.join(PREVIOUS_LOG);
    let mut raw = serde_json::to_vec(event)?;
    raw.push(b'\n');
    let current_len = std::fs::metadata(&current)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    if current_len > 0 && current_len.saturating_add(raw.len() as u64) > max_file_bytes {
        if let Err(error) = std::fs::remove_file(&previous) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
        std::fs::rename(&current, &previous)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(current)?;
    file.write_all(&raw)?;
    Ok(())
}

pub fn export(path: &Path, format: &str) -> Result<(), String> {
    match format {
        "jsonl" => export_jsonl(path, &log_paths()),
        "csv" => export_csv(path, &log_paths()),
        _ => Err("performance export format must be jsonl or csv".into()),
    }
}

fn export_jsonl(path: &Path, sources: &[PathBuf]) -> Result<(), String> {
    let mut output = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|error| format!("create export: {error}"))?,
    );
    for source in sources {
        let Ok(mut input) = std::fs::File::open(source) else {
            continue;
        };
        std::io::copy(&mut input, &mut output).map_err(|error| format!("write export: {error}"))?;
    }
    output
        .flush()
        .map_err(|error| format!("flush export: {error}"))
}

fn export_csv(path: &Path, sources: &[PathBuf]) -> Result<(), String> {
    let mut output = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|error| format!("create export: {error}"))?,
    );
    writeln!(
        output,
        "timestamp,app_version,platform,process_id,source,operation,duration_ms,success,sequence,metadata_json"
    )
    .map_err(|error| error.to_string())?;
    for source in sources {
        let Ok(file) = std::fs::File::open(source) else {
            continue;
        };
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(event) = serde_json::from_str::<PerformanceEvent>(&line) else {
                continue;
            };
            let metadata = serde_json::to_string(&event.metadata).unwrap_or_default();
            writeln!(
                output,
                "{},{},{},{},{},{},{:.3},{},{},{}",
                csv_field(&event.timestamp.to_rfc3339()),
                csv_field(&event.app_version),
                csv_field(&event.platform),
                event.process_id,
                csv_field(&event.source),
                csv_field(&event.operation),
                event.duration_ms,
                event.success,
                event.sequence,
                csv_field(&metadata)
            )
            .map_err(|error| error.to_string())?;
        }
    }
    output.flush().map_err(|error| error.to_string())
}

fn csv_field(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_names_are_bounded_and_metadata_is_sanitized() {
        assert!(valid_operation("frontend.sessions.render"));
        assert!(!valid_operation("contains spaces"));
        assert!(!valid_operation(&"x".repeat(97)));
        let metadata = BTreeMap::from([
            ("count".into(), "5".into()),
            ("bad key".into(), "ignored".into()),
            ("long".into(), "x".repeat(200)),
        ]);
        let result = sanitize_metadata(metadata);
        assert_eq!(result.len(), 2);
        assert_eq!(result["long"].len(), 128);
    }

    #[test]
    fn csv_escaping_doubles_quotes() {
        assert_eq!(csv_field("a,\"b\""), "\"a,\"\"b\"\"\"");
    }

    fn event(sequence: u64) -> PerformanceEvent {
        PerformanceEvent {
            schema_version: SCHEMA_VERSION,
            app_version: "test".into(),
            platform: "test".into(),
            process_id: 1,
            sequence,
            timestamp: Utc::now(),
            source: "backend".into(),
            operation: "test.operation".into(),
            duration_ms: 1.25,
            success: true,
            metadata: BTreeMap::from([("count".into(), sequence.to_string())]),
        }
    }

    #[test]
    fn recorder_is_disabled_by_default() {
        assert!(!PerformanceRecorder::default().is_enabled());
    }

    #[test]
    fn rotation_and_exports_preserve_both_segments() {
        let directory = tempfile::tempdir().unwrap();
        let export_directory = tempfile::tempdir().unwrap();
        append_event_in(directory.path(), &event(1), 1).unwrap();
        append_event_in(directory.path(), &event(2), 1).unwrap();
        let sources = log_paths_in(directory.path());
        assert!(sources.iter().all(|path| path.exists()));

        let jsonl = export_directory.path().join("events.jsonl");
        export_jsonl(&jsonl, &sources).unwrap();
        let raw = std::fs::read_to_string(jsonl).unwrap();
        assert_eq!(raw.lines().count(), 2);

        let csv = export_directory.path().join("events.csv");
        export_csv(&csv, &sources).unwrap();
        let raw = std::fs::read_to_string(csv).unwrap();
        assert_eq!(raw.lines().count(), 3);
        assert!(raw.contains("test.operation"));
    }
}
