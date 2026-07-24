use crate::correlation::{project_scope_identity, ExternalEvent};
use crate::store::AppState;
use chrono::Utc;
use notify_debouncer_full::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

const MAX_LOADED_EVENTS: usize = 10_000;
const MAX_EVENT_LOG_BYTES: u64 = 8 * 1024 * 1024;
const CODEX_CONFIG_NAMES: &[&str] = &["config.toml", "AGENTS.md"];
const CLAUDE_CONFIG_NAMES: &[&str] = &["settings.json", "settings.local.json", "CLAUDE.md"];

pub struct ConfigWatcherHandle {
    _inner: Box<dyn std::any::Any + Send + Sync>,
}

type SnapshotValue = (String, u64);
type Snapshot = HashMap<String, SnapshotValue>;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct SnapshotState {
    entries: Snapshot,
    roots: BTreeSet<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ConfigRoot {
    harness: String,
    path: PathBuf,
    scope: Option<String>,
    direct_names: &'static [&'static str],
    nested: bool,
}

fn data_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|path| path.join("agent-odometer"))
}
fn log_path() -> Option<PathBuf> {
    data_dir().map(|path| path.join("config-events-v2.jsonl"))
}
fn previous_log_path() -> Option<PathBuf> {
    data_dir().map(|path| path.join("config-events-v2.previous.jsonl"))
}
fn snapshot_path() -> Option<PathBuf> {
    data_dir().map(|path| path.join("config-snapshot-v2.json"))
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

fn project_scope(path: &Path) -> PathBuf {
    gix::discover(path)
        .ok()
        .map(|repo| {
            repo.work_dir()
                .unwrap_or_else(|| repo.git_dir())
                .to_path_buf()
        })
        .unwrap_or_else(|| path.to_path_buf())
}

fn project_roots(working_directories: impl IntoIterator<Item = PathBuf>) -> Vec<ConfigRoot> {
    let mut out = Vec::new();
    let project_scopes: BTreeSet<PathBuf> = working_directories
        .into_iter()
        .map(|path| project_scope(&path))
        .collect();
    for scope_path in project_scopes {
        let scope = Some(project_scope_identity(&scope_path.to_string_lossy()));
        // Root-level instruction files belong to their respective harnesses.
        out.push(ConfigRoot {
            harness: "codex".into(),
            path: scope_path.clone(),
            scope: scope.clone(),
            direct_names: &["AGENTS.md"],
            nested: false,
        });
        out.push(ConfigRoot {
            harness: "claude_code".into(),
            path: scope_path.clone(),
            scope: scope.clone(),
            direct_names: &["CLAUDE.md"],
            nested: false,
        });
        out.push(ConfigRoot {
            harness: "codex".into(),
            path: scope_path.join(".codex"),
            scope: scope.clone(),
            direct_names: CODEX_CONFIG_NAMES,
            nested: true,
        });
        out.push(ConfigRoot {
            harness: "claude_code".into(),
            path: scope_path.join(".claude"),
            scope,
            direct_names: CLAUDE_CONFIG_NAMES,
            nested: true,
        });
    }
    out
}

fn normalized_root_path(path: &Path) -> String {
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_owned();
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn deduplicate_roots(roots: Vec<ConfigRoot>) -> Vec<ConfigRoot> {
    let mut unique = BTreeMap::<String, ConfigRoot>::new();
    for root in roots {
        // The same physical surface can be both a global harness root and a
        // project root (for example, a session started from the user's home
        // directory). Track it once and prefer global attribution.
        let key = format!(
            "{}\0{}\0{}\0{}",
            root.harness,
            normalized_root_path(&root.path),
            root.nested,
            root.direct_names.join("\0")
        );
        match unique.get_mut(&key) {
            Some(existing) if existing.scope.is_some() && root.scope.is_none() => {
                *existing = root;
            }
            Some(_) => {}
            None => {
                unique.insert(key, root);
            }
        }
    }
    unique.into_values().collect()
}

fn roots(state: &AppState) -> Vec<ConfigRoot> {
    let home = dirs::home_dir();
    let codex = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home.as_ref().map(|path| path.join(".codex")));
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home.map(|path| path.join(".claude")));
    let mut out = Vec::new();
    if let Some(path) = codex {
        out.push(ConfigRoot {
            harness: "codex".into(),
            path,
            scope: None,
            direct_names: CODEX_CONFIG_NAMES,
            nested: true,
        });
    }
    if let Some(path) = claude {
        out.push(ConfigRoot {
            harness: "claude_code".into(),
            path,
            scope: None,
            direct_names: CLAUDE_CONFIG_NAMES,
            nested: true,
        });
    }
    let working_directories: BTreeSet<PathBuf> = state
        .sessions
        .iter()
        .filter_map(|entry| {
            entry
                .value()
                .working_directory
                .as_deref()
                .map(PathBuf::from)
        })
        .collect();
    out.extend(project_roots(working_directories));
    deduplicate_roots(out)
}

fn is_safe_config_path(root: &ConfigRoot, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(&root.path) else {
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
        && root
            .direct_names
            .iter()
            .any(|allowed| name.eq_ignore_ascii_case(allowed)))
        || (root.nested
            && components.first().is_some_and(|part| part == "hooks")
            && path.extension().and_then(|value| value.to_str()) == Some("json"))
        || (root.nested
            && components.first().is_some_and(|part| part == "skills")
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

fn load_snapshot() -> SnapshotState {
    snapshot_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_snapshot(snapshot: &SnapshotState) {
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

fn discover_snapshot(roots: &[ConfigRoot]) -> (Snapshot, HashMap<String, ConfigRoot>) {
    let mut snapshot = Snapshot::new();
    let mut root_by_key = HashMap::new();
    for root in roots {
        if !root.path.exists() {
            continue;
        }
        let direct = root.direct_names.iter().map(|name| root.path.join(name));
        let nested = [root.path.join("hooks"), root.path.join("skills")]
            .into_iter()
            .filter(|_| root.nested)
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
                root_by_key.insert(key.clone(), root.clone());
                snapshot.insert(key, current);
            }
        }
    }
    (snapshot, root_by_key)
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

fn tracked_root_for_path<'a>(path: &Path, roots: &'a [ConfigRoot]) -> Option<&'a ConfigRoot> {
    roots.iter().find(|root| is_safe_config_path(root, path))
}

fn root_identity(root: &ConfigRoot) -> String {
    let scope = root.scope.as_deref().unwrap_or("global");
    let raw = format!(
        "{}\0{}\0{scope}\0{}\0{}",
        root.harness,
        normalized_root_path(&root.path),
        root.nested,
        root.direct_names.join("\0")
    );
    stable_hash(raw.as_bytes())
}

type SnapshotChange = (
    String,
    ConfigRoot,
    Option<SnapshotValue>,
    Option<SnapshotValue>,
);

fn merge_discovery(
    observed: &SnapshotState,
    latest: &mut SnapshotState,
    current: &Snapshot,
    root_by_key: &HashMap<String, ConfigRoot>,
    roots: &[ConfigRoot],
) -> Vec<SnapshotChange> {
    let observed_entries = observed
        .entries
        .iter()
        .filter(|(path, _)| {
            tracked_root_for_path(Path::new(path), roots)
                .is_some_and(|root| observed.roots.contains(&root_identity(root)))
        })
        .map(|(path, value)| (path.clone(), value.clone()))
        .collect::<Snapshot>();
    let current_entries = current
        .iter()
        .filter(|(path, _)| {
            root_by_key
                .get(*path)
                .is_some_and(|root| observed.roots.contains(&root_identity(root)))
        })
        .map(|(path, value)| (path.clone(), value.clone()))
        .collect::<Snapshot>();

    let mut emitted = Vec::new();
    for (key, old, new) in snapshot_diff(Some(&observed_entries), &current_entries) {
        // A watcher or another discovery completed after this scan began.
        // Never overwrite that newer persisted observation with stale scan data.
        if latest.entries.get(&key) != old.as_ref() {
            continue;
        }
        let path = PathBuf::from(&key);
        let root = root_by_key
            .get(&key)
            .or_else(|| tracked_root_for_path(&path, roots));
        let Some(root) = root else { continue };
        match &new {
            Some(value) => {
                latest.entries.insert(key.clone(), value.clone());
            }
            None => {
                latest.entries.remove(&key);
            }
        }
        emitted.push((key, root.clone(), old, new));
    }

    // A project root discovered after the session scan is a baseline, not a
    // burst of file creations. Merge its files silently. If a watcher already
    // observed a newer value, retain that value instead of the scan result.
    for (key, value) in current {
        let Some(root) = root_by_key.get(key) else {
            continue;
        };
        if observed.roots.contains(&root_identity(root)) {
            continue;
        }
        if latest.entries.get(key) == observed.entries.get(key) {
            latest.entries.insert(key.clone(), value.clone());
        }
    }
    latest.roots.extend(roots.iter().map(root_identity));

    emitted
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
    root: &ConfigRoot,
    previous: Option<&(String, u64)>,
    current: Option<&(String, u64)>,
) -> ExternalEvent {
    let kind = match (previous, current) {
        (None, Some(_)) => "created",
        (Some(_), None) => "removed",
        _ => "modified",
    };
    let mut metadata = BTreeMap::new();
    metadata.insert("harness".into(), root.harness.clone());
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
    let previous_size = previous.map_or(0, |(_, size)| *size);
    let current_size = current.map_or(0, |(_, size)| *size);
    metadata.insert(
        "safe_diff".into(),
        format!("size {previous_size} -> {current_size} bytes; content hash changed"),
    );
    let timestamp = Utc::now();
    ExternalEvent {
        id: format!(
            "config:{}:{}",
            timestamp.timestamp_millis(),
            metadata["path_id"]
        ),
        timestamp,
        scope: root.scope.clone(),
        source: "config".into(),
        kind: kind.into(),
        metadata,
    }
}

fn record_change(app: &AppHandle, state: &Arc<AppState>, root: &ConfigRoot, path: &Path) {
    if !is_safe_config_path(root, path) {
        return;
    }
    let key = path.to_string_lossy().into_owned();
    let _io = event_io_lock().lock().unwrap();
    // Reload under the global writer lock so overlapping startup/post-scan
    // watchers always compare against and update the latest persisted state.
    let mut snapshot = load_snapshot();
    let current = snapshot_file(path);
    let previous = snapshot.entries.get(&key).cloned();
    if previous == current {
        return;
    }
    match &current {
        Some(value) => {
            snapshot.entries.insert(key, value.clone());
        }
        None => {
            snapshot.entries.remove(&key);
        }
    }
    let tracked = snapshot.roots.contains(&root_identity(root));
    save_snapshot(&snapshot);
    let event = tracked.then(|| event_for(path, root, previous.as_ref(), current.as_ref()));
    if let Some(event) = &event {
        append_event_locked(event);
    }
    drop(_io);
    if let Some(event) = event {
        state.push_external_event(event.clone());
        let _ = app.emit("config-event", event);
    }
}

pub fn start(app: AppHandle, state: Arc<AppState>) -> anyhow::Result<ConfigWatcherHandle> {
    let roots = roots(&state);
    let observed = {
        let _io = event_io_lock().lock().unwrap();
        load_snapshot()
    };

    let app_cb = app.clone();
    // AppState owns this watcher handle; avoid a strong-reference cycle.
    let state_cb = Arc::downgrade(&state);
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
                    if let Some(root) = tracked_root_for_path(path, &roots_cb) {
                        record_change(&app_cb, &state_cb, root, path);
                    }
                }
            }
        },
    )?;
    let mut watched = BTreeSet::new();
    for root in &roots {
        if root.path.exists() && watched.insert((root.path.clone(), false)) {
            if let Err(error) = debouncer.watch(&root.path, RecursiveMode::NonRecursive) {
                tracing::warn!("could not watch config root {:?}: {}", root.path, error);
            }
        }
        if root.nested {
            for nested in [root.path.join("hooks"), root.path.join("skills")] {
                if nested.exists() && watched.insert((nested.clone(), true)) {
                    if let Err(error) = debouncer.watch(&nested, RecursiveMode::Recursive) {
                        tracing::warn!("could not watch config tree {:?}: {}", nested, error);
                    }
                }
            }
        }
    }

    // Discovery hashes files and can traverse large skill trees. Run it after
    // the watcher is live and off the startup thread. The merge below skips a
    // path if the watcher already observed a newer value while discovery ran.
    let baseline_app = app.clone();
    let baseline_state = state.clone();
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let (current, root_by_key) = discover_snapshot(&roots);
        let discovered_files = current.len();
        let _io = event_io_lock().lock().unwrap();
        let mut latest = load_snapshot();
        let emitted = merge_discovery(&observed, &mut latest, &current, &root_by_key, &roots);
        save_snapshot(&latest);
        let change_count = emitted.len();
        let mut event_records = Vec::with_capacity(change_count);
        for (key, root, old, current) in emitted {
            let path = PathBuf::from(key);
            let event = event_for(&path, &root, old.as_ref(), current.as_ref());
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
    use super::{
        deduplicate_roots, discover_snapshot, event_for, is_safe_config_path, merge_discovery,
        project_roots, project_scope_identity, root_identity, snapshot_diff, tracked_root_for_path,
        ConfigRoot, Snapshot, SnapshotState, CODEX_CONFIG_NAMES,
    };
    use std::collections::HashMap;
    use std::path::Path;
    use std::process::Command;

    fn config_root(path: &Path, scope: Option<&str>) -> ConfigRoot {
        ConfigRoot {
            harness: "codex".into(),
            path: path.to_path_buf(),
            scope: scope.map(project_scope_identity),
            direct_names: CODEX_CONFIG_NAMES,
            nested: true,
        }
    }

    #[test]
    fn first_snapshot_is_a_baseline() {
        let current = Snapshot::from([("config.toml".into(), ("a".into(), 1))]);
        assert!(snapshot_diff(None, &current).is_empty());
    }

    #[test]
    fn newly_discovered_root_is_baselined_without_created_events() {
        let directory = tempfile::tempdir().unwrap();
        let root = config_root(directory.path(), Some("synthetic-project"));
        let path = directory
            .path()
            .join("config.toml")
            .to_string_lossy()
            .into_owned();
        let current = Snapshot::from([(path.clone(), ("current".into(), 7))]);
        let root_by_key = HashMap::from([(path.clone(), root.clone())]);
        let observed = SnapshotState::default();
        let mut latest = observed.clone();

        let emitted = merge_discovery(
            &observed,
            &mut latest,
            &current,
            &root_by_key,
            std::slice::from_ref(&root),
        );

        assert!(emitted.is_empty());
        assert_eq!(latest.entries, current);
        assert!(latest.roots.contains(&root_identity(&root)));
    }

    #[test]
    fn known_root_reports_offline_creates_modifications_and_removals() {
        let directory = tempfile::tempdir().unwrap();
        let root = config_root(directory.path(), Some("synthetic-project"));
        let changed = directory
            .path()
            .join("config.toml")
            .to_string_lossy()
            .into_owned();
        let removed = directory
            .path()
            .join("AGENTS.md")
            .to_string_lossy()
            .into_owned();
        let created = directory
            .path()
            .join("skills/example/SKILL.md")
            .to_string_lossy()
            .into_owned();
        let observed = SnapshotState {
            entries: Snapshot::from([
                (changed.clone(), ("old".into(), 1)),
                (removed.clone(), ("gone".into(), 2)),
            ]),
            roots: [root_identity(&root)].into(),
        };
        let current = Snapshot::from([
            (changed.clone(), ("new".into(), 3)),
            (created.clone(), ("added".into(), 4)),
        ]);
        let root_by_key = HashMap::from([
            (changed.clone(), root.clone()),
            (created.clone(), root.clone()),
        ]);
        let mut latest = observed.clone();

        let emitted = merge_discovery(
            &observed,
            &mut latest,
            &current,
            &root_by_key,
            std::slice::from_ref(&root),
        );

        assert_eq!(emitted.len(), 3);
        assert_eq!(latest.entries, current);
        assert!(emitted
            .iter()
            .any(|(key, _, old, new)| key == &created && old.is_none() && new.is_some()));
        assert!(emitted
            .iter()
            .any(|(key, _, old, new)| key == &changed && old.is_some() && new.is_some()));
        assert!(emitted
            .iter()
            .any(|(key, _, old, new)| key == &removed && old.is_some() && new.is_none()));
    }

    #[test]
    fn stale_discovery_does_not_overwrite_a_newer_watcher_value() {
        let directory = tempfile::tempdir().unwrap();
        let root = config_root(directory.path(), Some("synthetic-project"));
        let path = directory
            .path()
            .join("config.toml")
            .to_string_lossy()
            .into_owned();
        let observed = SnapshotState {
            entries: Snapshot::from([(path.clone(), ("old".into(), 1))]),
            roots: [root_identity(&root)].into(),
        };
        let current = Snapshot::from([(path.clone(), ("stale-scan".into(), 2))]);
        let root_by_key = HashMap::from([(path.clone(), root.clone())]);
        let mut latest = observed.clone();
        latest
            .entries
            .insert(path.clone(), ("watcher-newer".into(), 3));

        let emitted = merge_discovery(
            &observed,
            &mut latest,
            &current,
            &root_by_key,
            std::slice::from_ref(&root),
        );

        assert!(emitted.is_empty());
        assert_eq!(latest.entries[&path], ("watcher-newer".into(), 3));
    }

    #[test]
    fn project_roots_use_the_containing_worktree_and_harness_surfaces() {
        let directory = tempfile::tempdir().unwrap();
        let status = Command::new("git")
            .arg("-C")
            .arg(directory.path())
            .args(["init", "--quiet"])
            .status()
            .unwrap();
        assert!(status.success());
        let nested = directory.path().join("packages/example");
        std::fs::create_dir_all(&nested).unwrap();

        let roots = project_roots([nested]);
        assert_eq!(roots.len(), 4);
        let expected_scope = project_scope_identity(&directory.path().to_string_lossy());
        assert!(roots
            .iter()
            .all(|root| root.scope.as_deref() == Some(expected_scope.as_str())));
        assert!(roots.iter().any(|root| {
            root.harness == "codex" && root.path == directory.path().join(".codex") && root.nested
        }));
        assert!(roots.iter().any(|root| {
            root.harness == "claude_code"
                && root.path == directory.path().join(".claude")
                && root.nested
        }));
        assert!(roots.iter().any(|root| {
            root.harness == "codex"
                && root.path == directory.path()
                && root.direct_names == ["AGENTS.md"]
        }));
        assert!(roots.iter().any(|root| {
            root.harness == "claude_code"
                && root.path == directory.path()
                && root.direct_names == ["CLAUDE.md"]
        }));
    }

    #[test]
    fn duplicate_physical_roots_prefer_global_attribution() {
        let directory = tempfile::tempdir().unwrap();
        let global = config_root(directory.path(), None);
        let project = config_root(directory.path(), Some("synthetic-project"));

        let roots = deduplicate_roots(vec![global.clone(), project]);

        assert_eq!(roots, vec![global]);
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

        let (snapshot, _) = discover_snapshot(&[super::ConfigRoot {
            harness: "codex".into(),
            path: root,
            scope: None,
            direct_names: CODEX_CONFIG_NAMES,
            nested: true,
        }]);
        assert_eq!(snapshot.len(), 2);
        assert!(snapshot.keys().all(|key| !key.contains("sessions")));
    }

    #[test]
    fn safe_paths_are_resolved_relative_to_the_harness_root() {
        let root = Path::new("/tmp/synthetic-root");
        let config_root = config_root(root, None);
        assert!(is_safe_config_path(&config_root, &root.join("config.toml")));
        assert!(is_safe_config_path(
            &config_root,
            &root.join("hooks/example.json")
        ));
        assert!(is_safe_config_path(
            &config_root,
            &root.join("skills/example/SKILL.md")
        ));
        assert!(!is_safe_config_path(
            &config_root,
            Path::new("/tmp/outside/config.toml")
        ));
        assert!(!is_safe_config_path(
            &config_root,
            &root.join("cache/config.toml")
        ));
        assert!(!is_safe_config_path(
            &config_root,
            &root.join("hooks/example.txt")
        ));
        assert!(!is_safe_config_path(
            &config_root,
            &root.join("nested/config.toml")
        ));
    }

    #[test]
    fn config_events_capture_change_kind_and_redacted_metadata() {
        let path = Path::new("/synthetic/config.toml");
        let config_root = config_root(Path::new("/synthetic"), Some("/synthetic/project"));
        let created = event_for(path, &config_root, None, Some(&("new".into(), 3)));
        assert_eq!(created.kind, "created");
        assert_eq!(created.metadata["harness"], "codex");
        assert_eq!(created.metadata["content_hash"], "new");
        let scope = created.scope.as_deref().unwrap();
        assert!(scope.starts_with("project:"));
        assert!(!scope.contains("synthetic"));
        assert_eq!(
            created.metadata["safe_diff"],
            "size 0 -> 3 bytes; content hash changed"
        );
        assert!(!created.metadata.contains_key("path"));

        let removed = event_for(path, &config_root, Some(&("old".into(), 2)), None);
        assert_eq!(removed.kind, "removed");
        assert_eq!(removed.metadata["previous_hash"], "old");

        let modified = event_for(
            path,
            &config_root,
            Some(&("old".into(), 2)),
            Some(&("new".into(), 3)),
        );
        assert_eq!(modified.kind, "modified");
        let roots = [config_root];
        assert_eq!(
            tracked_root_for_path(path, &roots).map(|root| root.harness.as_str()),
            Some("codex")
        );
        assert!(tracked_root_for_path(Path::new("/other/config.toml"), &roots).is_none());
    }
}
