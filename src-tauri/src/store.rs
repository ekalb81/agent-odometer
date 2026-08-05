use crate::config_events::ConfigWatcherHandle;
use crate::correlation::ExternalEvent;
use crate::history_store::HistoryStore;
use crate::model::{Session, SourceAvailability};
use crate::scanner::ScanReport;
use crate::watcher::WatcherHandle;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::collections::{HashSet, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize};
use std::sync::{Arc, Condvar, Mutex};

const MAX_EXTERNAL_EVENTS: usize = 10_000;

/// Durable-history archive lifecycle (#116). `AppState::new()` used to open
/// and migrate the archive synchronously before Tauri's `.setup()` ran,
/// which meant a chained schema migration on an existing install completed
/// before any window existed. It now starts `Pending` and
/// `commands::spawn_history_open` resolves it to `Ready`/`Unavailable` on a
/// background thread, so a window can appear immediately.
///
/// `Pending` is a distinct state from `Unavailable`, deliberately: treating
/// it as "unavailable" would make every accounting query fall back to
/// whatever the in-memory scan has managed to observe so far (itself gated
/// behind this same readiness signal, see `spawn_scan`) and report it as
/// though it were complete. `commands::sessions_in_ranges` — the accounting
/// authority — checks this explicitly and returns an honest "still
/// preparing" error instead.
#[derive(Clone)]
enum HistoryReadiness {
    Pending,
    Ready(Arc<HistoryStore>),
    Unavailable,
}

/// The serializable shape of [`HistoryReadiness`], for IPC status reporting
/// (`commands::get_history_status`, `commands::sessions_in_ranges`). Never
/// carries the store itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryReadinessKind {
    Pending,
    Ready,
    Unavailable,
}

impl From<&HistoryReadiness> for HistoryReadinessKind {
    fn from(value: &HistoryReadiness) -> Self {
        match value {
            HistoryReadiness::Pending => Self::Pending,
            HistoryReadiness::Ready(_) => Self::Ready,
            HistoryReadiness::Unavailable => Self::Unavailable,
        }
    }
}

/// Snapshot of the durable-history migration's most recently reported step,
/// retained so `commands::get_history_status` can answer correctly for a
/// listener that attaches after progress events have already fired — mirrors
/// `ScanStatus`'s "call once on mount, then follow events" contract (#116).
#[derive(Debug, Clone)]
pub struct HistoryStepSnapshot {
    pub step: String,
    pub step_index: u32,
    pub step_total: u32,
    pub items_done: Option<usize>,
    pub items_total: Option<usize>,
    pub elapsed_ms: Option<u64>,
}

/// The most recently completed bulk/incremental scan's per-provider counters,
/// retained for the provider diagnostics report (issue #39). This is a plain
/// cache of the last `ScanReport`; it is never a source of truth and is
/// overwritten by the next scan.
#[derive(Debug, Clone)]
pub struct ScanSummary {
    pub completed_at: DateTime<Utc>,
    pub report: ScanReport,
}

#[derive(Debug)]
struct PathSessionState {
    generation: u64,
    storage_id: String,
    watcher_touched: bool,
    /// A remove event in this generation. Keep the path entry as a tombstone
    /// so an already-parsed bulk-scan callback cannot resurrect it.
    removed: bool,
}

/// A durable reconciliation result. `displaced` is populated when one source
/// path was reassigned to a distinct logical transcript, allowing callers to
/// update the old session's source availability immediately.
#[derive(Debug, Clone)]
pub struct ReconciledSession {
    pub session: Session,
    pub displaced: Option<Session>,
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
    /// Sessions are keyed by their local durable storage ID, never by a
    /// provider's mutable/reused transcript ID.
    pub sessions: DashMap<String, Arc<Session>>,
    /// Long-lived local archive lifecycle (#116): `Pending` until
    /// `commands::spawn_history_open` resolves it. A failure to open it
    /// leaves live parsing available, but is logged rather than silently
    /// treated as a cache. Use [`AppState::history_ready`] /
    /// [`AppState::history_readiness`] rather than matching this directly.
    history: Mutex<HistoryReadiness>,
    /// Blocks `wait_for_history_ready` callers until `history` leaves
    /// `Pending`.
    history_ready_cv: Condvar,
    /// The migration's most recently reported step, for `get_history_status`.
    last_history_step: Mutex<Option<HistoryStepSnapshot>>,
    /// The durable-store generation currently associated with the active
    /// bulk scan. Watcher writes use it when available so a file created
    /// after scan discovery cannot be marked missing at scan completion.
    history_scan_generation: AtomicI64,
    pub scanned: AtomicBool,
    /// Files processed / discovered by the current bulk scan, for the UI's
    /// startup progress indicator.
    pub scan_done: AtomicUsize,
    pub scan_total: AtomicUsize,
    /// Duration of the last completed scan in ms (0 = none yet).
    pub scan_elapsed_ms: AtomicU64,
    /// Why the current/last scan's cache could not be treated as fully warm;
    /// None for an ordinary warm scan. Set once the cache is opened, before
    /// the scan's progress events start firing.
    pub cold_reason: Mutex<Option<crate::scan_cache::ColdReason>>,
    /// Identifies the configuration generation allowed to publish scan work.
    pub scan_generation: AtomicU64,
    /// Identifies the instruction-inventory scan allowed to publish results.
    instruction_scan_generation: AtomicU64,
    /// Serializes configuration transitions so watcher/scan generations cannot interleave.
    pub config_transition: Mutex<()>,
    pub watcher: Mutex<Option<WatcherHandle>>,
    pub config_watcher: Mutex<Option<ConfigWatcherHandle>>,
    instruction_paths: Mutex<HashSet<String>>,
    session_paths: DashMap<String, PathSessionState>,
    /// Storage ids whose most recent history-store persist failed: their
    /// ledger rows may be stale or absent, so ledger-backed aggregation must
    /// compute exactly these sessions from in-memory history instead.
    /// Cleared per id on the next successful persist.
    ledger_stale: DashMap<String, ()>,
    pub external_events: Mutex<ExternalEventStore>,
    pub performance: crate::performance::PerformanceRecorder,
    pub tray: Mutex<Option<crate::tray::TrayState>>,
    pub tray_available: AtomicBool,
    /// Per-provider counters from the most recently completed scan, read by
    /// the on-demand provider diagnostics report. Never populated eagerly
    /// beyond the scan that already runs at startup/config-save.
    last_scan_report: Mutex<Option<ScanSummary>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            history: Mutex::new(HistoryReadiness::Pending),
            history_ready_cv: Condvar::new(),
            last_history_step: Mutex::new(None),
            history_scan_generation: AtomicI64::new(0),
            scanned: AtomicBool::new(false),
            scan_done: AtomicUsize::new(0),
            scan_total: AtomicUsize::new(0),
            scan_elapsed_ms: AtomicU64::new(0),
            cold_reason: Mutex::new(None),
            // Startup watcher events and the initial bulk scan share generation 1.
            scan_generation: AtomicU64::new(1),
            instruction_scan_generation: AtomicU64::new(0),
            config_transition: Mutex::new(()),
            watcher: Mutex::new(None),
            config_watcher: Mutex::new(None),
            instruction_paths: Mutex::new(HashSet::new()),
            session_paths: DashMap::new(),
            ledger_stale: DashMap::new(),
            external_events: Mutex::new(ExternalEventStore::new(
                crate::config_events::load_events(),
            )),
            performance: crate::performance::PerformanceRecorder::default(),
            tray: Mutex::new(None),
            tray_available: AtomicBool::new(false),
            last_scan_report: Mutex::new(None),
        }
    }

    /// The archive when it is reachable (`Ready`); `None` while it is still
    /// opening (`Pending`) or is genuinely unavailable (`Unavailable`).
    /// Every call site that used to match `Option<Arc<HistoryStore>>`
    /// directly now goes through this, so `Pending` degrades exactly like
    /// `Unavailable` did before #116 — never like a full archive.
    pub fn history_ready(&self) -> Option<Arc<HistoryStore>> {
        match &*self.history.lock().unwrap() {
            HistoryReadiness::Ready(store) => Some(store.clone()),
            HistoryReadiness::Pending | HistoryReadiness::Unavailable => None,
        }
    }

    /// The archive's current lifecycle state, for status reporting
    /// (`commands::get_history_status`) and for the one caller that must
    /// tell `Pending` apart from `Unavailable` rather than treating both as
    /// "no archive" — `commands::sessions_in_ranges`, the ledger accounting
    /// authority (#116).
    pub fn history_readiness(&self) -> HistoryReadinessKind {
        HistoryReadinessKind::from(&*self.history.lock().unwrap())
    }

    /// Blocks the calling thread until the archive has left `Pending`.
    /// `commands::spawn_scan`'s background thread calls this before its
    /// first `observe()`, preserving the ordering `AppState::new()` used to
    /// guarantee synchronously before #116: every archived session is
    /// hydrated into `sessions` (by [`Self::set_history_ready`]) before a
    /// bulk scan can publish its first result, so the scan can never race
    /// `hydrate_history`'s wholesale overwrite of the in-memory map.
    pub fn wait_for_history_ready(&self) -> HistoryReadinessKind {
        let guard = self.history.lock().unwrap();
        let guard = self
            .history_ready_cv
            .wait_while(guard, |state| matches!(state, HistoryReadiness::Pending))
            .unwrap();
        HistoryReadinessKind::from(&*guard)
    }

    /// Resolves `Pending` to `Ready`/`Unavailable`. The only caller is
    /// `commands::spawn_history_open`'s background thread, exactly once per
    /// run. Wakes every `wait_for_history_ready` waiter and then hydrates
    /// the in-memory projection from the now-reachable archive — the same
    /// step `AppState::new()` used to perform synchronously before #116.
    pub fn set_history_ready(&self, store: Option<Arc<HistoryStore>>) {
        {
            let mut guard = self.history.lock().unwrap();
            *guard = match store {
                Some(store) => HistoryReadiness::Ready(store),
                None => HistoryReadiness::Unavailable,
            };
        }
        self.history_ready_cv.notify_all();
        self.hydrate_history();
    }

    /// Records the migration's most recently reported step, for
    /// `commands::get_history_status` to answer a listener that attaches
    /// after progress events already fired.
    pub fn record_history_step(&self, snapshot: HistoryStepSnapshot) {
        *self.last_history_step.lock().unwrap() = Some(snapshot);
    }

    pub fn last_history_step(&self) -> Option<HistoryStepSnapshot> {
        self.last_history_step.lock().unwrap().clone()
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

    pub fn begin_instruction_scan(&self) -> u64 {
        // Starting a new generation must serialize with allowlist publication.
        // Otherwise an older scan could pass its generation check, lose the
        // race to this increment, and still replace the allowlist afterward.
        let _paths = self.instruction_paths.lock().unwrap();
        self.instruction_scan_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            + 1
    }

    pub fn cancel_instruction_scan(&self) -> u64 {
        // Cancellation and publication share the allowlist lock so a cancelled
        // generation cannot publish after this method returns.
        let _paths = self.instruction_paths.lock().unwrap();
        self.instruction_scan_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
    }

    pub fn cancel_instruction_scan_and_clear_paths(&self) -> u64 {
        let mut paths = self.instruction_paths.lock().unwrap();
        let cancelled = self
            .instruction_scan_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        paths.clear();
        cancelled
    }

    pub fn instruction_scan_is_current(&self, generation: u64) -> bool {
        self.instruction_scan_generation
            .load(std::sync::atomic::Ordering::Acquire)
            == generation
    }

    pub fn clear_sessions(&self) {
        self.session_paths.clear();
    }

    /// Starts a durable location-observation generation for a bulk scan.
    /// The scan is still safe when the archive is unavailable: live session
    /// presentation continues, but no durability claim is made.
    pub fn begin_history_scan(&self) -> Option<i64> {
        let history = self.history_ready()?;
        match history.begin_scan() {
            Ok(generation) => {
                self.history_scan_generation
                    .store(generation, std::sync::atomic::Ordering::Release);
                Some(generation)
            }
            Err(error) => {
                tracing::warn!("could not begin durable history scan: {}", error);
                None
            }
        }
    }

    /// Reconciles a freshly parsed source with the durable archive before it
    /// can enter the in-memory projection. This is what makes a moved file
    /// retain its identity and lets duplicate provider IDs be disambiguated.
    pub fn reconcile_observed_session(&self, path: &Path, session: Session) -> ReconciledSession {
        let generation = self
            .history_scan_generation
            .load(std::sync::atomic::Ordering::Acquire);
        self.reconcile_session_at_generation(path, session, generation)
    }

    /// Variant for bulk scans. The generation belongs to that scan rather
    /// than the mutable live watcher generation, so a superseded scan cannot
    /// accidentally stamp locations as seen by a newer one.
    pub fn reconcile_scanned_session_if_current(
        &self,
        app_generation: u64,
        path: &Path,
        session: Session,
        history_generation: Option<i64>,
    ) -> Option<ReconciledSession> {
        // This closes the otherwise subtle race between an app scan
        // generation check and the durable write: set_config holds the same
        // transition mutex while invalidating a scan generation.
        let _transition = self.config_transition.lock().unwrap();
        if self.current_scan_generation() != app_generation {
            return None;
        }

        // Hold this path's DashMap entry through the durable write. A remove
        // either happened first (and its tombstone rejects this callback), or
        // waits until observe has completed and then marks the location
        // missing. That prevents a late scan result from resurrecting a
        // removed source in either the archive or the live projection.
        let key = path_key(path);
        let path_state = self
            .session_paths
            .entry(key)
            .or_insert_with(|| PathSessionState {
                generation: app_generation,
                storage_id: String::new(),
                watcher_touched: false,
                removed: false,
            });
        if path_state.generation == app_generation
            && (path_state.watcher_touched || path_state.removed)
        {
            return None;
        }
        let reconciled =
            self.reconcile_session_at_generation(path, session, history_generation.unwrap_or(0));
        drop(path_state);
        Some(reconciled)
    }

    fn reconcile_session_at_generation(
        &self,
        path: &Path,
        session: Session,
        generation: i64,
    ) -> ReconciledSession {
        let Some(history) = self.history_ready() else {
            return ReconciledSession {
                session,
                displaced: None,
            };
        };
        match history.observe_with_displaced(path, &session, generation) {
            Ok((stored, displaced)) => {
                self.ledger_stale.remove(&stored.key);
                let displaced = displaced.map(|stored| {
                    self.sessions
                        .insert(stored.key, Arc::new(stored.session.clone()));
                    stored.session
                });
                ReconciledSession {
                    session: stored.session,
                    displaced,
                }
            }
            Err(error) => {
                tracing::warn!(
                    "could not persist session history for {:?}: {}",
                    path,
                    error
                );
                self.ledger_stale.insert(session.effective_storage_id(), ());
                ReconciledSession {
                    session,
                    displaced: None,
                }
            }
        }
    }

    /// True when this session's ledger rows cannot be trusted for
    /// aggregation because its latest persist failed.
    pub fn ledger_is_stale(&self, storage_id: &str) -> bool {
        self.ledger_stale.contains_key(storage_id)
    }

    /// Completes an error-free current scan and rehydrates the archive so
    /// source removals become an explicit Missing state instead of deletes.
    pub fn finish_history_scan(&self, generation: i64) -> Vec<Session> {
        let Some(history) = self.history_ready() else {
            return Vec::new();
        };
        if let Err(error) = history.finish_scan(generation) {
            tracing::warn!("could not finish durable history scan: {}", error);
            return Vec::new();
        }
        self.hydrate_history()
    }

    /// Loads every archived session into the in-memory projection. The store
    /// is authoritative for availability; caller can emit returned changed
    /// sessions after an initial hydration or scan completion.
    pub fn hydrate_history(&self) -> Vec<Session> {
        let Some(history) = self.history_ready() else {
            return Vec::new();
        };
        let stored = match history.load_sessions() {
            Ok(stored) => stored,
            Err(error) => {
                tracing::warn!("could not load durable session history: {}", error);
                return Vec::new();
            }
        };
        // Dirty markings survive restarts in the store itself; rebuild the
        // in-process stale set so ledger-backed aggregation keeps routing
        // these sessions through in-memory history.
        match history.dirty_session_keys() {
            Ok(keys) => {
                for key in keys {
                    self.ledger_stale.insert(key, ());
                }
            }
            Err(error) => {
                tracing::warn!("could not load ledger-dirty markings: {}", error);
            }
        }
        let mut changed = Vec::new();
        for stored in stored {
            let session = stored.session;
            let key = stored.key;
            // Scan callbacks already emit fresh present snapshots. Here we
            // only need to announce availability/path transitions caused by
            // archive reconciliation (most notably a missing source).
            let is_changed = self.sessions.get(&key).is_none_or(|existing| {
                existing.source_availability != session.source_availability
                    || existing.file_path != session.file_path
            });
            self.sessions.insert(key, Arc::new(session.clone()));
            if is_changed {
                changed.push(session);
            }
        }
        changed
    }

    /// Persists a metadata-only in-memory overlay (for example a thread name
    /// from the Codex session index) without changing source ownership.
    pub fn persist_session_metadata(&self, session: &Session) {
        let Some(history) = self.history_ready() else {
            return;
        };
        if let Err(error) = history.update_snapshot(session) {
            tracing::warn!(
                "could not persist metadata-only session snapshot for {}: {}",
                session.effective_storage_id(),
                error
            );
        }
    }

    /// Converts a physical source deletion into a retained, availability-
    /// marked session. It never removes the logical session from memory.
    pub fn mark_source_missing(&self, path: &Path) -> Option<Session> {
        // Write the tombstone before touching SQLite. A bulk worker that has
        // already parsed this path must see it before it can observe/publish
        // stale Present state.
        let key = path_key(path);
        let storage_id = self
            .session_paths
            .get(&key)
            .map(|state| state.storage_id.clone());
        let generation = self.current_scan_generation();
        self.session_paths
            .entry(key.clone())
            .and_modify(|state| {
                state.generation = generation;
                state.watcher_touched = true;
                state.removed = true;
            })
            .or_insert_with(|| PathSessionState {
                generation,
                storage_id: storage_id.clone().unwrap_or_default(),
                watcher_touched: true,
                removed: true,
            });
        let persisted =
            self.history_ready()
                .and_then(|history| match history.mark_path_missing(path) {
                    Ok(stored) => stored,
                    Err(error) => {
                        tracing::warn!(
                            "could not mark durable source missing for {:?}: {}",
                            path,
                            error
                        );
                        None
                    }
                });
        let durable_storage_id = persisted.as_ref().map(|stored| stored.key.clone());
        if let Some(durable_storage_id) = durable_storage_id {
            if let Some(mut state) = self.session_paths.get_mut(&key) {
                state.storage_id = durable_storage_id;
            }
        }
        if let Some(stored) = persisted {
            self.sessions
                .insert(stored.key, Arc::new(stored.session.clone()));
            return Some(stored.session);
        }
        let storage_id = storage_id?;
        let still_present_elsewhere = self
            .session_paths
            .iter()
            .any(|entry| entry.storage_id == storage_id && !entry.removed);
        if still_present_elsewhere {
            return self
                .sessions
                .get(&storage_id)
                .map(|session| session.as_ref().clone());
        }
        let mut session = self.sessions.get_mut(&storage_id)?;
        let session = std::sync::Arc::make_mut(session.value_mut());
        session.source_availability = SourceAvailability::Missing;
        Some(session.clone())
    }

    pub fn publish_instruction_paths_if_current(
        &self,
        generation: u64,
        paths: impl IntoIterator<Item = String>,
    ) -> bool {
        let mut allowed = self.instruction_paths.lock().unwrap();
        if !self.instruction_scan_is_current(generation) {
            return false;
        }
        *allowed = paths.into_iter().collect();
        true
    }

    pub fn instruction_path_allowed(&self, path: &Path) -> bool {
        self.instruction_paths
            .lock()
            .unwrap()
            .contains(&path_key(path))
    }

    /// Publishes a bulk-scan result unless the live watcher already observed
    /// this path in the same generation. The path entry serializes scan and
    /// watcher publication so an older scan cannot win a last-write race.
    pub fn publish_scanned_session(&self, generation: u64, path: &Path, session: Session) -> bool {
        if self.current_scan_generation() != generation {
            return false;
        }
        let key = path_key(path);
        let storage_id = session.effective_storage_id();
        let mut path_state = self
            .session_paths
            .entry(key)
            .or_insert_with(|| PathSessionState {
                generation,
                storage_id: storage_id.clone(),
                watcher_touched: false,
                removed: false,
            });
        if self.current_scan_generation() != generation {
            return false;
        }
        if path_state.generation == generation && (path_state.watcher_touched || path_state.removed)
        {
            return false;
        }
        let replaced = (path_state.generation == generation)
            .then(|| path_state.storage_id.clone())
            .filter(|previous| previous != &storage_id);
        *path_state = PathSessionState {
            generation,
            storage_id: storage_id.clone(),
            watcher_touched: false,
            removed: false,
        };
        self.sessions.insert(storage_id, Arc::new(session));
        drop(path_state);
        let _ = replaced;
        true
    }

    pub fn publish_watched_session(&self, path: &Path, session: Session) {
        let generation = self.current_scan_generation();
        let key = path_key(path);
        let storage_id = session.effective_storage_id();
        let mut path_state = self
            .session_paths
            .entry(key)
            .or_insert_with(|| PathSessionState {
                generation,
                storage_id: storage_id.clone(),
                watcher_touched: true,
                removed: false,
            });
        let replaced = (path_state.generation == generation)
            .then(|| path_state.storage_id.clone())
            .filter(|previous| previous != &storage_id);
        *path_state = PathSessionState {
            generation,
            storage_id: storage_id.clone(),
            watcher_touched: true,
            removed: false,
        };
        self.sessions.insert(storage_id, Arc::new(session));
        drop(path_state);
        let _ = replaced;
    }

    pub fn remove_session_path(&self, path: &Path) -> Option<String> {
        self.session_paths
            .remove(&path_key(path))
            .map(|(_, removed)| removed.storage_id)
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

    /// Records the most recently completed scan's counters for the provider
    /// diagnostics report. Overwrites any previous summary.
    pub fn record_scan_report(&self, report: ScanReport) {
        *self.last_scan_report.lock().unwrap() = Some(ScanSummary {
            completed_at: Utc::now(),
            report,
        });
    }

    /// The last completed scan's counters, if any scan has finished yet.
    pub fn last_scan_summary(&self) -> Option<ScanSummary> {
        self.last_scan_report.lock().unwrap().clone()
    }
}

fn path_key(path: &Path) -> String {
    crate::paths::normalized_path_key(path, false)
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
    use crate::model::TokenTotals;
    use crate::provider::codex_provider_id;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn state() -> AppState {
        AppState {
            sessions: DashMap::new(),
            history: Mutex::new(HistoryReadiness::Unavailable),
            history_ready_cv: Condvar::new(),
            last_history_step: Mutex::new(None),
            history_scan_generation: AtomicI64::new(0),
            scanned: AtomicBool::new(false),
            scan_done: AtomicUsize::new(0),
            scan_total: AtomicUsize::new(0),
            scan_elapsed_ms: AtomicU64::new(0),
            cold_reason: Mutex::new(None),
            scan_generation: AtomicU64::new(1),
            instruction_scan_generation: AtomicU64::new(0),
            config_transition: Mutex::new(()),
            watcher: Mutex::new(None),
            config_watcher: Mutex::new(None),
            instruction_paths: Mutex::new(HashSet::new()),
            session_paths: DashMap::new(),
            ledger_stale: DashMap::new(),
            external_events: Mutex::new(ExternalEventStore::new(Vec::new())),
            performance: crate::performance::PerformanceRecorder::default(),
            tray: Mutex::new(None),
            tray_available: AtomicBool::new(false),
            last_scan_report: Mutex::new(None),
        }
    }

    fn session(id: &str, turns: u32) -> Session {
        Session {
            id: id.into(),
            storage_id: format!("codex:thread:{id}"),
            harness: codex_provider_id(),
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            source_availability: Default::default(),
            archived: false,
            started_at: Utc::now(),
            last_event_at: Utc::now(),
            working_directory: None,
            originator: None,
            source: None,
            subagent_id_is_path_fallback: false,
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
            rate_limits_history: Vec::new(),
            turns: Vec::new(),
            tool_observations: Vec::new(),
            tool_metrics: Default::default(),
            tool_metrics_by_model: Default::default(),
            category_totals: Default::default(),
            optimization_findings: Vec::new(),
            project_key: None,
            project_label: None,
            project_provenance: None,
        }
    }

    #[test]
    fn watcher_touch_prevents_older_bulk_scan_overwrite() {
        let state = state();
        let path = PathBuf::from("C:/sessions/a.jsonl");
        state.publish_watched_session(&path, session("a", 2));
        assert!(!state.publish_scanned_session(1, &path, session("a", 1)));
        assert_eq!(state.sessions.get("codex:thread:a").unwrap().total_turns, 2);
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
    fn instruction_scan_generation_cancels_prior_work() {
        let state = state();
        let first = state.begin_instruction_scan();
        assert!(state.instruction_scan_is_current(first));
        let second = state.begin_instruction_scan();
        assert!(!state.instruction_scan_is_current(first));
        assert!(state.instruction_scan_is_current(second));
        assert_eq!(state.cancel_instruction_scan(), second);
        assert!(!state.instruction_scan_is_current(second));
    }

    #[test]
    fn cancelled_instruction_scan_cannot_repopulate_cleared_paths() {
        let state = state();
        let path = PathBuf::from("C:/projects/removed/AGENTS.md");
        let path_key = path_key(&path);
        let scan = state.begin_instruction_scan();
        assert!(state.publish_instruction_paths_if_current(scan, [path_key.clone()]));
        assert!(state.instruction_path_allowed(&path));

        assert_eq!(state.cancel_instruction_scan_and_clear_paths(), scan);
        assert!(!state.instruction_path_allowed(&path));
        assert!(!state.publish_instruction_paths_if_current(scan, [path_key]));
        assert!(!state.instruction_path_allowed(&path));
    }

    #[test]
    fn removing_bulk_scanned_path_only_removes_the_path_overlay() {
        let state = state();
        let path = PathBuf::from("C:/sessions/a.jsonl");
        assert!(state.publish_scanned_session(1, &path, session("a", 1)));
        assert_eq!(
            state.remove_session_path(&path),
            Some("codex:thread:a".into())
        );
        assert!(state.sessions.contains_key("codex:thread:a"));
    }

    #[test]
    fn removal_tombstone_rejects_late_same_generation_scan_result() {
        let state = state();
        let path = PathBuf::from("C:/sessions/a.jsonl");
        assert!(state.publish_scanned_session(1, &path, session("a", 1)));
        let missing = state
            .mark_source_missing(&path)
            .expect("published path should resolve to a retained session");
        assert_eq!(missing.source_availability, SourceAvailability::Missing);

        // Simulates a scan worker that parsed this file before the watcher
        // remove event but only reached publication afterward.
        assert!(!state.publish_scanned_session(1, &path, session("a", 2)));
        assert_eq!(
            state
                .sessions
                .get("codex:thread:a")
                .unwrap()
                .source_availability,
            SourceAvailability::Missing
        );
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

    #[test]
    fn history_readiness_starts_pending_and_resolves_to_ready() {
        let state = state();
        assert_eq!(state.history_readiness(), HistoryReadinessKind::Pending);
        assert!(state.history_ready().is_none());

        let directory = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
        state.set_history_ready(Some(Arc::new(store)));

        assert_eq!(state.history_readiness(), HistoryReadinessKind::Ready);
        assert!(state.history_ready().is_some());
    }

    #[test]
    fn history_readiness_resolves_to_unavailable_and_still_degrades_gracefully() {
        // Pending must never be treated like Unavailable by a caller that
        // only checks `history_ready()` while it is transient — but once
        // resolved, Unavailable itself must behave exactly like the
        // pre-#116 "archive never opened" path: observing a session must
        // not panic or block, and the session stays usable in memory.
        let state = state();
        state.set_history_ready(None);
        assert_eq!(state.history_readiness(), HistoryReadinessKind::Unavailable);
        assert!(state.history_ready().is_none());

        let path = PathBuf::from("C:/sessions/a.jsonl");
        let reconciled = state.reconcile_observed_session(&path, session("a", 1));
        assert!(reconciled.displaced.is_none());
        assert_eq!(reconciled.session.id, "a");
    }

    #[test]
    fn wait_for_history_ready_blocks_until_set_history_ready_resolves_it() {
        let state = Arc::new(state());
        assert_eq!(state.history_readiness(), HistoryReadinessKind::Pending);
        let waiter_state = state.clone();
        let waiter = std::thread::spawn(move || waiter_state.wait_for_history_ready());
        // Not required for correctness (set_history_ready always wakes every
        // waiter, however late it arrived) — just makes it likelier this run
        // actually exercises the blocking path rather than winning a race.
        std::thread::sleep(std::time::Duration::from_millis(20));
        state.set_history_ready(None);
        assert_eq!(waiter.join().unwrap(), HistoryReadinessKind::Unavailable);
    }
}
