use crate::correlation::ExternalEvent;
use crate::store::AppState;
use chrono::Utc;
use notify_debouncer_full::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

const MAX_LOADED_EVENTS: usize = 10_000;
const MAX_EVENT_LOG_BYTES: u64 = 8 * 1024 * 1024;
const DIRECT_CONFIG_NAMES: &[&str] = &[
    "config.toml",
    "settings.json",
    "settings.local.json",
    "CLAUDE.md",
    "AGENTS.md",
];

pub struct ConfigWatcherHandle {
    _inner: Box<dyn std::any::Any + Send + Sync>,
}

type SnapshotValue = (String, u64);
type Snapshot = HashMap<String, SnapshotValue>;

fn data_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|path| path.join("agent-odometer"))
}
fn log_path() -> Option<PathBuf> {
    data_dir().map(|path| path.join("config-events-v1.jsonl"))
}
fn previous_log_path() -> Option<PathBuf> {
    data_dir().map(|path| path.join("config-events-v1.previous.jsonl"))
}
fn snapshot_path() -> Option<PathBuf> {
    data_dir().map(|path| path.join("config-snapshot-v1.json"))
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

fn roots() -> Vec<(String, PathBuf)> {
    let home = dirs::home_dir();
    let codex = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".codex")));
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".claude")));
    [("codex", codex), ("claude_code", claude)]
        .into_iter()
        .filter_map(|(harness, root)| root.map(|root| (harness.into(), root)))
        .collect()
}

fn is_safe_config_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if components.iter().any(|component| {
        matches!(
            component.as_str(),
            "sessions" | "archived_sessions" | "cache" | "logs" | "tmp"
        )
    }) {
        return false;
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    (components.len() == 1
        && matches!(
            name.as_str(),
            "config.toml" | "settings.json" | "settings.local.json" | "claude.md" | "agents.md"
        ))
        || (components.first().is_some_and(|part| part == "hooks")
            && path.extension().and_then(|value| value.to_str()) == Some("json"))
        || (components.first().is_some_and(|part| part == "skills")
            && path.extension().and_then(|value| value.to_str()) == Some("md"))
}

fn snapshot_file(path: &Path) -> Option<(String, u64)> {
    if std::fs::metadata(path).ok()?.len() > 4 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > 4 * 1024 * 1024 {
        return None;
    }
    Some((stable_hash(&bytes), bytes.len() as u64))
}

fn load_snapshot() -> Option<Snapshot> {
    snapshot_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn save_snapshot(snapshot: &Snapshot) {
    let Some(path) = snapshot_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_vec(snapshot) {
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, &raw).is_ok() && std::fs::rename(&temporary, &path).is_err() {
            // Windows cannot rename over an existing destination. A direct
            // fallback is preferable to silently leaving a stale snapshot.
            let _ = std::fs::write(&path, raw);
            let _ = std::fs::remove_file(temporary);
        }
    }
}

fn discover_snapshot(roots: &[(String, PathBuf)]) -> (Snapshot, HashMap<String, String>) {
    let mut snapshot = Snapshot::new();
    let mut harness_by_key = HashMap::new();
    for (harness, root) in roots {
        if !root.exists() {
            continue;
        }
        let direct = DIRECT_CONFIG_NAMES.iter().map(|name| root.join(name));
        let nested = [root.join("hooks"), root.join("skills")]
            .into_iter()
            .filter(|path| path.exists())
            .flat_map(|path| {
                WalkDir::new(path)
                    .follow_links(false)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_file())
                    .map(|entry| entry.into_path())
            });
        for path in direct
            .chain(nested)
            .filter(|path| is_safe_config_path(root, path))
        {
            if let Some(current) = snapshot_file(&path) {
                let key = path.to_string_lossy().into_owned();
                harness_by_key.insert(key.clone(), harness.clone());
                snapshot.insert(key, current);
            }
        }
    }
    (snapshot, harness_by_key)
}

fn snapshot_diff(
    previous: Option<&Snapshot>,
    current: &Snapshot,
) -> Vec<(String, Option<SnapshotValue>, Option<SnapshotValue>)> {
    let Some(previous) = previous else {
        return Vec::new();
    };
    previous
        .keys()
        .chain(current.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|key| {
            let old = previous.get(key);
            let new = current.get(key);
            (old != new).then(|| (key.clone(), old.cloned(), new.cloned()))
        })
        .collect()
}

fn harness_for_path<'a>(path: &Path, roots: &'a [(String, PathBuf)]) -> Option<&'a str> {
    roots
        .iter()
        .find(|(_, root)| path.starts_with(root))
        .map(|(harness, _)| harness.as_str())
}

pub fn load_events() -> Vec<ExternalEvent> {
    let _io = event_io_lock().lock().unwrap();
    let mut events = VecDeque::with_capacity(MAX_LOADED_EVENTS);
    for path in [previous_log_path(), log_path()].into_iter().flatten() {
        let Ok(file) = std::fs::File::open(path) else {
            continue;
        };
        for event in BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str(&line).ok())
        {
            if events.len() == MAX_LOADED_EVENTS {
                events.pop_front();
            }
            events.push_back(event);
        }
    }
    events.into()
}

fn event_io_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Caller must hold `event_io_lock` so rotation and appends cannot interleave
/// with baseline discovery or a second watcher callback.
fn append_event_locked(event: &ExternalEvent) {
    let Some(path) = log_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::metadata(&path)
        .map(|metadata| metadata.len() >= MAX_EVENT_LOG_BYTES)
        .unwrap_or(false)
    {
        if let Some(previous) = previous_log_path() {
            let _ = std::fs::remove_file(&previous);
            if let Err(error) = std::fs::rename(&path, &previous) {
                tracing::warn!("could not rotate config event log: {}", error);
            }
        }
    }
    if let (Ok(mut file), Ok(raw)) = (
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path),
        serde_json::to_string(event),
    ) {
        let _ = writeln!(file, "{raw}");
    }
}

fn event_for(
    path: &Path,
    harness: &str,
    previous: Option<&(String, u64)>,
    current: Option<&(String, u64)>,
) -> ExternalEvent {
    let kind = match (previous, current) {
        (None, Some(_)) => "created",
        (Some(_), None) => "removed",
        _ => "modified",
    };
    let mut metadata = BTreeMap::new();
    metadata.insert("harness".into(), harness.into());
    metadata.insert(
        "path_id".into(),
        stable_hash(path.to_string_lossy().as_bytes()),
    );
    if let Some((hash, size)) = previous {
        metadata.insert("previous_hash".into(), hash.clone());
        metadata.insert("previous_size".into(), size.to_string());
    }
    if let Some((hash, size)) = current {
        metadata.insert("content_hash".into(), hash.clone());
        metadata.insert("size".into(), size.to_string());
    }
    let timestamp = Utc::now();
    ExternalEvent {
        id: format!(
            "config:{}:{}",
            timestamp.timestamp_millis(),
            metadata["path_id"]
        ),
        timestamp,
        scope: None,
        source: "config".into(),
        kind: kind.into(),
        metadata,
    }
}

fn record_change(
    app: &AppHandle,
    state: &Arc<AppState>,
    snapshot: &Arc<Mutex<Snapshot>>,
    harness: &str,
    root: &Path,
    path: &Path,
) {
    if !is_safe_config_path(root, path) {
        return;
    }
    let key = path.to_string_lossy().into_owned();
    let _io = event_io_lock().lock().unwrap();
    // Read after taking the global writer lock so two watcher instances cannot
    // publish an older pre-lock observation after a newer one.
    let current = snapshot_file(path);
    let (event, snapshot_copy) = {
        let mut guard = snapshot.lock().unwrap();
        let previous = guard.get(&key).cloned();
        if previous == current {
            return;
        }
        let event = event_for(path, harness, previous.as_ref(), current.as_ref());
        match current {
            Some(value) => {
                guard.insert(key, value);
            }
            None => {
                guard.remove(&key);
            }
        }
        (event, guard.clone())
    };
    save_snapshot(&snapshot_copy);
    append_event_locked(&event);
    drop(_io);
    state.push_external_event(event.clone());
    let _ = app.emit("config-event", event);
}

pub fn start(app: AppHandle, state: Arc<AppState>) -> anyhow::Result<ConfigWatcherHandle> {
    let roots = roots();
    let previous = load_snapshot();
    let snapshot = Arc::new(Mutex::new(previous.clone().unwrap_or_default()));

    let app_cb = app.clone();
    // AppState owns this watcher handle; avoid a strong-reference cycle.
    let state_cb = Arc::downgrade(&state);
    let snapshot_cb = snapshot.clone();
    let roots_cb = roots.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(500),
        None,
        move |result: DebounceEventResult| {
            let Some(state_cb) = state_cb.upgrade() else {
                return;
            };
            let Ok(events) = result else { return };
            for event in events {
                for path in &event.paths {
                    if let Some((harness, root)) =
                        roots_cb.iter().find(|(_, root)| path.starts_with(root))
                    {
                        record_change(&app_cb, &state_cb, &snapshot_cb, harness, root, path);
                    }
                }
            }
        },
    )?;
    for (_, root) in &roots {
        if root.exists() {
            debouncer.watch(root, RecursiveMode::NonRecursive)?;
            for nested in [root.join("hooks"), root.join("skills")] {
                if nested.exists() {
                    debouncer.watch(&nested, RecursiveMode::Recursive)?;
                }
            }
        }
    }

    // Discovery hashes files and can traverse large skill trees. Run it after
    // the watcher is live and off the startup thread. The merge below skips a
    // path if the watcher already observed a newer value while discovery ran.
    let baseline_app = app.clone();
    let baseline_state = state.clone();
    let baseline_snapshot = snapshot.clone();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let (current, harness_by_key) = discover_snapshot(&roots);
        let discovered_files = current.len();
        let changes = snapshot_diff(previous.as_ref(), &current);
        let mut emitted = Vec::new();
        let _io = event_io_lock().lock().unwrap();
        let snapshot_copy = {
            let mut guard = baseline_snapshot.lock().unwrap();
            if previous.is_none() {
                *guard = current;
            } else {
                for (key, old, new) in changes {
                    if guard.get(&key) != old.as_ref() {
                        continue;
                    }
                    match &new {
                        Some(value) => {
                            guard.insert(key.clone(), value.clone());
                        }
                        None => {
                            guard.remove(&key);
                        }
                    }
                    emitted.push((key, old, new));
                }
            }
            guard.clone()
        };
        save_snapshot(&snapshot_copy);
        let change_count = emitted.len();
        let mut event_records = Vec::with_capacity(change_count);
        for (key, old, current) in emitted {
            let path = PathBuf::from(&key);
            let harness = harness_by_key
                .get(&key)
                .map(String::as_str)
                .or_else(|| harness_for_path(&path, &roots));
            let Some(harness) = harness else { continue };
            let event = event_for(&path, harness, old.as_ref(), current.as_ref());
            append_event_locked(&event);
            event_records.push(event);
        }
        drop(_io);
        for event in event_records {
            baseline_state.push_external_event(event.clone());
            let _ = baseline_app.emit("config-event", event);
        }
        baseline_state.performance.record_backend(
            "background.config_discovery",
            started,
            true,
            BTreeMap::from([
                ("files".into(), discovered_files.to_string()),
                ("changes".into(), change_count.to_string()),
            ]),
        );
    });
    Ok(ConfigWatcherHandle {
        _inner: Box::new(debouncer),
    })
}

#[cfg(test)]
mod tests {
    use super::{discover_snapshot, snapshot_diff, Snapshot};

    #[test]
    fn first_snapshot_is_a_baseline() {
        let current = Snapshot::from([("config.toml".into(), ("a".into(), 1))]);
        assert!(snapshot_diff(None, &current).is_empty());
    }

    #[test]
    fn snapshot_diff_reports_offline_creates_modifications_and_removals() {
        let previous = Snapshot::from([
            ("changed".into(), ("old".into(), 1)),
            ("removed".into(), ("gone".into(), 2)),
        ]);
        let current = Snapshot::from([
            ("changed".into(), ("new".into(), 3)),
            ("created".into(), ("added".into(), 4)),
        ]);
        let diff = snapshot_diff(Some(&previous), &current);
        assert_eq!(diff.len(), 3);
        assert!(diff.iter().any(|(key, old, new)| {
            key == "created" && old.is_none() && new.as_ref() == current.get(key)
        }));
        assert!(diff.iter().any(|(key, old, new)| {
            key == "changed"
                && old.as_ref() == previous.get(key)
                && new.as_ref() == current.get(key)
        }));
        assert!(diff.iter().any(|(key, old, new)| {
            key == "removed" && old.as_ref() == previous.get(key) && new.is_none()
        }));
    }

    #[test]
    fn discovery_ignores_large_unrelated_harness_trees() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("codex");
        std::fs::create_dir_all(root.join("skills/example")).unwrap();
        std::fs::create_dir_all(root.join("sessions/deep")).unwrap();
        std::fs::write(root.join("config.toml"), "model = 'test'").unwrap();
        std::fs::write(root.join("skills/example/SKILL.md"), "# Synthetic skill").unwrap();
        std::fs::write(root.join("sessions/deep/AGENTS.md"), "must be ignored").unwrap();

        let (snapshot, _) = discover_snapshot(&[("codex".into(), root)]);
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.keys().all(|key| !key.contains("sessions")));
    }
}
