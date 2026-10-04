//! Bounded, allowlisted MCP activity. Never retains requests or response bodies.
use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

const MAX_BYTES: u64 = 256 * 1024;
const MAX_ENTRIES: usize = 100;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientIdentity {
    Codex,
    ClaudeCode,
    Verifier,
    #[default]
    Other,
}

#[derive(Debug, Clone)]
pub struct Connection {
    pub client: ClientIdentity,
    pub version: Option<String>,
    pub initialized_at: DateTime<Utc>,
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            client: ClientIdentity::Other,
            version: None,
            initialized_at: Utc::now(),
        }
    }
}
impl Connection {
    pub fn from_initialize(params: &Value) -> Self {
        let name = params["clientInfo"]["name"].as_str().unwrap_or("");
        let client = match name.to_ascii_lowercase().as_str() {
            "codex" | "codex-cli" | "codex-mcp-client" => ClientIdentity::Codex,
            "claude-code" | "claude-cli" => ClientIdentity::ClaudeCode,
            "odometer-verifier" => ClientIdentity::Verifier,
            _ => ClientIdentity::Other,
        };
        let version = params["clientInfo"]["version"]
            .as_str()
            .filter(|value| {
                value.len() <= 32
                    && !value.is_empty()
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.' || byte == b'-')
            })
            .map(str::to_owned);
        Self {
            client,
            version,
            initialized_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Activity {
    pub timestamp: DateTime<Utc>,
    pub initialized_at: DateTime<Utc>,
    pub client: ClientIdentity,
    pub client_version: Option<String>,
    pub identity_authority: String,
    pub tool: String,
    pub duration_ms: u64,
    pub success: bool,
    pub error_code: Option<String>,
    pub result_bytes: usize,
    pub result_rows: Option<usize>,
    pub schema_version: Option<u64>,
    #[serde(default)]
    pub observation_generation: Option<String>,
    #[serde(default)]
    pub ledger_available: Option<bool>,
    pub fallback_or_unavailable: Option<bool>,
}

pub fn summarize(
    connection: &Connection,
    tool: &str,
    started: std::time::Instant,
    result: &Result<String, crate::mcp_server::ToolError>,
) -> Option<Activity> {
    // Tool names come from the static catalog, never raw unknown tool arguments.
    if !crate::mcp_server::advertised_tool_names()
        .iter()
        .any(|name| name == tool)
    {
        return None;
    }
    let value = result
        .as_ref()
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(text).ok());
    let activity = Activity {
        timestamp: Utc::now(),
        initialized_at: connection.initialized_at,
        client: connection.client,
        client_version: connection.version.clone(),
        identity_authority: "self_reported".into(),
        tool: tool.into(),
        duration_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        success: result.is_ok(),
        error_code: result.as_ref().err().map(|error| match error {
            crate::mcp_server::ToolError::Failed(message) => serde_json::to_value(
                crate::integration_status::DiagnosticCode::for_query_failure(message),
            )
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| "query_failed".into()),
            _ => "query_failed".into(),
        }),
        result_bytes: result.as_ref().map_or(0, String::len),
        result_rows: value.as_ref().and_then(|value| {
            ["sessions", "projects", "models", "snapshots", "groups"]
                .iter()
                .find_map(|field| value[*field].as_array().map(Vec::len))
        }),
        schema_version: value
            .as_ref()
            .and_then(|value| value["schema_version"].as_u64()),
        observation_generation: value
            .as_ref()
            .and_then(|value| value["observation"]["generation"].as_str())
            .filter(|value| {
                value.len() <= 64
                    && value.starts_with("observation:")
                    && value.bytes().all(|byte| {
                        byte.is_ascii_digit() || byte == b':' || byte.is_ascii_lowercase()
                    })
            })
            .map(str::to_owned),
        ledger_available: value
            .as_ref()
            .and_then(|value| value["ledger_available"].as_bool()),
        fallback_or_unavailable: value
            .as_ref()
            .map(|value| contains_incomplete_basis(value, 0)),
    };
    Some(activity)
}

pub fn record(activity: Activity) {
    if let Some(path) =
        dirs::data_local_dir().map(|root| root.join("agent-odometer/integration-activity.json"))
    {
        let _ = append_at(&path, activity);
    }
}

fn contains_incomplete_basis(value: &Value, depth: usize) -> bool {
    if depth > 12 {
        return true;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .any(|value| contains_incomplete_basis(value, depth + 1)),
        Value::Object(values) => values.iter().any(|(key, value)| {
            (key == "basis" && matches!(value.as_str(), Some("fallback" | "unavailable" | "stale")))
                || contains_incomplete_basis(value, depth + 1)
        }),
        _ => false,
    }
}

fn open_regular(path: &Path, create: bool) -> Result<File> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            bail!("activity store is not a regular file");
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(create).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            bail!("activity store is not a regular file");
        }
    }
    if !metadata.is_file() {
        bail!("activity store is not a regular file");
    }
    Ok(file)
}
fn read_locked(file: &mut File) -> Result<Vec<Activity>> {
    if file.metadata()?.len() > MAX_BYTES {
        bail!("activity store exceeds its bound");
    }
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("activity store exceeds its bound");
    }
    let entries: Vec<Activity> = if bytes.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice(&bytes)?
    };
    if entries.len() > MAX_ENTRIES {
        bail!("activity store exceeds its entry bound");
    }
    if entries.iter().any(|entry| {
        entry.identity_authority != "self_reported"
            || !crate::mcp_server::advertised_tool_names().contains(&entry.tool)
            || entry.client_version.as_ref().is_some_and(|version| {
                version.len() > 32
                    || !version
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || byte == b'.' || byte == b'-')
            })
            || entry.error_code.as_ref().is_some_and(|code| {
                ![
                    "query_failed",
                    "query_cancelled",
                    "query_too_broad",
                    "ledger_not_ready",
                ]
                .contains(&code.as_str())
            })
            || entry.observation_generation.as_ref().is_some_and(|value| {
                value.len() > 64
                    || !value.starts_with("observation:")
                    || !value.bytes().all(|byte| {
                        byte.is_ascii_digit() || byte == b':' || byte.is_ascii_lowercase()
                    })
            })
    }) {
        bail!("activity metadata failed its allowlist");
    }
    Ok(entries)
}
// A duplicated/inherited descriptor can outlive this scope. Closing only our
// descriptor does not release a Unix flock on the shared open-file description.
// Explicit unlock also runs on read/write errors and unwinding.
struct ActivityLock(File);
impl Drop for ActivityLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn append_at(path: &Path, activity: Activity) -> Result<()> {
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| anyhow::anyhow!("activity path has no parent"))?,
    )?;
    let file = open_regular(path, true)?;
    file.try_lock()?;
    let mut locked = ActivityLock(file);
    let mut entries = read_locked(&mut locked.0)?;
    entries.push(activity);
    if entries.len() > MAX_ENTRIES {
        entries.drain(..entries.len() - MAX_ENTRIES);
    }
    let bytes = serde_json::to_vec(&entries)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!("activity store exceeds its bound");
    }
    locked.0.seek(SeekFrom::Start(0))?;
    locked.0.write_all(&bytes)?;
    locked.0.set_len(bytes.len() as u64)?;
    locked.0.sync_all()?;
    Ok(())
}
pub fn recent() -> Result<Vec<Activity>> {
    let Some(path) =
        dirs::data_local_dir().map(|root| root.join("agent-odometer/integration-activity.json"))
    else {
        return Ok(Vec::new());
    };
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = open_regular(&path, false)?;
    file.try_lock_shared()?;
    let mut locked = ActivityLock(file);
    read_locked(&mut locked.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn completed_activity_scope_releases_lock_while_duplicated_descriptor_lives() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("activity.json");
        let file = open_regular(&path, true).unwrap();
        file.try_lock().unwrap();
        let locked = ActivityLock(file);
        // dup and fork inheritance share the same open-file description. Keep
        // it alive to reproduce the lock contention observed in parallel CI.
        let duplicated = locked.0.try_clone().unwrap();
        let competing = open_regular(&path, true).unwrap();
        assert!(competing.try_lock().is_err());
        drop(locked);
        competing.try_lock().unwrap();
        competing.unlock().unwrap();
        drop(duplicated);
    }
    #[test]
    fn initialize_metadata_cannot_store_arbitrary_client_text() {
        let connection = Connection::from_initialize(
            &serde_json::json!({"clientInfo":{"name":"a synthetic private prompt","version":"synthetic secret"}}),
        );
        assert_eq!(connection.client, ClientIdentity::Other);
        assert_eq!(connection.version, None);
        let verifier = Connection::from_initialize(
            &serde_json::json!({"clientInfo":{"name":"odometer-verifier","version":"1"}}),
        );
        assert_eq!(verifier.client, ClientIdentity::Verifier);
    }
    #[test]
    fn damaged_or_unallowlisted_activity_is_unavailable_instead_of_empty_or_trusted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("activity.json");
        let activity = summarize(
            &Connection::default(),
            "odometer_status",
            std::time::Instant::now(),
            &Ok("{}".into()),
        )
        .unwrap();
        for (field, value) in [
            ("identity_authority", "authenticated"),
            ("tool", "synthetic private text"),
            ("client_version", "synthetic credential"),
            ("error_code", "synthetic private error"),
            ("observation_generation", "synthetic private path"),
        ] {
            let mut entry = serde_json::to_value(&activity).unwrap();
            entry[field] = serde_json::json!(value);
            std::fs::write(&path, serde_json::to_vec(&[entry]).unwrap()).unwrap();
            assert!(read_locked(&mut open_regular(&path, false).unwrap()).is_err());
        }
        std::fs::write(&path, b"{ incomplete synthetic metadata").unwrap();
        assert!(read_locked(&mut open_regular(&path, false).unwrap()).is_err());
        std::fs::write(&path, b"").unwrap();
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_BYTES + 1).unwrap();
        assert!(read_locked(&mut open_regular(&path, false).unwrap()).is_err());
        std::fs::write(
            &path,
            serde_json::to_vec(&vec![activity; MAX_ENTRIES + 1]).unwrap(),
        )
        .unwrap();
        assert!(read_locked(&mut open_regular(&path, false).unwrap()).is_err());
    }
    #[test]
    fn bounded_activity_retains_metadata_without_response_or_error_bodies() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("activity.json");
        let connection = Connection::from_initialize(
            &serde_json::json!({"clientInfo":{"name":"codex","version":"1.2.3"}}),
        );
        let result = Ok(serde_json::json!({"schema_version":1,"ledger_available":true,"observation":{"generation":"observation:123:2"},"private_prompt":"synthetic private text","sessions":[{"id":"synthetic sensitive id"}],"basis":"fallback"}).to_string());
        let metadata = summarize(
            &connection,
            "odometer_status",
            std::time::Instant::now(),
            &result,
        )
        .unwrap();
        for _ in 0..MAX_ENTRIES + 2 {
            append_at(&path, metadata.clone()).unwrap();
        }
        let raw = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&raw);
        assert!(!text.contains("synthetic private"));
        assert!(!text.contains("sensitive id"));
        let mut file = open_regular(&path, false).unwrap();
        let entries = read_locked(&mut file).unwrap();
        assert_eq!(entries.len(), MAX_ENTRIES);
        assert_eq!(entries[0].result_rows, Some(1));
        assert_eq!(entries[0].fallback_or_unavailable, Some(true));
        assert_eq!(entries[0].ledger_available, Some(true));
        assert_eq!(
            entries[0].observation_generation.as_deref(),
            Some("observation:123:2")
        );
        let failure = Err(crate::mcp_server::ToolError::Failed(
            "synthetic private error".into(),
        ));
        let failed = summarize(
            &connection,
            "usage_report",
            std::time::Instant::now(),
            &failure,
        )
        .unwrap();
        assert_eq!(failed.error_code.as_deref(), Some("query_failed"));
        assert!(!serde_json::to_string(&failed)
            .unwrap()
            .contains("private error"));
    }
}
