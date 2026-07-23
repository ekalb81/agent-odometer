use crate::config_events::ConfigWatcherHandle;
use crate::correlation::ExternalEvent;
use crate::model::Session;
use crate::watcher::WatcherHandle;
use dashmap::DashMap;
use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};

const MAX_EXTERNAL_EVENTS: usize = 10_000;

#[derive(Debug)]
struct PathSessionState {
    generation: u64,
    session_id: String,
    watcher_touched: bool,
}

pub struct ExternalEventStore {
    events: VecDeque<ExternalEvent>,
    ids: HashSet<String>,
}

impl ExternalEventStore {
    fn new(events: Vec<ExternalEvent>) -> Self {
        let mut store = Self {
            events: VecDeque::with_capacity(MAX_EXTERNAL_EVENTS),
            ids: HashSet::with_capacity(MAX_EXTERNAL_EVENTS),
        };
        store.extend(events);
        store
    }

    fn extend(&mut self, events: impl IntoIterator<Item = ExternalEvent>) {
        for event in events {
            if !self.ids.insert(event.id.clone()) {
                continue;
            }
            self.events.push_back(event);
            while self.events.len() > MAX_EXTERNAL_EVENTS {
                if let Some(removed) = self.events.pop_front() {
                    self.ids.remove(&removed.id);
                }
            }
        }
    }

    fn snapshot(&self) -> Vec<ExternalEvent> {
        self.events.iter().cloned().collect()
    }
}

pub struct AppState {
    pub sessions: DashMap<String, Arc<Session>>,
    pub scanned: AtomicBool,
    /// Files processed / discovered by the current bulk scan, for the UI's
    /// startup progress indicator.
    pub scan_done: AtomicUsize,
    pub scan_total: AtomicUsize,
    /// Duration of the last completed scan in ms (0 = none yet).
    pub scan_elapsed_ms: AtomicU64,
    /// Identifies the configuration generation allowed to publish scan work.
    pub scan_generation: AtomicU64,
    /// Serializes configuration transitions so watcher/scan generations cannot interleave.
    pub config_transition: Mutex<()>,
    pub watcher: Mutex<Option<WatcherHandle>>,
    pub config_watcher: Mutex<Option<ConfigWatcherHandle>>,
    session_paths: DashMap<String, PathSessionState>,
    pub external_events: Mutex<ExternalEventStore>,
    pub performance: crate::performance::PerformanceRecorder,
    pub tray: Mutex<Option<crate::tray::TrayState>>,
    pub tray_available: AtomicBool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            scanned: AtomicBool::new(false),
            scan_done: AtomicUsize::new(0),
            scan_total: AtomicUsize::new(0),
            scan_elapsed_ms: AtomicU64::new(0),
            // Startup watcher events and the initial bulk scan share generation 1.
            scan_generation: AtomicU64::new(1),
            config_transition: Mutex::new(()),
            watcher: Mutex::new(None),
            config_watcher: Mutex::new(None),
            session_paths: DashMap::new(),
            external_events: Mutex::new(ExternalEventStore::new(
                crate::config_events::load_events(),
            )),
            performance: crate::performance::PerformanceRecorder::default(),
            tray: Mutex::new(None),
            tray_available: AtomicBool::new(false),
        }
    }

    pub fn current_scan_generation(&self) -> u64 {
        self.scan_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn advance_scan_generation(&self) -> u64 {
        self.scan_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            + 1
    }

    pub fn clear_sessions(&self) {
        self.session_paths.clear();
        self.sessions.clear();
    }

    /// Publishes a bulk-scan result unless the live watcher already observed
    /// this path in the same generation. The path entry serializes scan and
    /// watcher publication so an older scan cannot win a last-write race.
    pub fn publish_scanned_session(&self, generation: u64, path: &Path, session: Session) -> bool {
        if self.current_scan_generation() != generation {
            return false;
        }
        let key = path_key(path);
        let id = session.id.clone();
        let mut path_state = self
            .session_paths
            .entry(key)
            .or_insert_with(|| PathSessionState {
                generation,
                session_id: id.clone(),
                watcher_touched: false,
            });
        if self.current_scan_generation() != generation {
            return false;
        }
        if path_state.generation == generation && path_state.watcher_touched {
            return false;
        }
        let replaced = (path_state.generation == generation)
            .then(|| path_state.session_id.clone())
            .filter(|previous| previous != &id);
        *path_state = PathSessionState {
            generation,
            session_id: id.clone(),
            watcher_touched: false,
        };
        self.sessions.insert(id, Arc::new(session));
        drop(path_state);
        if let Some(previous) = replaced {
            self.remove_session_if_unreferenced(&previous, generation);
        }
        true
    }

    pub fn publish_watched_session(&self, path: &Path, session: Session) {
        let generation = self.current_scan_generation();
        let key = path_key(path);
        let id = session.id.clone();
        let mut path_state = self
            .session_paths
            .entry(key)
            .or_insert_with(|| PathSessionState {
                generation,
                session_id: id.clone(),
                watcher_touched: true,
            });
        let replaced = (path_state.generation == generation)
            .then(|| path_state.session_id.clone())
            .filter(|previous| previous != &id);
        *path_state = PathSessionState {
            generation,
            session_id: id.clone(),
            watcher_touched: true,
        };
        self.sessions.insert(id, Arc::new(session));
        drop(path_state);
        if let Some(previous) = replaced {
            self.remove_session_if_unreferenced(&previous, generation);
        }
    }

    pub fn remove_session_path(&self, path: &Path) -> Option<(String, bool)> {
        let generation = self.current_scan_generation();
        let (_, removed) = self.session_paths.remove(&path_key(path))?;
        let removed_from_sessions = removed.generation == generation
            && self.remove_session_if_unreferenced(&removed.session_id, generation);
        Some((removed.session_id, removed_from_sessions))
    }

    fn remove_session_if_unreferenced(&self, session_id: &str, generation: u64) -> bool {
        let referenced = self
            .session_paths
            .iter()
            .any(|entry| entry.generation == generation && entry.session_id.as_str() == session_id);
        if !referenced {
            self.sessions.remove(session_id);
            true
        } else {
            false
        }
    }

    pub fn external_events_snapshot(&self) -> Vec<ExternalEvent> {
        self.external_events.lock().unwrap().snapshot()
    }

    pub fn push_external_event(&self, event: ExternalEvent) {
        self.external_events.lock().unwrap().extend([event]);
    }

    pub fn extend_external_events(&self, events: impl IntoIterator<Item = ExternalEvent>) {
        self.external_events.lock().unwrap().extend(events);
    }
}

fn path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    let value = value.strip_prefix(r"\\?\").unwrap_or(&value);
    let normalized = value.replace('\\', "/");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

// Constructing the full Tauri/Wry AppState in a Windows unit-test binary
// eagerly links GUI entry points before the Rust test harness starts. CI runs
// these platform-independent state-machine tests on Linux; Windows still
// compiles the production paths and exercises parser/cache integration tests.
#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use crate::model::{Harness, TokenTotals};
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn state() -> AppState {
        AppState {
            sessions: DashMap::new(),
            scanned: AtomicBool::new(false),
            scan_done: AtomicUsize::new(0),
            scan_total: AtomicUsize::new(0),
            scan_elapsed_ms: AtomicU64::new(0),
            scan_generation: AtomicU64::new(1),
            config_transition: Mutex::new(()),
            watcher: Mutex::new(None),
            config_watcher: Mutex::new(None),
            session_paths: DashMap::new(),
            external_events: Mutex::new(ExternalEventStore::new(Vec::new())),
            performance: crate::performance::PerformanceRecorder::default(),
            tray: Mutex::new(None),
            tray_available: AtomicBool::new(false),
        }
    }

    fn session(id: &str, turns: u32) -> Session {
        Session {
            id: id.into(),
            harness: Harness::Codex,
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            archived: false,
            started_at: Utc::now(),
            last_event_at: Utc::now(),
            working_directory: None,
            originator: None,
            source: None,
            history_mode: None,
            memory_mode: None,
            cli_version: None,
            model_provider: None,
            model: None,
            service_tier: None,
            plan_type: None,
            credits_unlimited: None,
            credits_balance: None,
            context_window: None,
            latest_context_tokens: None,
            total_turns: turns,
            first_user_message: None,
            tokens_total: TokenTotals::default(),
            tokens_by_model: HashMap::new(),
            tokens_history: Vec::new(),
            turns: Vec::new(),
            tool_observations: Vec::new(),
            tool_metrics: Default::default(),
            tool_metrics_by_model: Default::default(),
            category_totals: Default::default(),
            optimization_findings: Vec::new(),
        }
    }

    #[test]
    fn watcher_touch_prevents_older_bulk_scan_overwrite() {
        let state = state();
        let path = PathBuf::from("C:/sessions/a.jsonl");
        state.publish_watched_session(&path, session("a", 2));
        assert!(!state.publish_scanned_session(1, &path, session("a", 1)));
        assert_eq!(state.sessions.get("a").unwrap().total_turns, 2);
    }

    #[test]
    fn stale_scan_generation_cannot_publish_or_complete_new_state() {
        let state = state();
        let path = PathBuf::from("C:/sessions/a.jsonl");
        let stale = state.current_scan_generation();
        state.advance_scan_generation();
        state.clear_sessions();
        assert!(!state.publish_scanned_session(stale, &path, session("a", 1)));
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn removing_bulk_scanned_path_removes_session() {
        let state = state();
        let path = PathBuf::from("C:/sessions/a.jsonl");
        assert!(state.publish_scanned_session(1, &path, session("a", 1)));
        assert_eq!(state.remove_session_path(&path), Some(("a".into(), true)));
        assert!(state.sessions.is_empty());
    }

    #[test]
    fn external_events_are_deduplicated_and_bounded() {
        let state = state();
        for index in 0..=MAX_EXTERNAL_EVENTS {
            state.push_external_event(ExternalEvent {
                id: index.to_string(),
                timestamp: Utc::now(),
                scope: None,
                source: "test".into(),
                kind: "change".into(),
                metadata: Default::default(),
            });
        }
        state.push_external_event(ExternalEvent {
            id: MAX_EXTERNAL_EVENTS.to_string(),
            timestamp: Utc::now(),
            scope: None,
            source: "test".into(),
            kind: "duplicate".into(),
            metadata: Default::default(),
        });
        let events = state.external_events_snapshot();
        assert_eq!(events.len(), MAX_EXTERNAL_EVENTS);
        assert_eq!(events.first().unwrap().id, "1");
        assert_eq!(events.last().unwrap().kind, "change");
    }
}
