use crate::config_events::ConfigWatcherHandle;
use crate::correlation::ExternalEvent;
use crate::history_store::HistoryStore;
use crate::model::{ResidentSession, Session, SessionSummary, SourceAvailability};
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
    /// provider's mutable/reused transcript ID. Holds a resident *summary*
    /// of each session (issue #139), not the full content — full sessions
    /// are loaded on demand from the ledger via [`Self::full_session`]/
    /// [`Self::full_sessions`]. Full sessions used to be kept resident here
    /// permanently, which is what drove startup hydration time and steady-
    /// state memory before this change.
    pub sessions: DashMap<String, Arc<ResidentSession>>,
    /// Full session content for exactly the sessions that cannot be trusted
    /// to come back correctly from a fresh ledger read on demand (issue
    /// #139): either this specific session's last durable persist failed
    /// (mirrors `ledger_stale`), or the durable archive is not `Ready` at
    /// all (`Pending`/`Unavailable`) so there is no ledger to read from in
    /// the first place. Both are expected-rare in the `Ready` steady state;
    /// `Unavailable` is the one case where this can grow to the whole
    /// corpus — the same full-residency cost this issue removes from the
    /// common case, kept only because there is genuinely no other durable
    /// source of full content once the archive can never open this run.
    /// Cleared per key the moment a subsequent durable write for that key
    /// succeeds; see [`Self::resident_from_live_parse`].
    full_session_fallback: DashMap<String, Arc<Session>>,
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
    /// Storage ids a bulk scan durably persisted via
    /// [`HistoryStore::observe_bulk`] (issue #132) without also updating
    /// their `rollup_*` rows — the facts are correct and committed, but the
    /// ledger's rollup-backed range aggregation for exactly these sessions
    /// would under-report until the deferred rebuild runs. Distinct from
    /// `ledger_stale` (a genuine persist *failure*, cleared only by a
    /// successful re-persist): this set is cleared wholesale by
    /// [`Self::mark_rollups_rebuilt`] once `rebuild_rollups_if_stale`
    /// completes, and never needs to survive a restart — a crash before that
    /// rebuild is instead caught by `HistoryStore::open`'s own durable
    /// `rollups_stale` marker, which rebuilds before this process can ever
    /// read a rollup row again. See [`Self::ledger_is_stale`].
    rollup_deferred_stale: DashMap<String, ()>,
    pub external_events: Mutex<ExternalEventStore>,
    pub performance: crate::performance::PerformanceRecorder,
    pub tray: Mutex<Option<crate::tray::TrayState>>,
    pub tray_available: AtomicBool,
    /// Per-provider counters from the most recently completed scan, read by
    /// the on-demand provider diagnostics report. Never populated eagerly
    /// beyond the scan that already runs at startup/config-save.
    last_scan_report: Mutex<Option<ScanSummary>>,
    /// Bumped every time `sessions` gains a new/replaced entry or an
    /// existing one's availability changes (issue #128). `quota_snapshots`
    /// compares this against the value it last recomputed from instead of
    /// diffing session content, mirroring how `scan_generation` gates scan
    /// publication rather than re-deriving "did anything change".
    sessions_generation: AtomicU64,
    /// Cached `quota_store::QuotaStoreFile`, loaded from disk once and kept
    /// current only by [`Self::set_quota_store`] (`commands::set_quota_config`'s
    /// write path) rather than reloaded from disk on every
    /// `get_quota_snapshots` call (issue #128).
    quota_store_cache: Mutex<Option<Arc<crate::quota_store::QuotaStoreFile>>>,
    /// Cached `get_quota_snapshots` output; see [`crate::quota::QuotaSnapshotCache`]
    /// (issue #128).
    quota_snapshot_cache: crate::quota::QuotaSnapshotCache,
    /// Incrementally maintained per-provider rate-limit points/credit
    /// observations feeding [`Self::quota_snapshots`]'s recompute path
    /// (issue #131); see [`crate::quota::QuotaPointsIndex`]. Every
    /// `sessions`-mutation site in this file calls `update_session`
    /// alongside `touch_sessions_generation`.
    quota_points_index: crate::quota::QuotaPointsIndex,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            full_session_fallback: DashMap::new(),
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
            rollup_deferred_stale: DashMap::new(),
            external_events: Mutex::new(ExternalEventStore::new(
                crate::config_events::load_events(),
            )),
            performance: crate::performance::PerformanceRecorder::default(),
            tray: Mutex::new(None),
            tray_available: AtomicBool::new(false),
            last_scan_report: Mutex::new(None),
            sessions_generation: AtomicU64::new(0),
            quota_store_cache: Mutex::new(None),
            quota_snapshot_cache: crate::quota::QuotaSnapshotCache::new(),
            quota_points_index: crate::quota::QuotaPointsIndex::new(),
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
    /// Returns the hydration's shape (issue #139) so the caller can attach
    /// it to `startup.history_hydrate` instead of discarding it.
    pub fn set_history_ready(
        &self,
        store: Option<Arc<HistoryStore>>,
    ) -> crate::history_store::HydrationStats {
        {
            let mut guard = self.history.lock().unwrap();
            *guard = match store {
                Some(store) => HistoryReadiness::Ready(store),
                None => HistoryReadiness::Unavailable,
            };
        }
        self.history_ready_cv.notify_all();
        self.hydrate_history()
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

    /// True when `storage_id`'s full content cannot be trusted to come back
    /// correctly from a fresh ledger read right now (issue #139): either the
    /// archive is not `Ready` at all, or this specific session's most recent
    /// durable persist attempt failed (`ledger_stale`). Distinct from
    /// [`Self::ledger_is_stale`], which also includes `rollup_deferred_stale`
    /// — a session whose *rollup* tables lag but whose row-level
    /// `session_json` (what [`HistoryStore::load_one`] reads) is perfectly
    /// current, so it needs no full-residency fallback at all.
    fn needs_full_residency(&self, storage_id: &str) -> bool {
        self.history_ready().is_none() || self.ledger_stale.contains_key(storage_id)
    }

    /// Builds the resident summary for a session whose content is freshly,
    /// authoritatively sourced from the ledger itself — a hydration load, a
    /// successful durable write's own round-tripped result, or a durable
    /// availability correction. These are always trustworthy regardless of
    /// this session's `ledger_stale`/`rollup_deferred_stale` marking (that
    /// marking is about *rollup* aggregation being behind, never about
    /// `session_json` itself), so this unconditionally clears any stale
    /// full-content fallback entry rather than consulting
    /// [`Self::needs_full_residency`].
    fn resident_from_ledger(&self, session: &Session) -> Arc<ResidentSession> {
        self.full_session_fallback
            .remove(&session.effective_storage_id());
        Arc::new(ResidentSession::of(session))
    }

    /// Builds the resident summary for a session that just came from a live
    /// parse (watcher append or bulk scan) and may or may not have been
    /// durably persisted successfully yet. When
    /// [`Self::needs_full_residency`] says the ledger cannot currently
    /// vouch for this session, the full content is kept in
    /// `full_session_fallback` alongside the summary so
    /// [`Self::full_session`]/[`Self::full_sessions`] and the
    /// `sessions_in_ranges` in-memory fallback keep working without it —
    /// otherwise any stale fallback entry is cleared, since a subsequent
    /// successful write means the ledger is trustworthy again.
    fn resident_from_live_parse(&self, session: &Session) -> Arc<ResidentSession> {
        let storage_id = session.effective_storage_id();
        if self.needs_full_residency(&storage_id) {
            self.full_session_fallback
                .insert(storage_id, Arc::new(session.clone()));
        } else {
            self.full_session_fallback.remove(&storage_id);
        }
        Arc::new(ResidentSession::of(session))
    }

    /// Resolves one session's full content (issue #139): from the resident
    /// full-content fallback if it is there, otherwise a fresh ledger read.
    /// `Ok(None)` means `session_id` is not a session this process knows
    /// about at all — the pre-#139 meaning of `get_session_details`
    /// returning nothing. Once a session *is* known, this never silently
    /// substitutes an empty/zero-usage `Session` for one it could not load
    /// (#116's honesty property) — a load failure is always `Err`.
    pub fn full_session(&self, session_id: &str) -> Result<Option<Session>, String> {
        if !self.sessions.contains_key(session_id) {
            return Ok(None);
        }
        Ok(self
            .full_sessions(std::slice::from_ref(&session_id.to_string()))?
            .pop()
            .map(|session| (*session).clone()))
    }

    /// Batched variant of [`Self::full_session`], for callers that need full
    /// content for many sessions at once (`sessions_in_ranges`'s in-memory
    /// fallback, `tool_impact`, `correlate_events`, `scan_git_outcomes`,
    /// token-budget quota evaluation). `ids` should already be filtered to
    /// keys the caller knows are current `sessions` entries — an id this
    /// process has never observed is simply not resolvable and is not
    /// treated as an error here (mirrors this method's pre-#139 callers,
    /// which already silently skipped an id absent from `state.sessions`).
    /// What *is* an error: an id that is known but cannot be resolved —
    /// the ledger is not ready, or a load failed — rather than silently
    /// returning fewer sessions than requested and letting an aggregate look
    /// complete when it is not (#116).
    pub fn full_sessions(&self, ids: &[String]) -> Result<Vec<Arc<Session>>, String> {
        let mut out = Vec::with_capacity(ids.len());
        let mut to_load: Vec<String> = Vec::new();
        for id in ids {
            match self.full_session_fallback.get(id) {
                Some(full) => out.push(full.value().clone()),
                None => to_load.push(id.clone()),
            }
        }
        if !to_load.is_empty() {
            let history = self.history_ready().ok_or_else(|| {
                "durable history is not available; full session content cannot be loaded"
                    .to_string()
            })?;
            let loaded = history
                .load_many(&to_load)
                .map_err(|error| format!("could not load session content: {error}"))?;
            if loaded.len() != to_load.len() {
                return Err(format!(
                    "could not load {} of {} requested session(s) from durable history",
                    to_load.len() - loaded.len(),
                    to_load.len()
                ));
            }
            out.extend(loaded.into_iter().map(|stored| Arc::new(stored.session)));
        }
        Ok(out)
    }

    /// Reconciles a freshly parsed source with the durable archive before it
    /// can enter the in-memory projection. This is what makes a moved file
    /// retain its identity and lets duplicate provider IDs be disambiguated.
    pub fn reconcile_observed_session(&self, path: &Path, session: Session) -> ReconciledSession {
        let generation = self
            .history_scan_generation
            .load(std::sync::atomic::Ordering::Acquire);
        self.reconcile_session_at_generation(path, session, generation, false)
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
        let reconciled = self.reconcile_session_at_generation(
            path,
            session,
            history_generation.unwrap_or(0),
            true,
        );
        drop(path_state);
        Some(reconciled)
    }

    /// `bulk` selects [`HistoryStore::observe_bulk`] (deferred rollup
    /// maintenance, issue #132) over [`HistoryStore::observe_with_displaced`]
    /// (immediate per-session rollup maintenance). Only
    /// `reconcile_scanned_session_if_current` — the bulk-scan path — passes
    /// `true`; the live watcher path (`reconcile_observed_session`) touches
    /// at most one session at a time, where per-session rollup maintenance
    /// was never the cost problem.
    fn reconcile_session_at_generation(
        &self,
        path: &Path,
        session: Session,
        generation: i64,
        bulk: bool,
    ) -> ReconciledSession {
        let Some(history) = self.history_ready() else {
            return ReconciledSession {
                session,
                displaced: None,
            };
        };
        if bulk {
            match history.observe_bulk(path, &session, generation) {
                Ok(outcome) => self.apply_bulk_outcome(outcome),
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
        } else {
            match history.observe_with_displaced(path, &session, generation) {
                Ok((stored, displaced)) => {
                    self.ledger_stale.remove(&stored.key);
                    // A non-bulk observe always maintains rollups
                    // immediately, so this session's rollups are correct
                    // regardless of any earlier bulk scan that had not yet
                    // rebuilt them.
                    self.rollup_deferred_stale.remove(&stored.key);
                    let displaced = displaced.map(|stored| {
                        self.quota_points_index
                            .update_session(&stored.key, &stored.session);
                        self.sessions
                            .insert(stored.key, self.resident_from_ledger(&stored.session));
                        self.touch_sessions_generation();
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
    }

    /// Shared tail of a successful [`HistoryStore::observe_bulk`]/
    /// [`HistoryStore::observe_bulk_batch`] item: updates the two staleness
    /// sets and publishes a displaced session exactly as the single-item
    /// bulk branch of [`Self::reconcile_session_at_generation`] always has.
    fn apply_bulk_outcome(
        &self,
        outcome: crate::history_store::BulkObserveOutcome,
    ) -> ReconciledSession {
        self.ledger_stale.remove(&outcome.stored.key);
        if outcome.rollups_deferred {
            self.rollup_deferred_stale
                .insert(outcome.stored.key.clone(), ());
        } else {
            self.rollup_deferred_stale.remove(&outcome.stored.key);
        }
        let displaced = outcome.displaced.map(|stored| {
            self.quota_points_index
                .update_session(&stored.key, &stored.session);
            self.sessions
                .insert(stored.key, self.resident_from_ledger(&stored.session));
            self.touch_sessions_generation();
            stored.session
        });
        ReconciledSession {
            session: outcome.stored.session,
            displaced,
        }
    }

    /// Batched variant of [`Self::reconcile_scanned_session_if_current`]
    /// (issue #132): every item in `items` shares one durable-write
    /// transaction ([`HistoryStore::observe_bulk_batch`]) instead of one
    /// transaction each — the actual fix for the scan-write serialization
    /// this issue names. `scanner::scan_all` is this method's only caller
    /// path (via `commands::spawn_scan`'s batch closure), handing it one
    /// [`crate::scanner::SCAN_WRITE_BATCH_SIZE`]-sized chunk at a time.
    ///
    /// Each item's `session_paths` eligibility (a same-generation watcher
    /// touch or removal tombstone must win) is still checked and reserved
    /// per item, exactly like the single-item path — but only for the
    /// duration of that check, not held across the shared durable write the
    /// way the single-item path holds its one entry across its one write.
    /// Holding many `session_paths` entries open at once around one shared
    /// transaction risks a same-shard DashMap write-lock re-entry — a real
    /// deadlock, not merely a slower path — the moment two of this batch's
    /// distinct paths happen to hash into the same internal shard, which at
    /// a batch size in the tens is the expected case, not an edge case.
    ///
    /// Releasing the reservation before the write opens a window: a
    /// watcher-driven removal for one of this batch's paths can land
    /// between this reservation and the batch's commit, and the batch's
    /// write can durably mark that path `present` after the removal already
    /// marked it `missing`. Unlike an in-place session-content race (which
    /// `history_store::store_snapshot`'s monotonicity check already resolves regardless
    /// of commit order — a stale write to `session_snapshots` simply loses,
    /// whichever transaction commits last), there is no such ordering-
    /// independent guard on `source_locations.present`; a plain last-write-
    /// wins column needs an explicit correction pass instead. That pass is
    /// [`Self::reconcile_removals_racing_batch_commit`], run unconditionally
    /// after the durable write below (batched or, on failure, the per-item
    /// fallback) for every path this call touched: it re-checks
    /// `session_paths` for a same-generation removal and, if one raced,
    /// durably corrects it via the same [`HistoryStore::mark_path_missing`]
    /// a live removal would have taken and republishes the corrected
    /// `Missing` session into `state.sessions` immediately — restoring the
    /// single-item path's guarantee ("a late scan result cannot resurrect a
    /// removed source in either the archive or the live projection")
    /// without holding any lock across the write, and converging
    /// immediately rather than only self-healing at the next full scan. The
    /// single-item watcher-append path (`reconcile_observed_session`) does
    /// not go through this method at all and keeps its existing, stronger
    /// guarantee (it never needed this correction pass to begin with).
    ///
    /// On a durable-write failure for the whole batch (`observe_bulk_batch`
    /// rolls the whole shared transaction back on any single item's error —
    /// see its own doc comment), falls back to writing this batch one
    /// session at a time via the single-item bulk path, preserving today's
    /// per-session failure isolation rather than losing an entire batch to
    /// one malformed session.
    pub fn reconcile_scanned_batch_if_current(
        &self,
        app_generation: u64,
        history_generation: Option<i64>,
        items: Vec<(std::path::PathBuf, Session)>,
    ) -> Vec<(std::path::PathBuf, ReconciledSession)> {
        // Mirrors the single-item path: holding this for the whole call
        // closes the same race with `set_config`'s generation bump.
        let _transition = self.config_transition.lock().unwrap();
        if self.current_scan_generation() != app_generation {
            return Vec::new();
        }

        let mut eligible: Vec<(std::path::PathBuf, Session)> = Vec::with_capacity(items.len());
        for (path, session) in items {
            let key = path_key(&path);
            let path_state = self
                .session_paths
                .entry(key)
                .or_insert_with(|| PathSessionState {
                    generation: app_generation,
                    storage_id: String::new(),
                    watcher_touched: false,
                    removed: false,
                });
            let ineligible = path_state.generation == app_generation
                && (path_state.watcher_touched || path_state.removed);
            drop(path_state);
            if !ineligible {
                eligible.push((path, session));
            }
        }
        if eligible.is_empty() {
            return Vec::new();
        }

        let Some(history) = self.history_ready() else {
            return eligible
                .into_iter()
                .map(|(path, session)| {
                    (
                        path,
                        ReconciledSession {
                            session,
                            displaced: None,
                        },
                    )
                })
                .collect();
        };

        let generation = history_generation.unwrap_or(0);
        let paths: Vec<std::path::PathBuf> = eligible
            .iter()
            .map(|(path, _session)| path.clone())
            .collect();
        let batch: Vec<(&Path, &Session, i64)> = eligible
            .iter()
            .map(|(path, session)| (path.as_path(), session, generation))
            .collect();

        let reconciled = match history.observe_bulk_batch(&batch) {
            Ok(outcomes) => eligible
                .into_iter()
                .zip(outcomes)
                .map(|((path, _session), outcome)| (path, self.apply_bulk_outcome(outcome)))
                .collect(),
            Err(error) => {
                tracing::warn!(
                    "could not persist session history batch of {} session(s) as one \
                     transaction, retrying one at a time: {}",
                    eligible.len(),
                    error
                );
                eligible
                    .into_iter()
                    .map(|(path, session)| {
                        let reconciled =
                            self.reconcile_session_at_generation(&path, session, generation, true);
                        (path, reconciled)
                    })
                    .collect()
            }
        };
        // Runs regardless of which branch above wrote the durable rows:
        // both drop `session_paths` reservations before their write, so
        // both are exposed to the same removal-race window.
        self.reconcile_removals_racing_batch_commit(app_generation, &history, &paths);
        reconciled
    }

    /// Post-commit half of the removal-race correction
    /// [`Self::reconcile_scanned_batch_if_current`] documents on itself: for
    /// every path that call just durably wrote, re-checks `session_paths`
    /// for a same-generation removal tombstone. `mark_source_missing` always
    /// sets `removed` and `watcher_touched` together for a real removal, so
    /// checking `removed` alone is precise — nothing else in this module
    /// sets it. A hit means a watcher-driven removal landed in the window
    /// between this path's eligibility reservation and the batch's commit;
    /// this durably corrects it via the exact `HistoryStore::mark_path_missing`
    /// call a live removal takes, and republishes the corrected `Missing`
    /// session into `state.sessions` so the live projection converges
    /// immediately too, not just the archive.
    ///
    /// Idempotent and safe regardless of ordering against a concurrent
    /// `mark_source_missing`'s own durable write: both agree on
    /// `present = 0`, so whichever actually commits last does not matter,
    /// and calling `mark_path_missing` again when it already ran is a
    /// harmless no-op update. A path with no removal (the overwhelmingly
    /// common case) costs one DashMap lookup and nothing else.
    fn reconcile_removals_racing_batch_commit(
        &self,
        app_generation: u64,
        history: &crate::history_store::HistoryStore,
        paths: &[std::path::PathBuf],
    ) {
        for path in paths {
            let key = path_key(path);
            let raced = self
                .session_paths
                .get(&key)
                .is_some_and(|state| state.generation == app_generation && state.removed);
            if !raced {
                continue;
            }
            match history.mark_path_missing(path) {
                Ok(Some(stored)) => {
                    tracing::debug!(
                        "corrected a durable resurrection for {:?}: a watcher removal raced \
                         its batch write",
                        path
                    );
                    self.quota_points_index
                        .update_session(&stored.key, &stored.session);
                    self.sessions
                        .insert(stored.key, self.resident_from_ledger(&stored.session));
                    self.touch_sessions_generation();
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        "could not correct durable source availability for {:?} after a \
                         removal raced its batch write: {}",
                        path,
                        error
                    );
                }
            }
        }
    }

    /// True when this session's ledger rows cannot be trusted for rollup-
    /// backed aggregation: either its latest persist failed (`ledger_stale`),
    /// or a bulk scan durably wrote its facts but has not yet rebuilt
    /// `rollup_*` for it (`rollup_deferred_stale`, issue #132). Either way
    /// the caller (`commands::sessions_in_ranges`) must compute this session
    /// from in-memory history instead of the ledger's rollup fast path.
    pub fn ledger_is_stale(&self, storage_id: &str) -> bool {
        self.ledger_stale.contains_key(storage_id)
            || self.rollup_deferred_stale.contains_key(storage_id)
    }

    /// Runs `HistoryStore::rebuild_rollups_if_stale` and, if it actually
    /// rebuilt, clears every session this process had marked
    /// `rollup_deferred_stale` — a full rebuild is global, so it resolves
    /// every session's deferred rollup regardless of which bulk-scan
    /// generation deferred it. The only caller is `commands::spawn_scan`,
    /// once per completed (non-superseded) scan, unconditionally — whether
    /// or not the scan reported parse failures, since a deferred rollup is
    /// orthogonal to whether source availability could be finalized.
    /// Returns whether a rebuild actually ran, for the caller's metric
    /// (issue #140): "nothing was deferred" and "rebuilt" both take this
    /// path, and only a recording can otherwise tell them apart.
    pub fn finalize_bulk_scan_rollups(&self) -> bool {
        let Some(history) = self.history_ready() else {
            return false;
        };
        match history.rebuild_rollups_if_stale() {
            Ok(true) => {
                self.rollup_deferred_stale.clear();
                true
            }
            Ok(false) => false,
            Err(error) => {
                tracing::warn!(
                    "could not rebuild durable history rollups after scan: {}",
                    error
                );
                false
            }
        }
    }

    /// Completes an error-free current scan: marks locations `finish_scan`
    /// found unseen as missing, then refreshes only the in-memory sessions
    /// that transition affected (issue #132/#139/#140).
    ///
    /// This used to call the same wholesale [`Self::hydrate_history`] the
    /// true startup path uses — re-deserializing every archived session's
    /// snapshot just to notice the handful `finish_scan`'s `UPDATE` actually
    /// touched. On the corpus a v0.8.8 field recording measured, that made
    /// this call's cost comparable to `startup.history_hydrate`'s ~33s, and
    /// it ran on *every* bulk scan, not just at startup — yet it had no
    /// metric of its own, so it was invisible inside `startup.bulk_scan`'s
    /// unaccounted residual (issue #140). `HistoryStore::finish_scan` now
    /// reports exactly which session keys it changed, and each is reloaded
    /// with [`HistoryStore::load_one`] — the same per-session query
    /// `Self::apply_loaded_session`'s other caller (`Self::hydrate_history`)
    /// already used — instead of the whole corpus.
    pub fn finish_history_scan(&self, generation: i64) -> Vec<Session> {
        let Some(history) = self.history_ready() else {
            return Vec::new();
        };
        let affected_keys = match history.finish_scan(generation) {
            Ok(keys) => keys,
            Err(error) => {
                tracing::warn!("could not finish durable history scan: {}", error);
                return Vec::new();
            }
        };
        let mut changed = Vec::with_capacity(affected_keys.len());
        for key in &affected_keys {
            match history.load_one(key) {
                Ok(stored) => {
                    if let Some(session) = self.apply_loaded_session(stored) {
                        changed.push(session);
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "could not reload session {} after finishing scan: {}",
                        key,
                        error
                    );
                }
            }
        }
        changed
    }

    /// Applies one freshly loaded archived session to the in-memory
    /// projection, exactly like a single iteration of [`Self::hydrate_history`]'s
    /// loop. Shared with [`Self::finish_history_scan`] so both call sites
    /// publish and detect changes identically regardless of whether they
    /// loaded the whole corpus or one session.
    fn apply_loaded_session(&self, stored: crate::history_store::StoredSession) -> Option<Session> {
        let session = stored.session;
        let key = stored.key;
        // Scan callbacks already emit fresh present snapshots. Here we only
        // need to announce availability/path transitions caused by archive
        // reconciliation (most notably a missing source).
        let is_changed = self.sessions.get(&key).is_none_or(|existing| {
            existing.summary.source_availability != session.source_availability
                || existing.summary.file_path != session.file_path
        });
        self.quota_points_index.update_session(&key, &session);
        self.sessions
            .insert(key, self.resident_from_ledger(&session));
        self.touch_sessions_generation();
        is_changed.then_some(session)
    }

    /// Loads every archived session into the in-memory projection. The store
    /// is authoritative for availability; this is the true startup hydration
    /// path — [`Self::finish_history_scan`] deliberately does not call this
    /// anymore; see its doc comment.
    ///
    /// Streams via [`HistoryStore::stream_sessions`] rather than collecting
    /// a `Vec<StoredSession>` first (field regression on #139: what stayed
    /// *resident* after hydration shrank, but what got *allocated* during it
    /// did not — the old `Vec<StoredSession>` still peaked at the whole
    /// corpus's full content, deserialized, live at once, before this loop
    /// could drop any of it one at a time). Each `StoredSession` is applied
    /// and dropped before the next one is even read off the connection, so
    /// peak allocation here is O(one session) rather than O(corpus); this is
    /// still a streaming change, not a semantic one — `state.sessions` and
    /// `ledger_stale` end up populated exactly as before.
    pub fn hydrate_history(&self) -> crate::history_store::HydrationStats {
        let Some(history) = self.history_ready() else {
            return crate::history_store::HydrationStats::default();
        };
        // Dirty markings survive restarts in the store itself; rebuild the
        // in-process stale set so ledger-backed aggregation keeps routing
        // these sessions through in-memory history. Populated before the
        // session stream below, same relative order as when this collected
        // a `Vec` first and applied it afterward.
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
        match history.stream_sessions(|stored| {
            self.apply_loaded_session(stored);
        }) {
            Ok(stats) => stats,
            Err(error) => {
                tracing::warn!("could not load durable session history: {}", error);
                crate::history_store::HydrationStats::default()
            }
        }
    }

    /// Persists a metadata-only in-memory overlay (a caller-supplied full
    /// `Session`) without changing source ownership. General-purpose: unlike
    /// [`Self::persist_thread_name_overlay_batch`], this accepts arbitrary
    /// caller-side content and so still needs [`HistoryStore::update_snapshot`]'s
    /// divergence detection against the ledger's own facts. The session-index
    /// thread-name overlay — this method's original and, as of the field
    /// regression on issues #139/#141, only in-tree caller — has moved to
    /// `persist_thread_name_overlay_batch` instead, since it only ever
    /// changes one field and never needs full session content at all. Kept
    /// as the general primitive for a future overlay that does need to
    /// write arbitrary caller-supplied content.
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

    /// Batched variant of [`Self::persist_session_metadata`] (issue #141):
    /// every session in `sessions` shares one durable-write transaction
    /// ([`HistoryStore::update_snapshot_batch`]) instead of one transaction
    /// per session — the same shape [`Self::reconcile_scanned_batch_if_current`]
    /// uses for the scan's own write path. On a whole-batch failure, falls
    /// back to writing each session individually via
    /// [`Self::persist_session_metadata`], preserving today's per-session
    /// failure isolation rather than silently dropping the rest of the
    /// batch's metadata updates. See `persist_session_metadata`'s doc
    /// comment: also general-purpose, also with no in-tree caller as of the
    /// #139/#141 field regression's fix, for the same reason.
    pub fn persist_session_metadata_batch(&self, sessions: &[Session]) {
        if sessions.is_empty() {
            return;
        }
        let Some(history) = self.history_ready() else {
            return;
        };
        if let Err(error) = history.update_snapshot_batch(sessions) {
            tracing::warn!(
                "could not persist metadata-only session snapshot batch of {} session(s) as \
                 one transaction, retrying one at a time: {}",
                sessions.len(),
                error
            );
            for session in sessions {
                self.persist_session_metadata(session);
            }
        }
    }

    /// Targeted metadata-only durable write for the session-index overlay
    /// (issue #141 field regression): `updates` is `(storage key, new
    /// thread_name)` pairs read straight off the just-patched
    /// `ResidentSession` summaries. Unlike [`Self::persist_session_metadata_batch`],
    /// this never needs this pass's full session content — no
    /// `AppState::full_sessions` ledger read of turns/token histories/tool
    /// observations happens on this path at all; see
    /// [`crate::history_store::HistoryStore::overlay_thread_names`]'s doc
    /// comment for the investigation behind that. Falls back to one-at-a-
    /// time retries on a whole-batch failure, mirroring
    /// `persist_session_metadata_batch`.
    pub fn persist_thread_name_overlay_batch(&self, updates: &[(String, Option<String>)]) {
        if updates.is_empty() {
            return;
        }
        let Some(history) = self.history_ready() else {
            return;
        };
        if let Err(error) = history.overlay_thread_names(updates) {
            tracing::warn!(
                "could not persist thread-name overlay batch of {} session(s) as one \
                 transaction, retrying one at a time: {}",
                updates.len(),
                error
            );
            for update in updates {
                if let Err(error) = history.overlay_thread_names(std::slice::from_ref(update)) {
                    tracing::warn!(
                        "could not persist thread-name overlay for {}: {}",
                        update.0,
                        error
                    );
                }
            }
        }
    }

    /// Converts a physical source deletion into a retained, availability-
    /// marked session. It never removes the logical session from memory.
    /// Returns the resident summary (issue #139) rather than a full
    /// `Session` — every caller only ever needed it to build the
    /// `session-updated` event payload, which is summary-shaped already.
    pub fn mark_source_missing(&self, path: &Path) -> Option<SessionSummary> {
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
            self.quota_points_index
                .update_session(&stored.key, &stored.session);
            let summary = SessionSummary::of(&stored.session);
            self.sessions
                .insert(stored.key, self.resident_from_ledger(&stored.session));
            self.touch_sessions_generation();
            return Some(summary);
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
                .map(|resident| resident.summary.clone());
        }
        let mut resident = self.sessions.get_mut(&storage_id)?;
        let resident = std::sync::Arc::make_mut(resident.value_mut());
        resident.summary.source_availability = SourceAvailability::Missing;
        // A companion full-content fallback entry (kept only when the
        // ledger could not vouch for this session) must stay consistent
        // too, since `Self::full_session`/`full_sessions` hand its
        // `source_availability` straight to callers.
        if let Some(mut full) = self.full_session_fallback.get_mut(&storage_id) {
            std::sync::Arc::make_mut(&mut full).source_availability = SourceAvailability::Missing;
        }
        // Deliberately does not call `quota_points_index.update_session`
        // here: the only field this branch changes is
        // `source_availability`, which `QuotaPointsIndex` never reads (it
        // only folds in `rate_limits_history`/`credits_*`/`harness`/
        // `last_event_at`, none of which moved), so retracting and
        // re-adding this session's points would be a same-result, wasted
        // O(points) pass rather than a correctness requirement — and doing
        // it would need a full `Session` this branch no longer has on hand.
        self.touch_sessions_generation();
        Some(resident.summary.clone())
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
        self.quota_points_index
            .update_session(&storage_id, &session);
        self.sessions
            .insert(storage_id, self.resident_from_live_parse(&session));
        self.touch_sessions_generation();
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
        self.quota_points_index
            .update_session(&storage_id, &session);
        self.sessions
            .insert(storage_id, self.resident_from_live_parse(&session));
        self.touch_sessions_generation();
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

    /// Marks that `sessions` changed. Every call site that inserts into or
    /// mutates an entry in `sessions` must call this (issue #128) so
    /// [`Self::quota_snapshots`]'s cache knows a recompute is warranted.
    /// Over-invalidating (e.g. on a metadata-only change quota doesn't
    /// care about) is harmless — it only means the next
    /// `get_quota_snapshots` call is eligible to recompute, gated by
    /// [`crate::quota::QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL`] either way.
    fn touch_sessions_generation(&self) {
        self.sessions_generation
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// The current session-change generation; see
    /// [`Self::touch_sessions_generation`].
    pub fn sessions_generation(&self) -> u64 {
        self.sessions_generation
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// The cached quota-config file, loading it from disk on first access.
    /// Only [`Self::set_quota_store`] and [`Self::persist_quota_notification_log`]
    /// replace it thereafter (issues #128, #131).
    pub fn quota_store(&self) -> Arc<crate::quota_store::QuotaStoreFile> {
        let mut cache = self.quota_store_cache.lock().unwrap();
        if let Some(store) = cache.as_ref() {
            return store.clone();
        }
        let store = Arc::new(crate::quota_store::QuotaStoreFile::load());
        *cache = Some(store.clone());
        store
    }

    /// Replaces the cached quota-config file and invalidates the quota
    /// snapshot cache, since `max_cache_age_secs` feeds it directly. The
    /// only caller is `commands::set_quota_config`, immediately after it
    /// persists a new file to disk (issue #128).
    pub fn set_quota_store(&self, store: crate::quota_store::QuotaStoreFile) {
        *self.quota_store_cache.lock().unwrap() = Some(Arc::new(store));
        self.quota_snapshot_cache.invalidate();
    }

    /// Prunes and persists an updated notification dedup log, keeping the
    /// cached quota-config file in sync — deliberately *not* through
    /// [`Self::set_quota_store`], because that also invalidates the quota
    /// snapshot cache. The notification log never feeds
    /// `QuotaPointsIndex`/`quota_snapshots_from_sessions` (only
    /// `max_cache_age_secs` does, and this method never touches that
    /// field), so forcing a recompute here would have nothing new to pick
    /// up. The only caller is `commands::check_quota_alerts` (issue #131):
    /// routing its write through the shared cached store here, instead of
    /// loading and saving its own copy of `quota-v1.json` on every poll, is
    /// what lets that command reuse [`Self::quota_snapshots`] instead of
    /// re-walking the corpus on its own independent polling interval.
    ///
    /// Fails closed like [`commands::set_quota_config`]'s write path: the
    /// in-memory cache is only replaced once the write to disk succeeds, so
    /// a failed save never leaves the cache claiming a log state that was
    /// never actually persisted.
    pub fn persist_quota_notification_log(
        &self,
        notification_log: Vec<crate::quota_store::NotificationLogEntry>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let mut cache = self.quota_store_cache.lock().unwrap();
        let mut store = match cache.as_ref() {
            Some(existing) => (**existing).clone(),
            None => crate::quota_store::QuotaStoreFile::load(),
        };
        store.notification_log = notification_log;
        store.prune_log(now, chrono::Duration::days(30));
        store.save()?;
        *cache = Some(Arc::new(store));
        Ok(())
    }

    /// Returns every provider's quota snapshot, recomputing from the
    /// incrementally maintained points index only when warranted; see
    /// [`crate::quota::QuotaSnapshotCache`] (issue #128) for the recompute
    /// gate and [`crate::quota::QuotaPointsIndex`] (issue #131) for why this
    /// recompute no longer walks `sessions`.
    pub fn quota_snapshots(
        &self,
        max_cache_age: chrono::Duration,
        now: DateTime<Utc>,
    ) -> Vec<crate::quota::QuotaSnapshot> {
        let generation = self.sessions_generation();
        self.quota_snapshot_cache
            .get_or_recompute(generation, std::time::Instant::now(), || {
                self.quota_points_index.snapshots(now, max_cache_age)
            })
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
            full_session_fallback: DashMap::new(),
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
            rollup_deferred_stale: DashMap::new(),
            external_events: Mutex::new(ExternalEventStore::new(Vec::new())),
            performance: crate::performance::PerformanceRecorder::default(),
            tray: Mutex::new(None),
            tray_available: AtomicBool::new(false),
            last_scan_report: Mutex::new(None),
            sessions_generation: AtomicU64::new(0),
            quota_store_cache: Mutex::new(None),
            quota_snapshot_cache: crate::quota::QuotaSnapshotCache::new(),
            quota_points_index: crate::quota::QuotaPointsIndex::new(),
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
        assert_eq!(
            state
                .sessions
                .get("codex:thread:a")
                .unwrap()
                .summary
                .total_turns,
            2
        );
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
                .summary
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
        // Deliberately `AppState::new()`, not the `state()` fixture below:
        // `state()` hardcodes `Unavailable` so the many unrelated tests that
        // use it get a deterministic "no archive, degrade gracefully"
        // fixture without needing a real store. This test exists
        // specifically to prove what the *real* constructor produces (#116:
        // construction must never resolve the archive path itself — only
        // `set_history_ready`, called from a background thread, may decide
        // `Ready`/`Unavailable`), so it has to go through `new()` itself
        // rather than a fixture that already encodes an answer.
        let state = AppState::new();
        assert_eq!(state.history_readiness(), HistoryReadinessKind::Pending);
        assert!(state.history_ready().is_none());

        let directory = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
        state.set_history_ready(Some(Arc::new(store)));

        assert_eq!(state.history_readiness(), HistoryReadinessKind::Ready);
        assert!(state.history_ready().is_some());
    }

    #[test]
    fn reconcile_scanned_batch_if_current_persists_every_item_in_one_transaction() {
        // Issue #132: the batch write path. Every item that passes the
        // eligibility check must come back reconciled, and — since this is
        // the bulk-scan path — routed to in-memory aggregation
        // (`ledger_is_stale`) until the scan's rollup rebuild runs.
        let state = AppState::new();
        let directory = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
        state.set_history_ready(Some(Arc::new(store)));

        let generation = state.current_scan_generation();
        let items = vec![
            (PathBuf::from("C:/sessions/a.jsonl"), session("a", 1)),
            (PathBuf::from("C:/sessions/b.jsonl"), session("b", 2)),
        ];
        let results = state.reconcile_scanned_batch_if_current(generation, Some(1), items);
        assert_eq!(results.len(), 2);
        for (_path, reconciled) in &results {
            assert!(
                state.ledger_is_stale(&reconciled.session.storage_id),
                "a freshly bulk-written session must be ledger-stale (rollup-deferred) \
                 until the scan rebuilds rollups"
            );
        }
    }

    #[test]
    fn reconcile_scanned_batch_if_current_corrects_a_removal_that_raced_the_batch_commit() {
        // Issue #132: `reconcile_scanned_batch_if_current`'s doc comment
        // describes a real race — a watcher removal landing between a
        // path's eligibility reservation and the batch's commit — and the
        // post-commit correction pass (`reconcile_removals_racing_batch_commit`)
        // that closes it. A unit test cannot inject a genuinely concurrent
        // watcher thread mid-transaction, so this drives the same two
        // halves in sequence: write the batch (establishing a durable
        // `Present` row), simulate the watcher's tombstone exactly as
        // `mark_source_missing` sets it, then invoke the same correction
        // pass the batch path always runs and assert it restores `Missing`
        // both durably and in `state.sessions`.
        let state = AppState::new();
        let directory = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
        state.set_history_ready(Some(Arc::new(store)));

        let generation = state.current_scan_generation();
        let path = PathBuf::from("C:/sessions/a.jsonl");
        let items = vec![(path.clone(), session("a", 1))];
        let results = state.reconcile_scanned_batch_if_current(generation, Some(1), items);
        assert_eq!(results.len(), 1);
        assert_eq!(
            results[0].1.session.source_availability,
            SourceAvailability::Present
        );
        let key_for_lookup = results[0].1.session.storage_id.clone();

        // Simulate the watcher's tombstone landing in the race window —
        // the same mutation `mark_source_missing` performs to
        // `session_paths`, without separately duplicating its own durable
        // write here (that write is exactly what this test proves the
        // correction pass substitutes for).
        let tombstone_key = path_key(&path);
        state
            .session_paths
            .entry(tombstone_key)
            .and_modify(|entry| {
                entry.generation = generation;
                entry.watcher_touched = true;
                entry.removed = true;
            });

        let history = state.history_ready().unwrap();
        state.reconcile_removals_racing_batch_commit(
            generation,
            &history,
            std::slice::from_ref(&path),
        );

        assert!(
            !state
                .sessions
                .get(&key_for_lookup)
                .is_some_and(
                    |entry| entry.summary.source_availability == SourceAvailability::Present
                ),
            "the corrected session must not still read Present in the live projection"
        );
        let corrected = history
            .load_sessions()
            .unwrap()
            .into_iter()
            .find(|stored| stored.session.id == "a")
            .expect("the durable session must still be archived, just marked missing");
        assert_eq!(
            corrected.session.source_availability,
            SourceAvailability::Missing
        );
    }

    #[test]
    fn reconcile_scanned_batch_if_current_drops_everything_for_a_stale_generation() {
        let state = AppState::new();
        let stale = state.current_scan_generation();
        state.advance_scan_generation();
        let items = vec![(PathBuf::from("C:/sessions/a.jsonl"), session("a", 1))];
        let results = state.reconcile_scanned_batch_if_current(stale, None, items);
        assert!(results.is_empty());
    }

    #[test]
    fn reconcile_scanned_batch_if_current_skips_an_item_the_watcher_already_touched() {
        // Mirrors `watcher_touch_prevents_older_bulk_scan_overwrite`'s intent
        // for the batch path: a same-generation watcher touch wins over a
        // batched scan result for the same path, exactly like the
        // single-item path.
        let state = state();
        let path = PathBuf::from("C:/sessions/a.jsonl");
        state.publish_watched_session(&path, session("a", 5));
        let generation = state.current_scan_generation();
        let items = vec![(path, session("a", 1))];
        let results = state.reconcile_scanned_batch_if_current(generation, None, items);
        assert!(results.is_empty());
    }

    #[test]
    fn ledger_is_stale_reflects_a_deferred_rollup_until_the_scan_finalizes_it() {
        // Issue #132's crash-safety-adjacent correctness property: between a
        // bulk write and the scan's rollup rebuild, `sessions_in_ranges`
        // must not serve a rollup-backed answer for a session whose rollups
        // are known to be behind its facts — `ledger_is_stale` is what
        // routes it to the always-accurate in-memory fallback instead.
        // `finalize_bulk_scan_rollups` (the scan-completion call) must clear
        // that once the rebuild actually runs.
        let state = AppState::new();
        let directory = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
        state.set_history_ready(Some(Arc::new(store)));

        let generation = state.current_scan_generation();
        let items = vec![(PathBuf::from("C:/sessions/a.jsonl"), session("a", 1))];
        let results = state.reconcile_scanned_batch_if_current(generation, Some(1), items);
        assert_eq!(results.len(), 1);
        let key = results[0].1.session.storage_id.clone();
        assert!(state.ledger_is_stale(&key));

        state.finalize_bulk_scan_rollups();
        assert!(!state.ledger_is_stale(&key));
    }

    #[test]
    #[ignore = "performance probe; run with --release --ignored --nocapture"]
    fn probe_reconcile_scanned_batch_vs_single_item_includes_config_transition() {
        // Issue #132 follow-up: `history_store::tests::probe_bulk_scan_write_serialization_before_and_after`
        // isolates `HistoryStore`'s write path alone (measured there:
        // 1.70x) and does not include `config_transition`'s collapse from
        // one process-wide mutex acquisition per session
        // (`reconcile_scanned_session_if_current`) to one per batch
        // (`reconcile_scanned_batch_if_current`) — both already defer
        // rollups identically (`bulk = true` either way), so the only
        // orchestration-layer difference this isolates is that lock-
        // acquisition count, plus the shape of the per-item
        // `session_paths` DashMap bookkeeping around it. Combine this
        // number with the `HistoryStore`-level probe's for the full
        // picture; neither one alone is "the" #132 number.
        const SESSIONS: usize = 4_000;
        const WORKERS: usize = 8;
        const BATCH_SIZE: usize = 64;

        let run = |label: &str, batched: bool| -> std::time::Duration {
            let state = Arc::new(AppState::new());
            let directory = tempfile::tempdir().unwrap();
            let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
            state.set_history_ready(Some(Arc::new(store)));
            let generation = state.current_scan_generation();
            let chunk = SESSIONS.div_ceil(WORKERS);

            let started = std::time::Instant::now();
            std::thread::scope(|scope| {
                for worker in 0..WORKERS {
                    let state = state.clone();
                    let start = worker * chunk;
                    let end = ((worker + 1) * chunk).min(SESSIONS);
                    scope.spawn(move || {
                        if batched {
                            let mut index = start;
                            while index < end {
                                let batch_end = (index + BATCH_SIZE).min(end);
                                let items: Vec<(PathBuf, Session)> = (index..batch_end)
                                    .map(|n| {
                                        (
                                            PathBuf::from(format!("C:/sessions/probe-{n}.jsonl")),
                                            session(&format!("probe-{n}"), 5),
                                        )
                                    })
                                    .collect();
                                state.reconcile_scanned_batch_if_current(
                                    generation,
                                    Some(1),
                                    items,
                                );
                                index = batch_end;
                            }
                        } else {
                            for n in start..end {
                                let path = PathBuf::from(format!("C:/sessions/probe-{n}.jsonl"));
                                let fixture = session(&format!("probe-{n}"), 5);
                                state.reconcile_scanned_session_if_current(
                                    generation,
                                    &path,
                                    fixture,
                                    Some(1),
                                );
                            }
                        }
                    });
                }
            });
            let elapsed = started.elapsed();
            eprintln!("{label}: {SESSIONS} sessions / {WORKERS} workers in {elapsed:?}");
            elapsed
        };

        let single_item = run(
            "single-item (reconcile_scanned_session_if_current: one config_transition \
             acquisition per session)",
            false,
        );
        let batched = run(
            "batched (reconcile_scanned_batch_if_current: one config_transition \
             acquisition per 64-session batch)",
            true,
        );

        eprintln!(
            "probe summary: single_item={single_item:?} batched={batched:?} speedup={:.2}x",
            single_item.as_secs_f64() / batched.as_secs_f64().max(0.000_001)
        );
    }

    #[test]
    fn history_readiness_resolves_to_unavailable_and_still_degrades_gracefully() {
        // Pending must never be treated like Unavailable by a caller that
        // only checks `history_ready()` while it is transient — but once
        // resolved, Unavailable itself must behave exactly like the
        // pre-#116 "archive never opened" path: observing a session must
        // not panic or block, and the session stays usable in memory.
        // `AppState::new()` so this actually exercises the Pending ->
        // Unavailable transition, not just an already-Unavailable fixture.
        let state = AppState::new();
        assert_eq!(state.history_readiness(), HistoryReadinessKind::Pending);
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
        // Same reasoning as above: `AppState::new()`, not `state()`, so the
        // initial `Pending` this asserts is the real constructor's behavior.
        let state = Arc::new(AppState::new());
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

    // -- check_quota_alerts no longer re-walks the corpus (issue #131,
    //    round two) ------------------------------------------------------

    #[test]
    fn check_quota_alerts_with_a_configured_budget_reads_the_points_index_not_the_corpus() {
        // `check_quota_alerts` used to build `Vec<QuotaSnapshot>` by walking
        // every session directly and independently of
        // `get_quota_snapshots`'s cache/index — invisible in a recording
        // taken with no budgets configured, live the moment a budget is.
        //
        // This is the "counting stand-in" equivalent for a function that
        // does not take an injectable compute closure: insert sessions
        // carrying real rate-limit data straight into `state.sessions`,
        // deliberately bypassing `publish_watched_session` (and therefore
        // `QuotaPointsIndex::update_session`) the way every real call site
        // in this file no longer does. The old, buggy implementation read
        // `state.sessions` directly and would have picked this data up
        // regardless. The current one reads only
        // `AppState::quota_snapshots`, which is backed by the points index
        // — so a budget evaluated against data that only ever landed in
        // `state.sessions` must see nothing, proving the corpus itself was
        // never walked.
        let state = state();
        let now = Utc::now();
        for i in 0..50 {
            let mut session = session(&format!("s{i}"), 1);
            session.last_event_at = now;
            session.rate_limits_history = vec![crate::model::RateLimitSnapshotPoint {
                timestamp: now,
                turn_id: None,
                limit_id: None,
                primary: Some(crate::model::RateLimitWindow {
                    used_percent: 90.0,
                    window_minutes: Some(300),
                    resets_at: Some(now + chrono::Duration::hours(5)),
                }),
                secondary: None,
            }];
            state.sessions.insert(
                session.effective_storage_id(),
                Arc::new(crate::model::ResidentSession::of(&session)),
            );
        }

        let mut store = crate::quota_store::QuotaStoreFile::default();
        store.budgets.push(crate::quota_store::QuotaBudget {
            id: "b1".to_string(),
            provider: codex_provider_id(),
            project_key: None,
            unit: crate::quota_store::BudgetUnit::PercentOfWindow,
            window_kind: Some("burst".to_string()),
            period_hours: None,
            threshold: 50.0,
            enabled: true,
        });
        store.notifications.enabled = true;
        state.set_quota_store(store);

        let alerts = crate::commands::check_quota_alerts_impl(&state);
        assert!(
            alerts.is_empty(),
            "an alert here would mean check_quota_alerts saw the 90%-used data that only ever \
             landed in state.sessions, i.e. it walked the corpus directly again instead of \
             reading the points index"
        );
    }
}
