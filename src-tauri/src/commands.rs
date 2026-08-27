#[cfg(windows)]
use crate::config::DEFENDER_EXCLUSION_RECEIPT_VERSION;
use crate::config::{Config, DefenderExclusionReceipt};
use crate::model::{
    Harness, RangeTotals, RateLimitSnapshotPoint, RateLimitWindow, Session, SessionSummary,
};
use crate::rates::RateCard;
use crate::scan_cache;
use crate::store::{
    AppState, HistoryReadinessKind, HistoryRebuildPhase, HistoryRebuildSnapshot,
    HistoryStepSnapshot,
};
#[cfg(any(windows, test))]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Local, Timelike, Utc};
#[cfg(any(windows, test))]
use std::collections::HashSet;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_dialog::DialogExt;

#[tauri::command]
pub fn list_external_events(
    state: State<'_, Arc<AppState>>,
) -> Vec<crate::correlation::ExternalEvent> {
    state.external_events_snapshot()
}

#[tauri::command]
pub async fn list_instruction_files(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::instructions::InstructionInventory, String> {
    let started = Instant::now();
    let app_state = state.inner().clone();
    // Racy-but-benign read for the cache decision only; the authoritative
    // enabled check happens inside the scan's config-transition snapshot.
    let instructions_enabled = Config::load()
        .map(|config| config.instructions_enabled)
        .unwrap_or(false);
    if instructions_enabled {
        if let Some(mut cached) = crate::instructions::load_persisted_inventory() {
            // Stale-while-revalidate: answer from the persisted inventory
            // immediately and rescan in the background. The fresh result is
            // persisted and delivered via `instruction-inventory-updated`.
            cached.stale = true;
            let cached_files = cached.files.len();
            let rescan_state = app_state.clone();
            tauri::async_runtime::spawn(async move {
                let rescan_started = Instant::now();
                match run_instruction_scan(app.clone(), rescan_state.clone()).await {
                    Ok(inventory) => {
                        rescan_state.performance.record_backend(
                            "instructions.background_rescan",
                            rescan_started,
                            true,
                            BTreeMap::from([
                                ("files".into(), inventory.files.len().to_string()),
                                ("entries".into(), inventory.entries_visited.to_string()),
                            ]),
                        );
                        let _ = app.emit("instruction-inventory-updated", &inventory);
                    }
                    Err(error) => {
                        rescan_state.performance.record_backend(
                            "instructions.background_rescan",
                            rescan_started,
                            false,
                            BTreeMap::new(),
                        );
                        if !error.contains(crate::instructions::SCAN_CANCELLED_ERROR) {
                            tracing::warn!("background instruction rescan failed: {}", error);
                            // Without a terminal signal the view would show
                            // "refreshing in background" forever.
                            let _ = app.emit("instruction-inventory-error", &error);
                        }
                    }
                }
            });
            app_state.performance.record_backend(
                "ipc.list_instruction_files",
                started,
                true,
                BTreeMap::from([
                    ("files".into(), cached_files.to_string()),
                    ("cache".into(), "hit".into()),
                ]),
            );
            return Ok(cached);
        }
    }
    let result = run_instruction_scan(app, app_state.clone()).await;
    app_state.performance.record_backend(
        "ipc.list_instruction_files",
        started,
        result.is_ok(),
        BTreeMap::from([
            (
                "files".into(),
                result
                    .as_ref()
                    .map(|inventory| inventory.files.len())
                    .unwrap_or(0)
                    .to_string(),
            ),
            ("cache".into(), "miss".into()),
        ]),
    );
    result
}

async fn run_instruction_scan(
    app: AppHandle,
    app_state: Arc<AppState>,
) -> Result<crate::instructions::InstructionInventory, String> {
    let (config, scan_id) = {
        // Capture the durable roots and allocate their scan generation in the
        // same config transition. A settings save can therefore only happen
        // wholly before or wholly after this snapshot.
        let _transition = app_state.config_transition.lock().unwrap();
        let config = Config::load().map_err(|error| error.to_string())?;
        let scan_id = app_state.begin_instruction_scan();
        (config, scan_id)
    };
    let instructions_enabled = config.instructions_enabled;
    let sessions = app_state
        .sessions
        .iter()
        .filter_map(|entry| {
            let session = &entry.value().summary;
            session.working_directory.as_ref().map(|working_directory| {
                crate::instructions::InstructionSessionContext {
                    working_directory: std::path::PathBuf::from(working_directory),
                    last_event_at: session.last_event_at,
                }
            })
        })
        .collect::<Vec<_>>();
    let scan_state = app_state.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::instructions::discover_with_progress(
            &config,
            &sessions,
            scan_id,
            |progress| {
                let _ = app.emit("instruction-scan-progress", progress);
            },
            || !scan_state.instruction_scan_is_current(scan_id),
        )
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string());
    if let Ok(inventory) = &result {
        let paths = inventory
            .files
            .iter()
            .map(|file| crate::instructions::normalized_path_key(std::path::Path::new(&file.path)))
            .collect::<Vec<_>>();
        let _transition = app_state.config_transition.lock().unwrap();
        // A superseded scan must not persist either: a settings transition or
        // newer scan owns the durable state from here on.
        if app_state.publish_instruction_paths_if_current(scan_id, paths) {
            if instructions_enabled {
                if let Err(error) = crate::instructions::persist_inventory(inventory) {
                    tracing::warn!("could not persist instruction inventory: {}", error);
                }
            } else {
                // The feature is off: a persisted index must not outlive that choice.
                crate::instructions::remove_persisted_inventory();
            }
        }
    }
    result
}

#[tauri::command]
pub fn cancel_instruction_scan(state: State<'_, Arc<AppState>>) -> u64 {
    state.cancel_instruction_scan()
}

fn validate_instruction_access(state: &AppState, path: &std::path::Path) -> Result<(), String> {
    // Keep the enabled flag and the discovered-path allowlist coherent with
    // any settings transition that revokes instruction roots.
    let _transition = state.config_transition.lock().unwrap();
    let config = Config::load().map_err(|error| error.to_string())?;
    if !config.instructions_enabled {
        return Err("instruction inventory is disabled".into());
    }
    if !state.instruction_path_allowed(path) {
        return Err("refresh the instruction inventory before opening this file".into());
    }
    crate::instructions::validate_instruction_path(path).map_err(|error| error.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn read_instruction_file(
    state: State<'_, Arc<AppState>>,
    path: String,
) -> Result<crate::instructions::InstructionContent, String> {
    let path = std::path::PathBuf::from(path);
    validate_instruction_access(&state, &path)?;
    crate::instructions::read_content(&path).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn open_instruction_file(state: State<'_, Arc<AppState>>, path: String) -> Result<(), String> {
    let path = std::path::PathBuf::from(path);
    validate_instruction_access(&state, &path)?;

    #[cfg(target_os = "linux")]
    let command = std::process::Command::new("xdg-open").arg(&path).spawn();

    #[cfg(target_os = "macos")]
    let command = std::process::Command::new("open").arg(&path).spawn();

    #[cfg(target_os = "windows")]
    let command = std::process::Command::new("explorer").arg(&path).spawn();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let command: Result<(), std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));

    command.map(|_| ()).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn get_performance_status(
    state: State<'_, Arc<AppState>>,
) -> crate::performance::PerformanceStatus {
    state.performance.status()
}

/// Live view of the current process, active phase, and recent phase timings
/// (issue #163) — `DiagnosticsPanel`'s data source. `enabled: false` (via
/// `crate::memory::MemoryLiveStatus::default()`, with `recent_operations`
/// left empty here to match) is the entire disabled-tracking answer; the
/// frontend renders that as "tracking is off", never as zeros or an empty
/// chart standing in for "measured, and it's fine".
#[derive(Debug, Clone, serde::Serialize)]
pub struct PerformanceLiveStatus {
    #[serde(flatten)]
    pub memory: crate::memory::MemoryLiveStatus,
    pub recent_operations: Vec<crate::performance::RecentOperation>,
}

#[tauri::command]
pub fn get_performance_live_status(state: State<'_, Arc<AppState>>) -> PerformanceLiveStatus {
    let memory = crate::memory::live_status(&state.performance);
    let recent_operations = if memory.enabled {
        state.performance.recent_operations()
    } else {
        Vec::new()
    };
    PerformanceLiveStatus {
        memory,
        recent_operations,
    }
}

#[tauri::command]
pub fn record_frontend_performance(
    state: State<'_, Arc<AppState>>,
    operation: String,
    duration_ms: f64,
    success: bool,
    metadata: std::collections::BTreeMap<String, String>,
) -> Result<(), String> {
    state
        .performance
        .record_frontend(operation, duration_ms, success, metadata)
}

#[tauri::command]
pub async fn export_performance_data(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    format: String,
) -> Result<bool, String> {
    let extension = match format.as_str() {
        "jsonl" => "jsonl",
        "csv" => "csv",
        _ => return Err("performance export format must be jsonl or csv".into()),
    };
    let Some(path) = app
        .dialog()
        .file()
        .set_title("Export performance measurements")
        .set_file_name(format!("odometer-performance.{extension}"))
        .add_filter(extension.to_ascii_uppercase(), &[extension])
        .blocking_save_file()
    else {
        return Ok(false);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;
    if !path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
    {
        return Err(format!("performance export path must end in .{extension}"));
    }
    let app_state = state.inner().clone();
    let extension = extension.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        app_state.performance.flush();
        let started = Instant::now();
        let result = crate::performance::export(&path, &extension);
        app_state.performance.record_backend(
            "export.performance_data",
            started,
            result.is_ok(),
            BTreeMap::from([("format".into(), extension)]),
        );
        result
    })
    .await
    .map_err(|error| error.to_string())??;
    Ok(true)
}

#[tauri::command]
pub async fn correlate_events(
    state: State<'_, Arc<AppState>>,
    query: crate::correlation::CorrelationQuery,
) -> Result<crate::correlation::CorrelationResult, String> {
    let started = Instant::now();
    if query.events.len() > 2_000 {
        return Err("correlation is limited to 2,000 events per request".into());
    }
    if !(-365..=365).contains(&query.before_days) || !(-365..=365).contains(&query.after_days) {
        return Err("correlation windows are limited to 365 days".into());
    }
    let app_state = state.inner().clone();
    let event_count = query.events.len();
    // Issue #139: `state.sessions` holds only resident summaries now, and
    // `correlate` needs full per-turn/token history — but `correlate_events`
    // has no session-id scoping of its own, so loading every resident
    // session's full content on every call would turn a live in-memory
    // clone into a whole-corpus ledger read on a hot-ish endpoint
    // (`ConfigTimeline.svelte` re-runs this on every live session-store
    // flush while its tab is open). `candidate_session_keys` narrows this
    // to sessions that could actually contribute — matching the query's
    // scope and overlapping at least one of its windows — using only the
    // already-resident summaries, before any ledger read happens. See its
    // doc comment for why that pre-filter cannot change the result.
    let blocking_state = app_state.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let summaries: Vec<(String, SessionSummary)> = blocking_state
            .sessions
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().summary.clone()))
            .collect();
        let candidate_ids = crate::correlation::candidate_session_keys(
            summaries
                .iter()
                .map(|(key, summary)| (key.as_str(), summary)),
            &query,
        );
        let sessions = blocking_state.full_sessions(&candidate_ids)?;
        Ok::<_, String>((
            sessions.len(),
            crate::correlation::correlate(&sessions, query),
        ))
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner);
    let session_count = result.as_ref().map(|(count, _)| *count).unwrap_or(0);
    app_state.performance.record_backend(
        "ipc.correlate_events",
        started,
        result.is_ok(),
        BTreeMap::from([
            ("sessions".into(), session_count.to_string()),
            ("events".into(), event_count.to_string()),
        ]),
    );
    result.map(|(_, correlation)| correlation)
}

/// Evaluates local, HEAD-reachable commits only. The gix repository handle is
/// read-only here: no remotes, hooks, shell commands, index, or worktree writes.
#[tauri::command]
pub async fn scan_git_outcomes(
    state: State<'_, Arc<AppState>>,
    post_window_hours: Option<i64>,
) -> Result<Vec<crate::git_outcomes::GitOutcome>, String> {
    let post_window_hours = post_window_hours.unwrap_or(24);
    if !(0..=8_760).contains(&post_window_hours) {
        return Err("git outcome window must be between 0 and 8760 hours".into());
    }
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        // Issue #139: no session-id scoping here either, so this is a
        // whole-corpus full-session load from the ledger now, same
        // reasoning as `correlate_events`.
        let ids: Vec<String> = state
            .sessions
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let sessions = state.full_sessions(&ids)?;
        let (outcomes, events) = crate::git_outcomes::evaluate(&sessions, post_window_hours);
        state.extend_external_events(events);
        state.performance.record_backend(
            "ipc.scan_git_outcomes",
            started,
            true,
            BTreeMap::from([
                ("sessions".into(), sessions.len().to_string()),
                ("outcomes".into(), outcomes.len().to_string()),
            ]),
        );
        Ok::<_, String>(outcomes)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner)
}

#[tauri::command]
pub fn set_tray_totals(
    state: State<'_, Arc<AppState>>,
    totals: crate::tray::TrayTotals,
) -> Result<(), String> {
    crate::tray::update(state.inner(), totals)
}

fn write_export_file(path: &std::path::Path, format: &str, content: &str) -> Result<(), String> {
    let allowed = path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(format));
    if !allowed {
        return Err(format!("export path must end in .{format}"));
    }
    std::fs::write(path, content).map_err(|error| format!("failed to write export: {error}"))
}

/// Opens the native save dialog in Rust, then writes only to the path returned
/// by that dialog. The webview never receives an arbitrary-path write command.
#[tauri::command]
pub async fn write_export(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    default_name: String,
    format: String,
    content: String,
) -> Result<bool, String> {
    let extension = match format.as_str() {
        "csv" => "csv",
        "json" => "json",
        _ => return Err("export format must be csv or json".into()),
    };
    if content.len() > 128 * 1024 * 1024 {
        return Err("export exceeds the 128 MiB safety limit".into());
    }

    let stem = std::path::Path::new(&default_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("odometer-export");
    let file_name = format!("{stem}.{extension}");
    let Some(path) = app
        .dialog()
        .file()
        .set_title("Export Odometer sessions")
        .set_file_name(file_name)
        .add_filter(extension.to_ascii_uppercase(), &[extension])
        .blocking_save_file()
    else {
        return Ok(false);
    };
    let path = path.into_path().map_err(|error| error.to_string())?;
    let app_state = state.inner().clone();
    let extension = extension.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let started = Instant::now();
        let result = write_export_file(&path, &extension, &content);
        app_state.performance.record_backend(
            "export.session_data",
            started,
            result.is_ok(),
            BTreeMap::from([
                ("format".into(), extension),
                ("bytes".into(), content.len().to_string()),
            ]),
        );
        result
    })
    .await
    .map_err(|error| error.to_string())??;
    Ok(true)
}

/// Returns lightweight summaries of all known sessions. Full sessions
/// (turns, token history) are fetched per-id via `get_session_details` —
/// shipping them all here measured ~200 MB of JSON on a real corpus.
/// Issue #139: `state.sessions` now holds the resident summary directly, so
/// this is a cheap clone rather than a `SessionSummary::of` recompute (which
/// used to re-derive `buckets` from the full `tokens_history` on every call).
#[tauri::command]
pub fn list_sessions(state: State<'_, Arc<AppState>>) -> Vec<SessionSummary> {
    let started = Instant::now();
    let result: Vec<_> = state
        .sessions
        .iter()
        .map(|entry| entry.value().summary.clone())
        .collect();
    state.performance.record_backend(
        "ipc.list_sessions",
        started,
        true,
        BTreeMap::from([("sessions".into(), result.len().to_string())]),
    );
    result
}

/// Returns one full session (turns and token history included), for the
/// detail drawer. Issue #139: `state.sessions` holds only a resident summary,
/// so this now loads full content on demand — from the ledger, or from the
/// resident full-content fallback for a session the ledger cannot currently
/// vouch for. `Ok(None)` means the id is not a session this process knows
/// about; `Err` means the id is known but its full content could not be
/// loaded right now (#116: never silently substituted with an empty/zero
/// session).
#[tauri::command]
pub fn get_session_details(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<Option<Session>, String> {
    let started = Instant::now();
    let result = state.full_session(&session_id);
    let found = result.as_ref().map(|s| s.is_some()).unwrap_or(false);
    state.performance.record_backend(
        "ipc.get_session_details",
        started,
        result.is_ok(),
        BTreeMap::from([("found".into(), found.to_string())]),
    );
    result
}

/// Wire form of one provider's most-recent subscription-usage snapshot,
/// returned by `get_subscription_usage`.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
pub struct SubscriptionUsageEntry {
    pub harness: Harness,
    pub captured_at: DateTime<Utc>,
    pub plan_type: Option<String>,
    pub credits_unlimited: Option<bool>,
    pub credits_balance: Option<f64>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
}

/// For each harness with at least one rate-limit snapshot, picks the newest
/// snapshot (by timestamp) across all of that harness's sessions and pairs
/// it with the account fields (`plan_type`/`credits_*`) from the session
/// that recorded it. Harnesses with no snapshots — Claude Code transcripts
/// carry none today — are omitted rather than padded with nulls.
fn newest_subscription_usage_by_harness<'a>(
    sessions: impl Iterator<Item = &'a crate::model::ResidentSession>,
) -> Vec<SubscriptionUsageEntry> {
    let mut newest: BTreeMap<Harness, (&'a SessionSummary, &'a RateLimitSnapshotPoint)> =
        BTreeMap::new();
    for resident in sessions {
        let session = &resident.summary;
        let Some(point) = resident.newest_rate_limit_point.as_ref() else {
            continue;
        };
        let is_newer = newest
            .get(&session.harness)
            .is_none_or(|(_, existing)| point.timestamp > existing.timestamp);
        if is_newer {
            newest.insert(session.harness.clone(), (session, point));
        }
    }
    newest
        .into_values()
        .map(|(session, point)| SubscriptionUsageEntry {
            harness: session.harness.clone(),
            captured_at: point.timestamp,
            plan_type: session.plan_type.clone(),
            credits_unlimited: session.credits_unlimited,
            credits_balance: session.credits_balance,
            primary: point.primary.clone(),
            secondary: point.secondary.clone(),
        })
        .collect()
}

/// Most-recent provider-reported subscription-usage snapshot per harness.
/// Issue #139: `newest_rate_limit_point` is one of the fields the resident
/// summary carries beyond the wire-shape `SessionSummary` specifically so
/// this command (and `diagnostics::collect_session_stats`) never need a
/// ledger read — cheap `Arc` clones of the already-resident projection, same
/// as before. Issue #152: it holds only the newest point (not the session's
/// whole history), which is all this command ever needed.
#[tauri::command]
pub fn get_subscription_usage(state: State<'_, Arc<AppState>>) -> Vec<SubscriptionUsageEntry> {
    let started = Instant::now();
    let sessions: Vec<Arc<crate::model::ResidentSession>> = state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    let result = newest_subscription_usage_by_harness(sessions.iter().map(Arc::as_ref));
    state.performance.record_backend(
        "ipc.get_subscription_usage",
        started,
        true,
        BTreeMap::from([("providers".into(), result.len().to_string())]),
    );
    result
}

/// One [from, to] window for `sessions_in_ranges`. Bounds are inclusive
/// RFC3339 instants; None is an open bound.
#[derive(Debug, serde::Deserialize)]
pub struct RangeBounds {
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Date-scoped token/credit rollups for every session across several windows
/// at once, computed from the in-memory event histories in a single pass.
/// Sessions with no token or tool activity in a window are omitted from that
/// window's map — the frontend treats a missing entry as zero. Async so the
/// walk runs on a worker thread instead of stalling the main thread's IPC.
#[tauri::command]
pub async fn sessions_in_ranges(
    state: State<'_, Arc<AppState>>,
    ranges: Vec<RangeBounds>,
    session_ids: Option<Vec<String>>,
) -> Result<Vec<HashMap<String, RangeTotals>>, String> {
    let started = Instant::now();
    if ranges.len() > 64 {
        return Err("range rollups are limited to 64 windows per request".into());
    }
    let parse = |v: &Option<String>| -> Result<Option<DateTime<Utc>>, String> {
        v.as_ref()
            .map(|s| s.parse().map_err(|e| format!("invalid timestamp: {e}")))
            .transpose()
    };
    let bounds = ranges
        .iter()
        .map(|r| Ok((parse(&r.from)?, parse(&r.to)?)))
        .collect::<Result<Vec<_>, String>>()?;
    let app_state = state.inner().clone();
    let keys: Vec<String> = match session_ids {
        Some(ids) => ids,
        None => app_state
            .sessions
            .iter()
            .map(|entry| entry.key().clone())
            .collect(),
    };
    let session_count = keys.len();
    let range_count = bounds.len();
    // The ledger is the accounting authority for this endpoint (AGENTS.md).
    // While it is still opening/migrating (#116), the in-memory fallback
    // below would only cover whatever the bulk scan has managed to observe
    // so far — itself gated behind this same readiness signal, see
    // `spawn_scan` — and a partial-looking-complete answer is worse than an
    // honest "not ready yet". Callers already treat a failure here as "leave
    // the cache as-is and retry on the next mutation" (see
    // `SessionsView.svelte`'s `sessions_in_ranges` call sites), which
    // self-heals automatically once the scan resumes after the ledger opens
    // and its `session-updated` events retrigger the fetch.
    if matches!(app_state.history_readiness(), HistoryReadinessKind::Pending) {
        app_state.performance.record_backend(
            "ipc.sessions_in_ranges",
            started,
            false,
            BTreeMap::from([
                ("sessions".into(), session_count.to_string()),
                ("ranges".into(), range_count.to_string()),
                ("source".into(), "pending".into()),
            ]),
        );
        return Err("durable history is still preparing; retry shortly".into());
    }
    let blocking_state = app_state.clone();
    let result = tauri::async_runtime::spawn_blocking(move || -> Result<_, String> {
        // The ledger is authoritative when available; sessions whose latest
        // persist failed (plus everything, when the store never opened) fall
        // back to walking in-memory history, keeping answers complete.
        let (ledger_keys, memory_keys): (Vec<String>, Vec<String>) =
            match blocking_state.history_ready() {
                Some(_) => keys
                    .into_iter()
                    .partition(|key| !blocking_state.ledger_is_stale(key)),
                None => (Vec::new(), keys),
            };
        let mut source = "memory";
        let mut out: Vec<HashMap<String, RangeTotals>> = vec![HashMap::new(); bounds.len()];
        let mut memory_keys = memory_keys;
        if let Some(history) = blocking_state.history_ready() {
            match history.range_totals_multi(&ledger_keys, &bounds) {
                Ok(maps) => {
                    source = if memory_keys.is_empty() {
                        "ledger"
                    } else {
                        "mixed"
                    };
                    out = maps;
                }
                Err(error) => {
                    tracing::warn!(
                        "ledger range aggregation failed; recomputing in memory: {}",
                        error
                    );
                    // Distinct from store-absent "memory" so recordings can
                    // spot ledger regressions rather than configuration.
                    source = "fallback";
                    memory_keys.extend(ledger_keys);
                }
            }
        }
        if !memory_keys.is_empty() {
            // Issue #139: `state.sessions` no longer carries full
            // `tokens_history`, so the in-memory fallback for exactly these
            // (expected-rare) sessions resolves full content on demand —
            // from the resident full-content fallback for a genuinely
            // ledger-stale session, or a ledger read for a session whose
            // facts are correct but rollups lag. A failure here is a hard
            // error for the whole call rather than a silent partial: a
            // window with no entry for one of these sessions reads as zero
            // to the frontend, which would be exactly #116's silent
            // undercount if the miss were swallowed instead.
            let sessions = blocking_state.full_sessions(&memory_keys)?;
            for session in sessions {
                let key = session.effective_storage_id();
                for (i, rt) in session.range_totals_multi(&bounds).into_iter().enumerate() {
                    if range_has_data(&rt) {
                        out[i].insert(key.clone(), rt);
                    }
                }
            }
        }
        Ok((out, source))
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|inner| inner);
    let source = result
        .as_ref()
        .map(|(_, source)| *source)
        .unwrap_or("failed");
    app_state.performance.record_backend(
        "ipc.sessions_in_ranges",
        started,
        result.is_ok(),
        BTreeMap::from([
            ("sessions".into(), session_count.to_string()),
            ("ranges".into(), range_count.to_string()),
            ("source".into(), source.to_string()),
        ]),
    );
    result.map(|(out, _)| out)
}

#[derive(Debug, serde::Deserialize)]
pub struct ToolImpactScopeQuery {
    pub from: Option<String>,
    pub to: Option<String>,
    pub session_ids: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ToolImpactQuery {
    pub target_kind: crate::tool_impact::ToolImpactTargetKind,
    pub target_key: String,
    pub from: Option<String>,
    pub to: Option<String>,
    pub session_ids: Option<Vec<String>>,
}

type ToolImpactRange = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

fn parse_tool_impact_range(
    from: Option<String>,
    to: Option<String>,
) -> Result<ToolImpactRange, String> {
    let parse = |value: Option<String>| -> Result<Option<DateTime<Utc>>, String> {
        value
            .map(|timestamp| {
                timestamp
                    .parse()
                    .map_err(|error| format!("invalid timestamp: {error}"))
            })
            .transpose()
    };
    let from = parse(from)?;
    let to = parse(to)?;
    if from.zip(to).is_some_and(|(from, to)| from > to) {
        return Err("comparison start must not be after its end".into());
    }
    Ok((from, to))
}

/// Resolves the ids in scope for a tool-impact query (issue #139 follow-up):
/// the caller's explicit list (filtered to ids this process actually knows
/// about, same as before #139) or the whole corpus, further narrowed by
/// `tool_impact::candidate_session_keys` to sessions whose resident summary
/// says they could actually contribute to `[from, to]` — see its doc
/// comment for why that narrowing cannot change `list_targets`/`compare`'s
/// result. Full session content is *not* resolved here: that is a possible
/// ledger read, done inside `spawn_blocking` via `AppState::full_sessions`
/// alongside the CPU-bound aggregation itself.
fn tool_impact_session_ids(
    app_state: &AppState,
    session_ids: Option<Vec<String>>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Vec<String> {
    let summaries: Vec<(String, SessionSummary)> = app_state
        .sessions
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().summary.clone()))
        .collect();
    let scoped: std::collections::HashSet<String> = match session_ids {
        Some(ids) => ids
            .into_iter()
            .filter(|id| app_state.sessions.contains_key(id))
            .collect(),
        None => summaries.iter().map(|(key, _)| key.clone()).collect(),
    };
    crate::tool_impact::candidate_session_keys(
        summaries
            .iter()
            .filter(|(key, _)| scoped.contains(key))
            .map(|(key, summary)| (key.as_str(), summary)),
        from,
        to,
    )
}

#[tauri::command]
pub async fn list_tool_impact_targets(
    state: State<'_, Arc<AppState>>,
    query: ToolImpactScopeQuery,
) -> Result<Vec<crate::tool_impact::ToolImpactTarget>, String> {
    let started = Instant::now();
    let (from, to) = parse_tool_impact_range(query.from, query.to)?;
    let app_state = state.inner().clone();
    let ids = tool_impact_session_ids(&app_state, query.session_ids, from, to);
    let session_count = ids.len();
    let blocking_state = app_state.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let sessions = blocking_state.full_sessions(&ids)?;
        Ok::<_, String>(crate::tool_impact::list_targets(&sessions, from, to))
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner);
    app_state.performance.record_backend(
        "ipc.list_tool_impact_targets",
        started,
        result.is_ok(),
        BTreeMap::from([("sessions".into(), session_count.to_string())]),
    );
    result
}

#[tauri::command]
pub async fn compare_tool_impact(
    state: State<'_, Arc<AppState>>,
    query: ToolImpactQuery,
) -> Result<crate::tool_impact::ToolImpactResult, String> {
    let started = Instant::now();
    let target_key = query.target_key.trim().to_ascii_lowercase();
    let max_length = match query.target_kind {
        crate::tool_impact::ToolImpactTargetKind::Provider => 64,
        crate::tool_impact::ToolImpactTargetKind::Tool => 128,
    };
    if target_key.is_empty()
        || target_key.len() > max_length
        || !target_key
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(format!(
            "target key must be 1-{max_length} letters, numbers, underscores, hyphens, or periods"
        ));
    }
    let (from, to) = parse_tool_impact_range(query.from, query.to)?;

    let app_state = state.inner().clone();
    let ids = tool_impact_session_ids(&app_state, query.session_ids, from, to);
    let session_count = ids.len();
    let target_kind = query.target_kind;
    let blocking_state = app_state.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let sessions = blocking_state.full_sessions(&ids)?;
        Ok::<_, String>(crate::tool_impact::compare(
            &sessions,
            target_kind,
            &target_key,
            from,
            to,
        ))
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner);
    app_state.performance.record_backend(
        "ipc.compare_tool_impact",
        started,
        result.is_ok(),
        BTreeMap::from([("sessions".into(), session_count.to_string())]),
    );
    result
}

pub(crate) fn range_has_data(range: &RangeTotals) -> bool {
    range.tokens.total_tokens != 0 || range.tool_metrics.calls != 0
}

/// Returns the current configuration.
#[tauri::command]
pub fn get_config(state: State<'_, Arc<AppState>>) -> Result<Config, String> {
    let started = Instant::now();
    let result = Config::load().map_err(|e| e.to_string());
    state
        .performance
        .record_backend("ipc.get_config", started, result.is_ok(), BTreeMap::new());
    result
}

/// Reports whether each requested harness hook is actually installed and the
/// last bounded, local hook result. This makes setup observable without
/// exposing transcript contents or arbitrary configuration data.
#[tauri::command]
pub fn get_turn_receipt_status(
    state: State<'_, Arc<AppState>>,
) -> Result<crate::harness_integration::TurnReceiptIntegrationStatus, String> {
    let _transition = state.config_transition.lock().unwrap();
    let config = Config::load().map_err(|error| error.to_string())?;
    Ok(crate::harness_integration::status(&config))
}

/// Reconciles the installed handlers with the already-saved opt-in settings.
/// It is intentionally separate from startup so Odometer never repairs or
/// writes harness configuration merely because the app was opened.
#[tauri::command]
pub fn repair_turn_receipt_integrations(
    state: State<'_, Arc<AppState>>,
) -> Result<crate::harness_integration::TurnReceiptIntegrationStatus, String> {
    let _transition = state.config_transition.lock().unwrap();
    let config = Config::load().map_err(|error| error.to_string())?;
    let transaction =
        crate::harness_integration::sync(&config).map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(crate::harness_integration::status(&config))
}

/// Persists a new configuration and emits "config-updated". Non-source
/// changes apply live; receipt changes also transactionally reconcile their
/// harness hooks. Session-source changes clear the session cache, restart
/// watchers, and rescan in the background.
#[tauri::command]
pub fn set_config(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    mut config: Config,
) -> Result<(), String> {
    let _transition = state.config_transition.lock().unwrap();
    let started = Instant::now();
    if !(1..=1_024).contains(&config.performance_log_max_mb) {
        return Err("performance log size must be between 1 and 1024 MiB".into());
    }
    crate::instructions::validate_instruction_roots(&config.instruction_roots)
        .map_err(|error| error.to_string())?;
    let previous = Config::load().map_err(|e| e.to_string())?;
    preserve_backend_owned_config(&previous, &mut config);
    let session_sources_changed = !previous.session_sources_equal(&config);
    let instruction_sources_changed = previous.instructions_enabled != config.instructions_enabled
        || previous.instruction_roots != config.instruction_roots;
    let instruction_settings_changed = instruction_sources_changed
        || previous.instructions_tab_visible != config.instructions_tab_visible;
    let receipt_settings_changed =
        crate::harness_integration::receipt_settings_changed(&previous, &config);
    // Non-session changes take effect immediately and do not force a full
    // corpus rescan. Instruction-source changes replace only the safe config watcher.
    if !session_sources_changed {
        let config_watcher_replacement = instruction_sources_changed
            .then(|| {
                crate::config_events::start(app.clone(), state.inner().clone(), &config)
                    .map_err(|error| error.to_string())
            })
            .transpose()?;
        let integration = receipt_settings_changed
            .then(|| crate::harness_integration::sync(&config))
            .transpose()
            .map_err(|error| error.to_string())?;
        if let Err(error) = config.save() {
            let error = error.to_string();
            if let Some(transaction) = integration {
                return match transaction.abort() {
                    Ok(()) => Err(error),
                    Err(rollback) => Err(format!("{error}; {rollback}")),
                };
            }
            return Err(error);
        }
        if instruction_sources_changed {
            state.cancel_instruction_scan_and_clear_paths();
            if !config.instructions_enabled {
                // Disabling the feature must also retire the persisted index.
                crate::instructions::remove_persisted_inventory();
            }
            let previous_watcher = state
                .config_watcher
                .lock()
                .unwrap()
                .replace(config_watcher_replacement.expect("replacement was staged"));
            drop(previous_watcher);
        }
        // A commit-time cleanup warning does not undo the durable config or
        // installed hooks. Keep applying live state and emit the new config
        // before surfacing that warning to the caller.
        let integration_error = integration
            .and_then(|transaction| transaction.commit().err())
            .map(|error| error.to_string());
        state.performance.configure(
            config.performance_tracking_enabled,
            config.performance_log_max_mb,
        );
        crate::memory::configure_heap_tracking(config.memory_heap_tracking_enabled);
        if let Err(error) = app.emit("config-updated", &config) {
            let error = error.to_string();
            return Err(match integration_error.as_deref() {
                Some(integration_error) => format!("{integration_error}; {error}"),
                None => error,
            });
        }
        state.performance.record_backend(
            if instruction_settings_changed {
                "settings.save_instructions"
            } else if receipt_settings_changed {
                "settings.save_turn_receipts"
            } else {
                "settings.save_performance"
            },
            started,
            integration_error.is_none(),
            BTreeMap::new(),
        );
        if let Some(error) = integration_error {
            return Err(error);
        }
        return Ok(());
    }

    // Stage the replacement before changing durable or live state. If watcher
    // construction fails, the existing configuration remains fully active.
    let provider_sources = config
        .provider_sources()
        .map_err(|error| error.to_string())?;
    let replacement = crate::watcher::start(
        app.clone(),
        state.inner().clone(),
        provider_sources.clone(),
        config.session_index_path.clone(),
    )
    .map_err(|e| e.to_string())?;
    let integration = receipt_settings_changed
        .then(|| crate::harness_integration::sync(&config))
        .transpose()
        .map_err(|error| error.to_string())?;
    if let Err(error) = config.save() {
        let error = error.to_string();
        if let Some(transaction) = integration {
            return match transaction.abort() {
                Ok(()) => Err(error),
                Err(rollback) => Err(format!("{error}; {rollback}")),
            };
        }
        return Err(error);
    }
    state.cancel_instruction_scan_and_clear_paths();
    if !config.instructions_enabled {
        // Disabling the feature must also retire the persisted index.
        crate::instructions::remove_persisted_inventory();
    }
    // A commit-time cleanup warning does not undo the durable config or
    // installed hooks. Keep applying live state and emit the new config
    // before surfacing that warning to the caller.
    let integration_error = integration
        .and_then(|transaction| transaction.commit().err())
        .map(|error| error.to_string());
    state.performance.configure(
        config.performance_tracking_enabled,
        config.performance_log_max_mb,
    );
    crate::memory::configure_heap_tracking(config.memory_heap_tracking_enabled);

    // Invalidate every prior scan before swapping watchers. Dropping the old
    // handle waits out any in-flight callback; clearing then removes all data
    // from the previous generation before the replacement scan begins.
    state.advance_scan_generation();
    let previous_watcher = state.watcher.lock().unwrap().replace(replacement);
    drop(previous_watcher);
    state.clear_sessions();
    state.scanned.store(false, Ordering::Release);
    state.scan_done.store(0, Ordering::Release);
    state.scan_total.store(0, Ordering::Release);
    state.scan_elapsed_ms.store(0, Ordering::Release);
    *state.cold_reason.lock().unwrap() = None;

    spawn_scan(
        app.clone(),
        state.inner().clone(),
        config.clone(),
        provider_sources,
        true,
    );

    if let Err(error) = app.emit("config-updated", &config) {
        let error = error.to_string();
        return Err(match integration_error.as_deref() {
            Some(integration_error) => format!("{integration_error}; {error}"),
            None => error,
        });
    }

    state.performance.record_backend(
        "settings.save_session_sources",
        started,
        integration_error.is_none(),
        BTreeMap::new(),
    );

    if let Some(error) = integration_error {
        return Err(error);
    }

    Ok(())
}

/// Wire form of a registered provider, for descriptor-driven frontend
/// surfaces (tabs, badges, filters, empty states).
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProviderDescriptorWire {
    pub id: crate::provider::ProviderId,
    pub display_name: String,
    pub archived_sources: bool,
    pub session_index: bool,
    pub currency: String,
    pub deep_link: bool,
    pub quota_source: bool,
    /// Issue #44 open-set tool/context dimension availability; see
    /// `ProviderCapabilities`'s field docs.
    pub mcp_dimension: bool,
    pub shell_dimension: bool,
    pub language_dimension: bool,
    pub context_dimension: bool,
}

#[tauri::command]
pub fn list_providers() -> Vec<ProviderDescriptorWire> {
    crate::provider::ProviderRegistry::builtin()
        .descriptors()
        .map(|descriptor| ProviderDescriptorWire {
            id: descriptor.id.clone(),
            display_name: descriptor.display_name.to_string(),
            archived_sources: descriptor.capabilities.archived_sources,
            session_index: descriptor.capabilities.session_index,
            currency: descriptor.capabilities.currency.to_string(),
            deep_link: descriptor.capabilities.deep_link,
            quota_source: descriptor.capabilities.quota_source,
            mcp_dimension: descriptor.capabilities.mcp_dimension,
            shell_dimension: descriptor.capabilities.shell_dimension,
            language_dimension: descriptor.capabilities.language_dimension,
            context_dimension: descriptor.capabilities.context_dimension,
        })
        .collect()
}

/// The provider-registry diagnostics report (issue #39): capability flags,
/// configured/default roots and their existence, discovery/parse counters
/// from the last completed scan, ledger health, pricing coverage, retention
/// risk, and quota-source status, per provider. Synchronous and bounded — a
/// handful of `Path::exists()` checks plus one pass over the already-loaded
/// session map — so it never delays startup and needs no cancellation path.
/// The local UI may show the exact paths this returns; redaction for export
/// happens in the frontend before the JSON leaves the app (see
/// `src/lib/diagnosticsExport.ts`).
#[tauri::command]
pub fn get_provider_diagnostics(
    state: State<'_, Arc<AppState>>,
) -> crate::diagnostics::DiagnosticsReport {
    let started = Instant::now();
    let config = Config::load().unwrap_or_default();
    let rates = get_rates();
    let report = crate::diagnostics::generate_report(&state, &config, &rates);
    state.performance.record_backend(
        "ipc.get_provider_diagnostics",
        started,
        true,
        BTreeMap::from([("providers".into(), report.providers.len().to_string())]),
    );
    report
}

fn preserve_backend_owned_config(previous: &Config, next: &mut Config) {
    next.defender_exclusion_receipt = previous.defender_exclusion_receipt.clone();
    // The Settings UI still edits the legacy flat root fields — but it echoes
    // back every wire field it received, including a `providers` map and
    // `config_version: 1` from get_config. Left versioned, normalization
    // would rebuild the flat fields from that stale map and silently discard
    // the user's root edits. Until the UI edits the map directly, an incoming
    // payload is always treated as legacy-authoritative for the builtin
    // providers; its flat fields win.
    next.config_version = 0;
    // Carry over provider-map entries for providers the frontend does not
    // know about, so a config written by a newer build (or for a
    // not-yet-registered provider) survives a settings save even through
    // clients that strip unknown wire fields. Legacy-branch normalization
    // only rewrites builtin entries, leaving these untouched.
    let registry = crate::provider::ProviderRegistry::builtin();
    let builtin_ids: Vec<_> = registry.descriptors().map(|d| d.id.clone()).collect();
    for (id, entry) in &previous.providers {
        if !builtin_ids.contains(id) {
            next.providers
                .entry(id.clone())
                .or_insert_with(|| entry.clone());
        }
    }
}

/// Scans all configured roots on a background thread, inserting sessions
/// into state and emitting a "session-updated" summary for each as it
/// parses. Applies the session-index name overlay and sets `scanned` when
/// done. Shared by startup (lib.rs) and set_config.
pub fn spawn_scan(
    app: AppHandle,
    state: Arc<AppState>,
    config: Config,
    provider_sources: crate::provider::ProviderSourceSet,
    source_configuration_valid: bool,
) {
    let generation = state.current_scan_generation();
    state.scanned.store(false, Ordering::Release);
    state.scan_done.store(0, Ordering::Release);
    state.scan_total.store(0, Ordering::Release);
    *state.cold_reason.lock().unwrap() = None;

    std::thread::spawn(move || {
        // The durable archive may still be opening/migrating (#116):
        // AppState starts `Pending` and only `spawn_history_open` resolves
        // it. Waiting here — rather than treating Pending like a
        // permanently unavailable archive — preserves the ordering
        // `AppState::new()` used to guarantee synchronously before #116:
        // every archived session is hydrated into `state.sessions` (by
        // `AppState::set_history_ready`) before this scan's first
        // `observe()`, so a bulk scan can never race `hydrate_history`'s
        // wholesale overwrite of the in-memory map.
        state.wait_for_history_ready();

        // Reserve the durable-write path, waiting out an in-flight rebuild
        // (issue #176) — the two would otherwise interleave writes to the
        // same `durable_sessions` rows, and a scan replaying a cached
        // pre-#153 session could land after the rebuild wrote the collapsed
        // one for that key. Held for the rest of this thread, so every
        // superseded-generation early return below releases it.
        //
        // Ordered after `wait_for_history_ready` deliberately: a rebuild
        // cannot start until the archive is `Ready` anyway, so waiting here
        // never holds the reservation across the archive's own opening.
        let _durable_write = state.begin_scan_after_any_rebuild();

        // Allocate the archive generation before workers start. Live
        // watcher writes during discovery use the same generation,
        // preventing a newly created file from being falsely marked missing
        // at completion.
        let history_generation = source_configuration_valid
            .then(|| state.begin_history_scan())
            .flatten();

        let started = std::time::Instant::now();
        // An invalid legacy source configuration must not prune an otherwise
        // healthy cache merely because the fail-closed scan has no roots.
        let cache_path = source_configuration_valid
            .then(scan_cache::default_path)
            .flatten();

        // Opened here, not inside scan_all: the caller needs cold_reason
        // before the scan's progress events start firing, and a cache must
        // only ever be opened once (a second open on the same file would see
        // this open's just-written version metadata and report a false-warm
        // cache).
        let cache_open_started = Instant::now();
        let cache = cache_path.as_deref().map(scan_cache::ScanCache::load);
        let cache_open_ms = cache_open_started.elapsed().as_secs_f64() * 1_000.0;
        let cold_reason = cache.as_ref().and_then(scan_cache::ScanCache::cold_reason);
        let cache_invalidation_ms = cache
            .as_ref()
            .map(scan_cache::ScanCache::invalidation_ms)
            .unwrap_or(0.0);
        *state.cold_reason.lock().unwrap() = cold_reason;
        if let Some(pragmas) = cache
            .as_ref()
            .and_then(scan_cache::ScanCache::pragma_snapshot)
        {
            crate::memory::record_sqlite_pragmas(&state.performance, "scan_cache", pragmas);
        }
        if let Some(footprint) = cache
            .as_ref()
            .and_then(scan_cache::ScanCache::database_footprint)
        {
            crate::memory::record_database_footprint(&state.performance, "scan_cache", footprint);
        }

        crate::memory::record_phase_sample(&state.performance, "bulk_scan_parallel", "before");
        // Continuous sampling for this phase's duration (issue #163): a
        // 118s scan otherwise produces only the two boundary samples above,
        // which cannot distinguish "uniformly slower" from "stalled 60s in
        // one place". `None` (no thread) whenever tracking is disabled.
        let progress_state = state.clone();
        let phase_sampler = crate::memory::PhaseSampler::start(
            &state.performance,
            "bulk_scan_parallel",
            std::time::Duration::from_millis(500),
            Box::new(move || crate::memory::PhaseProgress {
                done: Some(progress_state.scan_done.load(Ordering::Acquire) as u64),
                total: Some(progress_state.scan_total.load(Ordering::Acquire) as u64),
            }),
        );
        let report = crate::scanner::scan_all(
            &provider_sources,
            cache,
            |batch| {
                if state.current_scan_generation() != generation {
                    return;
                }
                // One durable-write transaction for the whole batch
                // (issue #132), instead of one per session; everything
                // downstream — in-memory publication, event emission — stays
                // per-session, unchanged from before batching.
                //
                // `scan_all` hands `batch` over by value and drops its own
                // reference immediately after this call (issue #182), so
                // taking ownership here avoids a full deep clone of up to
                // `SCAN_WRITE_BATCH_SIZE` owned `Session`s before the write
                // lock is even taken.
                let reconciled =
                    state.reconcile_scanned_batch_if_current(generation, history_generation, batch);
                for (path, reconciled) in reconciled {
                    let summary = SessionSummary::of(&reconciled.session);
                    if state.publish_scanned_session(generation, &path, reconciled.session) {
                        if let Err(e) = app.emit("session-updated", &summary) {
                            tracing::warn!("emit session-updated failed: {}", e);
                        }
                    }
                    if let Some(displaced) = reconciled.displaced {
                        if let Err(e) = app.emit("session-updated", &SessionSummary::of(&displaced))
                        {
                            tracing::warn!("emit displaced session-updated failed: {}", e);
                        }
                    }
                }
            },
            |done, total| {
                if state.current_scan_generation() != generation {
                    return;
                }
                state.scan_total.store(total, Ordering::Release);
                let previous = state.scan_done.fetch_max(done, Ordering::AcqRel);
                if done < previous {
                    return;
                }
                // Throttle: every 25th file plus the endpoints is smooth
                // enough for a progress line without event spam.
                if done == 0 || done == total || done % 25 == 0 {
                    let _ = app.emit(
                        "scan-progress",
                        &ScanStatus {
                            done,
                            total,
                            complete: false,
                            elapsed_ms: None,
                            cold_reason,
                        },
                    );
                }
            },
        );
        if let Some(phase_sampler) = phase_sampler {
            phase_sampler.stop();
        }
        crate::memory::record_phase_sample(&state.performance, "bulk_scan_parallel", "after");

        if state.current_scan_generation() != generation {
            return;
        }

        // Rebuilds every `rollup_*` table once, in a single pass, if the
        // scan just run deferred any per-session rollup maintenance
        // (issue #132). Unconditional — unlike the source-availability
        // finalization below, this does not depend on
        // `source_configuration_valid` or `report.parse_failures`: a
        // deferred rollup is a property of what `observe_bulk` durably wrote
        // during this scan, not of whether the scan's overall result can be
        // trusted enough to mark missing sources. Skipping it on a
        // parse-failure scan would leave sessions_in_ranges routing those
        // sessions through the slower in-memory fallback for the rest of
        // this process's lifetime, not just until the next scan.
        //
        // Timed as its own operation (issue #140): before this, the rebuild
        // was folded into `startup.bulk_scan`'s undifferentiated ~40-45s
        // post-scan residual, so a version-over-version delta was the only
        // evidence of its cost.
        crate::memory::record_phase_sample(&state.performance, "rollup_rebuild", "before");
        let rollup_rebuild_started = Instant::now();
        let rollup_outcome = state.finalize_bulk_scan_rollups();
        // `scope` and `sessions` alongside `rebuilt` (issue #154): every
        // recording from v0.8.9 on reported `rebuilt: true` on every launch,
        // which could not distinguish a rebuild proportional to what the scan
        // changed from a full O(entire ledger) recompute. Those are now the
        // two cases this phase can take, and the metric has to say which.
        let (rebuilt, scope, sessions) = match rollup_outcome {
            crate::history_store::RollupRebuildOutcome::NothingDeferred => {
                (false, "none", String::from("0"))
            }
            crate::history_store::RollupRebuildOutcome::Scoped { sessions } => {
                (true, "deferred_sessions", sessions.to_string())
            }
            crate::history_store::RollupRebuildOutcome::EntireLedger => {
                (true, "entire_ledger", String::from("all"))
            }
        };
        state.performance.record_backend(
            "startup.bulk_scan.rollup_rebuild",
            rollup_rebuild_started,
            true,
            BTreeMap::from([
                ("rebuilt".into(), rebuilt.to_string()),
                ("scope".into(), scope.into()),
                ("sessions".into(), sessions),
            ]),
        );
        crate::memory::record_phase_sample(&state.performance, "rollup_rebuild", "after");

        // Retained for the on-demand provider diagnostics report (issue #39).
        // Only the newest still-current scan's counters are kept.
        state.record_scan_report(report.clone());

        // A parser/read failure makes a complete source observation
        // untrustworthy. Retain stale-present history rather than incorrectly
        // marking a transcript missing. Timed as its own operation (issue
        // #140): `finish_history_scan` used to also re-hydrate every
        // archived session from the ledger just to notice the handful this
        // scan actually marked missing — see its doc comment — so this phase
        // could plausibly have been most of `startup.bulk_scan`'s
        // unaccounted residual.
        let missing_path_started = Instant::now();
        let mut missing_path_ran = false;
        let mut affected_sessions = 0usize;
        if !source_configuration_valid {
            tracing::warn!(
                "durable source availability was not finalized: invalid source configuration"
            );
        } else if report.parse_failures == 0 {
            if let Some(history_generation) = history_generation {
                missing_path_ran = true;
                let changed = state.finish_history_scan(history_generation);
                affected_sessions = changed.len();
                for session in changed {
                    if let Err(e) = app.emit("session-updated", &SessionSummary::of(&session)) {
                        tracing::warn!("emit session-updated failed: {}", e);
                    }
                }
            }
        } else {
            tracing::warn!(
                "durable source availability was not finalized: {} scan parse failure(s)",
                report.parse_failures
            );
        }
        state.performance.record_backend(
            "startup.bulk_scan.missing_path_pass",
            missing_path_started,
            true,
            BTreeMap::from([
                ("ran".into(), missing_path_ran.to_string()),
                ("affected_sessions".into(), affected_sessions.to_string()),
            ]),
        );

        if state.current_scan_generation() != generation {
            return;
        }

        // Overlay thread names from the session index, if present. Timed as
        // its own operation (issue #140).
        crate::memory::record_phase_sample(&state.performance, "session_index_overlay", "before");
        let session_index_started = Instant::now();
        let names = crate::session_index::read(&config.session_index_path);
        let overlay_ids = crate::session_index::apply(&state.sessions, &names);
        let overlay_changed = overlay_ids.len();

        // Issue #141/field regression: this pass used to call
        // `state.full_session`/`state.full_sessions` (a ledger read of the
        // full session — turns, token histories, tool observations, ~1 MB
        // each in a real corpus) purely to hand a complete `Session` back to
        // `persist_session_metadata_batch`, which only ever needed to change
        // one field. `persist_thread_name_overlay_batch` writes `thread_name`
        // straight from each changed session's resident summary instead, so
        // no full session content is loaded on this path at all — see
        // `HistoryStore::overlay_thread_names`'s doc comment for the
        // investigation behind that.
        let mut changed: Vec<(String, SessionSummary)> = Vec::with_capacity(overlay_ids.len());
        for id in overlay_ids {
            if let Some(summary) = state.sessions.get(&id).map(|entry| entry.summary.clone()) {
                changed.push((id, summary));
            }
        }
        let updates: Vec<(String, Option<String>)> = changed
            .iter()
            .map(|(id, summary)| (id.clone(), summary.thread_name.clone()))
            .collect();
        state.persist_thread_name_overlay_batch(&updates);

        for (_, summary) in &changed {
            if let Err(e) = app.emit("session-updated", summary) {
                tracing::warn!("emit session-updated failed: {}", e);
            }
        }
        state.performance.record_backend(
            "startup.bulk_scan.session_index_overlay",
            session_index_started,
            true,
            BTreeMap::from([("changed".into(), overlay_changed.to_string())]),
        );
        crate::memory::record_phase_sample(&state.performance, "session_index_overlay", "after");

        if state.current_scan_generation() != generation {
            return;
        }

        // Session working directories are now known, so replace the startup
        // watcher with one that also covers project-scoped Codex/Claude
        // configuration surfaces. This same path runs after settings changes.
        let config_watcher_started = Instant::now();
        let config_watcher_result =
            crate::config_events::start(app.clone(), state.clone(), &config);
        let config_watcher_ok = config_watcher_result.is_ok();
        match config_watcher_result {
            Ok(replacement) => {
                let previous = state.config_watcher.lock().unwrap().replace(replacement);
                drop(previous);
            }
            Err(error) => tracing::warn!("could not refresh config watcher: {}", error),
        }
        state.performance.record_backend(
            "background.config_watcher_refresh",
            config_watcher_started,
            config_watcher_ok,
            BTreeMap::from([("sessions".into(), state.sessions.len().to_string())]),
        );

        state.scanned.store(true, Ordering::Release);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        state.scan_elapsed_ms.store(elapsed_ms, Ordering::Release);
        let _ = app.emit(
            "scan-progress",
            &ScanStatus {
                done: state.scan_done.load(Ordering::Acquire),
                total: state.scan_total.load(Ordering::Acquire),
                complete: true,
                elapsed_ms: Some(elapsed_ms),
                cold_reason,
            },
        );
        tracing::info!(
            "scan complete in {:.1?}: {} sessions loaded, {} thread names from index",
            started.elapsed(),
            state.sessions.len(),
            names.len()
        );
        state.performance.record_backend(
            "startup.bulk_scan",
            started,
            source_configuration_valid && report.parse_failures == 0,
            BTreeMap::from([
                ("files".into(), report.files.to_string()),
                ("discovery_ms".into(), format!("{:.3}", report.discovery_ms)),
                (
                    "processing_ms".into(),
                    format!("{:.3}", report.processing_ms),
                ),
                ("cache_open_ms".into(), format!("{:.3}", cache_open_ms)),
                (
                    "cache_invalidation_ms".into(),
                    format!("{:.3}", cache_invalidation_ms),
                ),
                ("cache_hits".into(), report.cache_hits.to_string()),
                ("cache_misses".into(), report.cache_misses.to_string()),
                ("parsed_files".into(), report.parsed_files.to_string()),
                ("parse_failures".into(), report.parse_failures.to_string()),
                (
                    "parse_total_ms".into(),
                    format!("{:.3}", report.parse_total_ms),
                ),
                ("parse_max_ms".into(), format!("{:.3}", report.parse_max_ms)),
                (
                    "cache_lookup_total_ms".into(),
                    format!("{:.3}", report.cache_lookup_total_ms),
                ),
                (
                    "cache_lookup_sql_ms".into(),
                    format!("{:.3}", report.cache_lookup_sql_ms),
                ),
                (
                    "cache_lookup_deserialize_ms".into(),
                    format!("{:.3}", report.cache_lookup_deserialize_ms),
                ),
                (
                    "cache_hit_bytes_total".into(),
                    report.cache_hit_bytes_total.to_string(),
                ),
            ]),
        );
        // Startup's background work (history open/hydrate, this bulk scan,
        // rollup rebuild, session-index overlay, config watcher refresh) is
        // now done, so this is the app's first opportunity to look like it
        // will at steady state — a single sample, not a before/after pair.
        crate::memory::record_phase_sample(&state.performance, "idle", "point");
    });
}

/// Snapshot of the bulk scan's progress, for the UI's startup indicator.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ScanStatus {
    pub done: usize,
    pub total: usize,
    pub complete: bool,
    /// Wall-clock duration of the last completed scan; None while running.
    pub elapsed_ms: Option<u64>,
    /// Why this scan's cache could not be treated as fully warm; None for an
    /// ordinary warm scan.
    pub cold_reason: Option<scan_cache::ColdReason>,
}

/// Returns the current bulk-scan progress. The frontend calls this once on
/// mount (scan-progress events may have fired before its listeners attached)
/// and then follows the "scan-progress" events.
#[tauri::command]
pub fn get_scan_status(state: State<'_, Arc<AppState>>) -> ScanStatus {
    let complete = state.scanned.load(Ordering::Acquire);
    ScanStatus {
        done: state.scan_done.load(Ordering::Acquire),
        total: state.scan_total.load(Ordering::Acquire),
        complete,
        elapsed_ms: complete
            .then(|| state.scan_elapsed_ms.load(Ordering::Acquire))
            .filter(|ms| *ms > 0),
        cold_reason: *state.cold_reason.lock().unwrap(),
    }
}

/// Opens (and migrates, if needed) the durable history archive on a
/// background thread so Tauri can create a window before a chained schema
/// migration on an existing install completes (#116). `AppState` starts in
/// `HistoryReadiness::Pending`; this is the only path that ever resolves it.
/// `spawn_scan` and every IPC command that needs the ledger wait on or check
/// that readiness rather than treating `Pending` like a permanently
/// unavailable archive.
///
/// Records two separate performance metrics rather than one combined
/// `startup.history_open` (issue #132): `HistoryStore::open_default_with_progress`
/// (`Connection::open`, pragmas, `migrate()`) is one phase; `AppState::
/// set_history_ready` — which synchronously calls `hydrate_history`,
/// deserializing every archived session's snapshot to populate
/// `state.sessions` — is a second, separately-timed phase. A v0.8.7 field
/// recording showed 16,794ms for `startup.history_open` with no migration to
/// run between v0.8.6 and v0.8.7; that number was this whole function, not
/// just opening the file, and conflating the two made a hydration-dominated
/// cost look like an open/migration cost. `startup.history_open` is kept as
/// the open/migrate phase alone so it means what its name says; the new
/// `startup.history_hydrate` names the rest.
pub fn spawn_history_open(app: AppHandle, state: Arc<AppState>) {
    std::thread::spawn(move || {
        crate::memory::record_phase_sample(&state.performance, "history_open", "before");
        let open_started = Instant::now();
        let progress_app = app.clone();
        let progress_state = state.clone();
        let result = crate::history_store::HistoryStore::open_default_with_progress(move |event| {
            let snapshot = HistoryStepSnapshot {
                step: event.step.to_string(),
                step_index: event.step_index,
                step_total: event.step_total,
                items_done: event.items_done,
                items_total: event.items_total,
                elapsed_ms: event.elapsed_ms,
            };
            progress_state.record_history_step(snapshot.clone());
            if let Some(elapsed_ms) = event.elapsed_ms {
                progress_state.performance.record_backend_duration_ms(
                    format!("history.migration.{}", event.step),
                    elapsed_ms as f64,
                    true,
                    BTreeMap::from([
                        ("from_version".into(), event.from_version.to_string()),
                        ("to_version".into(), event.to_version.to_string()),
                    ]),
                );
            }
            let _ = progress_app.emit(
                "history-progress",
                &HistoryStatus {
                    status: HistoryReadinessStatus::Pending,
                    step: Some(snapshot.step),
                    step_index: Some(snapshot.step_index),
                    step_total: Some(snapshot.step_total),
                    items_done: snapshot.items_done,
                    items_total: snapshot.items_total,
                    elapsed_ms: snapshot.elapsed_ms,
                },
            );
        });
        let success = result.is_ok();
        // Phase 1 ends here: `Connection::open`, pragmas, and `migrate()`
        // (plus, since this change, a rebuild if #132's crash-safety check
        // found stale rollups from an interrupted prior scan) are done.
        state.performance.record_backend(
            "startup.history_open",
            open_started,
            success,
            BTreeMap::from([("available".into(), success.to_string())]),
        );
        crate::memory::record_phase_sample(&state.performance, "history_open", "after");
        // Phase 2: `set_history_ready` synchronously calls `hydrate_history`,
        // which deserializes every archived session's snapshot to populate
        // `state.sessions` — this is what a v0.8.7 field recording's
        // 16,794ms `startup.history_open` (with no migration to run) was
        // actually measuring, per this function's un-split shape before this
        // change.
        crate::memory::record_phase_sample(&state.performance, "history_hydrate", "before");
        // Continuous sampling for this phase's duration (issue #163). Progress
        // is a proxy — `state.sessions` len — rather than an exact hydration
        // counter: `hydrate_history` populates it one session at a time via
        // `apply_loaded_session`, so this grows in step with hydration, but a
        // concurrently-arriving live watcher event could also add to it. That
        // is an acceptable imprecision for a diagnostic progress readout, not
        // an accounting path. `None` (no thread) whenever tracking is disabled.
        let progress_state = state.clone();
        let hydrate_sampler = crate::memory::PhaseSampler::start(
            &state.performance,
            "history_hydrate",
            std::time::Duration::from_millis(500),
            Box::new(move || crate::memory::PhaseProgress {
                done: Some(progress_state.sessions.len() as u64),
                total: None,
            }),
        );
        let hydrate_started = Instant::now();
        let hydration_stats = match result {
            Ok(store) => {
                crate::memory::record_sqlite_pragmas(
                    &state.performance,
                    "history_store",
                    store.pragma_snapshot(),
                );
                crate::memory::record_database_footprint(
                    &state.performance,
                    "history_store",
                    store.database_footprint(),
                );
                state.set_history_ready(Some(Arc::new(store)))
            }
            Err(error) => {
                tracing::warn!("durable history unavailable: {}", error);
                state.set_history_ready(None)
            }
        };
        if let Some(hydrate_sampler) = hydrate_sampler {
            hydrate_sampler.stop();
        }
        // `sessions`/`bytes` (issue #139): recording both distinguishes "more
        // sessions" from "bigger sessions" as an explanation for this phase's
        // cost, rather than inferring it from a version-over-version session
        // count alone.
        state.performance.record_backend(
            "startup.history_hydrate",
            hydrate_started,
            success,
            BTreeMap::from([
                ("sessions".into(), hydration_stats.sessions.to_string()),
                ("bytes".into(), hydration_stats.bytes.to_string()),
            ]),
        );
        crate::memory::record_phase_sample(&state.performance, "history_hydrate", "after");
        let total_elapsed_ms = open_started.elapsed().as_millis() as u64;
        let last_step = state.last_history_step();
        let _ = app.emit(
            "history-progress",
            &HistoryStatus {
                status: if success {
                    HistoryReadinessStatus::Ready
                } else {
                    HistoryReadinessStatus::Unavailable
                },
                step: last_step.as_ref().map(|s| s.step.clone()),
                step_index: last_step.as_ref().map(|s| s.step_index),
                step_total: last_step.as_ref().map(|s| s.step_total),
                items_done: None,
                items_total: None,
                elapsed_ms: Some(total_elapsed_ms),
            },
        );
    });
}

/// Wire status for `HistoryReadinessKind` (#116): `pending` while the
/// archive is opening/migrating, `ready` once available, `unavailable` if it
/// failed to open (the app still degrades gracefully — live transcripts stay
/// readable, see `AGENTS.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryReadinessStatus {
    Pending,
    Ready,
    Unavailable,
}

/// Durable-history open/migration progress, mirroring `ScanStatus`'s
/// "call `get_history_status` once on mount, then follow `history-progress`
/// events" contract (#116). `step`/`step_index`/`step_total` describe the
/// step most recently reported (in progress while `status` is `pending`,
/// otherwise the last one that ran, or all `None` if the archive needed no
/// migration at all). `items_done`/`items_total` are `Some` only while a
/// step that streams per-row progress — currently just the v5->v6 project
/// identity backfill — is running.
#[derive(Debug, Clone, serde::Serialize)]
pub struct HistoryStatus {
    pub status: HistoryReadinessStatus,
    pub step: Option<String>,
    pub step_index: Option<u32>,
    pub step_total: Option<u32>,
    pub items_done: Option<usize>,
    pub items_total: Option<usize>,
    pub elapsed_ms: Option<u64>,
}

/// Returns the current durable-history open/migration status. The frontend
/// calls this once on mount (progress events may have fired before its
/// listeners attached) and then follows "history-progress" events.
#[tauri::command]
pub fn get_history_status(state: State<'_, Arc<AppState>>) -> HistoryStatus {
    let status = match state.history_readiness() {
        HistoryReadinessKind::Pending => HistoryReadinessStatus::Pending,
        HistoryReadinessKind::Ready => HistoryReadinessStatus::Ready,
        HistoryReadinessKind::Unavailable => HistoryReadinessStatus::Unavailable,
    };
    let last_step = state.last_history_step();
    HistoryStatus {
        status,
        step: last_step.as_ref().map(|s| s.step.clone()),
        step_index: last_step.as_ref().map(|s| s.step_index),
        step_total: last_step.as_ref().map(|s| s.step_total),
        items_done: last_step.as_ref().and_then(|s| s.items_done),
        items_total: last_step.as_ref().and_then(|s| s.items_total),
        elapsed_ms: last_step.as_ref().and_then(|s| s.elapsed_ms),
    }
}

/// Issue #162: re-parses every archived session from its source transcript
/// and `VACUUM`s the archive, on a background thread, so a stored snapshot
/// picks up whatever the current parser now does differently (starting with
/// #153's rate-limit point collapsing) without waiting on that session ever
/// being scanned again. User-triggered only — never automatic on upgrade,
/// so the cost stays opt-in and schedulable — and confirmed by the frontend
/// before this is ever called; this command does not confirm again.
/// Rejects a concurrent second call while one is already running rather than
/// running two at once against the same connection, and equally rejects one
/// started while a bulk scan is running (issue #176) — both write durable
/// session snapshots, so overlapping them makes the result depend on
/// interleaving and invalidates the rebuild's own before/after evidence.
#[tauri::command]
pub fn rebuild_history(app: AppHandle, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let reservation = state.try_begin_rebuild()?;
    state
        .rebuild_cancel_requested
        .store(false, Ordering::Release);
    spawn_history_rebuild(app, state.inner().clone(), reservation);
    Ok(())
}

/// Requests that a running [`rebuild_history`] stop before its next session.
/// Everything already durably written stays written — see
/// [`crate::history_store::HistoryStore::rebuild_from_transcripts`]'s doc
/// comment for why a partial rebuild is still a consistent one. A no-op if
/// no rebuild is running.
#[tauri::command]
pub fn cancel_history_rebuild(state: State<'_, Arc<AppState>>) {
    state
        .rebuild_cancel_requested
        .store(true, Ordering::Release);
}

/// Returns the current history-rebuild status. The frontend calls this once
/// on mount (progress events may have fired before its listeners attached)
/// and then follows "history-rebuild-progress" events — the same contract
/// `get_scan_status`/`get_history_status` establish for their events.
#[tauri::command]
pub fn get_history_rebuild_status(state: State<'_, Arc<AppState>>) -> HistoryRebuildSnapshot {
    state.history_rebuild_status()
}

/// Background half of [`rebuild_history`]. Emits "history-rebuild-progress"
/// (throttled the same way `spawn_scan` throttles "scan-progress": every
/// 25th session plus both endpoints) and publishes each durably-rewritten
/// session into live state exactly as a normal bulk scan's write would, so
/// the UI does not show pre-rebuild content until a restart.
///
/// Progress reporting deliberately does *not* reuse `scan_done`/`scan_total`
/// /the "scan-progress" event: those drive `App.svelte`'s full-screen
/// startup scanning gate (`!scanStore.status.complete`), so publishing a
/// user-triggered Settings action through them would block the whole app
/// behind a "Scanning…" overlay while the rebuild runs in the background.
/// This mirrors "history-progress"'s existing precedent instead — a second
/// event/status pair built on the *same* done/total/complete shape and
/// throttling rule, for a concern `scan-progress` does not own.
pub fn spawn_history_rebuild(
    app: AppHandle,
    state: Arc<AppState>,
    reservation: crate::store::DurableWriteGuard,
) {
    std::thread::spawn(move || {
        // Moved onto this thread and dropped when it ends, however it ends:
        // the early return below, a cancellation, a failure, or a panic all
        // release the durable-write path (issue #176). Previously an atomic
        // cleared only on the paths that reached `finish`.
        let _reservation = reservation;
        let started = Instant::now();
        let finish = |status: HistoryRebuildSnapshot| {
            state.set_history_rebuild_status(status.clone());
            let _ = app.emit("history-rebuild-progress", &status);
        };

        let Some(history) = state.history_ready() else {
            finish(HistoryRebuildSnapshot {
                phase: HistoryRebuildPhase::Failed,
                elapsed_ms: Some(0),
                error: Some("durable history is not available".into()),
                ..Default::default()
            });
            return;
        };

        let footprint_before = history.database_footprint();
        let running = HistoryRebuildSnapshot {
            phase: HistoryRebuildPhase::Running,
            ..Default::default()
        };
        state.set_history_rebuild_status(running.clone());
        let _ = app.emit("history-rebuild-progress", &running);

        // Issue #174: opened here, separately from any concurrent scan's own
        // cache handle, so the rebuild can fold each session's freshly
        // re-parsed content straight into the same cache file a later scan
        // reads — otherwise a populated cache still holding pre-rebuild
        // content would let that later scan replay it straight back into
        // the ledger this rebuild just shrank. `ScanCache::load` tolerates
        // concurrent opens from other processes/threads (see its own
        // generation bookkeeping), so this needs no coordination with
        // `spawn_scan`'s handle. `None` (no cache directory resolvable)
        // degrades to the pre-#174 behavior: the history store still gets
        // rebuilt, just without the scan-cache fix.
        let scan_cache_for_rebuild =
            scan_cache::default_path().map(|path| scan_cache::ScanCache::load(&path));

        let progress_state = state.clone();
        let progress_app = app.clone();
        let publish_state = state.clone();
        let publish_app = app.clone();
        let cancel_state = state.clone();
        let outcome = history.rebuild_from_transcripts(
            move |done, total| {
                if done == 0 || done == total || done % 25 == 0 {
                    let status = HistoryRebuildSnapshot {
                        phase: HistoryRebuildPhase::Running,
                        done,
                        total,
                        ..Default::default()
                    };
                    progress_state.set_history_rebuild_status(status.clone());
                    let _ = progress_app.emit("history-rebuild-progress", &status);
                }
            },
            move |outcome| {
                let summary = SessionSummary::of(&outcome.stored.session);
                let displaced_summary = outcome
                    .displaced
                    .as_ref()
                    .map(|displaced| SessionSummary::of(&displaced.session));
                publish_state.publish_rebuilt_session(outcome);
                let _ = publish_app.emit("session-updated", &summary);
                if let Some(displaced_summary) = displaced_summary {
                    let _ = publish_app.emit("session-updated", &displaced_summary);
                }
            },
            move || {
                cancel_state
                    .rebuild_cancel_requested
                    .load(Ordering::Acquire)
            },
            scan_cache_for_rebuild.as_ref(),
        );

        let status = match outcome {
            Ok(result) => {
                let (file_size_after, vacuum_error) = if result.cancelled {
                    (None, None)
                } else {
                    let vacuuming = HistoryRebuildSnapshot {
                        phase: HistoryRebuildPhase::Vacuuming,
                        done: result.sessions_considered,
                        total: result.sessions_considered,
                        ..Default::default()
                    };
                    state.set_history_rebuild_status(vacuuming.clone());
                    let _ = app.emit("history-rebuild-progress", &vacuuming);
                    match history.vacuum() {
                        Ok(()) => (history.database_footprint().total_bytes(), None),
                        Err(error) => (None, Some(error.to_string())),
                    }
                };
                let phase = if vacuum_error.is_some() {
                    HistoryRebuildPhase::Failed
                } else if result.cancelled {
                    HistoryRebuildPhase::Cancelled
                } else {
                    HistoryRebuildPhase::Complete
                };
                HistoryRebuildSnapshot {
                    phase,
                    done: result.sessions_considered,
                    total: result.sessions_considered,
                    elapsed_ms: Some(started.elapsed().as_millis() as u64),
                    error: vacuum_error,
                    sessions_reparsed: Some(result.sessions_reparsed),
                    sessions_missing_transcript: Some(result.sessions_missing_transcript),
                    sessions_failed: Some(result.sessions_failed),
                    rate_limit_points_before: Some(result.before.rate_limit_points),
                    rate_limit_points_after: Some(result.after.rate_limit_points),
                    session_json_bytes_before: Some(result.before.session_json_bytes),
                    session_json_bytes_after: Some(result.after.session_json_bytes),
                    file_size_before: footprint_before.total_bytes(),
                    file_size_after,
                }
            }
            Err(error) => HistoryRebuildSnapshot {
                phase: HistoryRebuildPhase::Failed,
                elapsed_ms: Some(started.elapsed().as_millis() as u64),
                error: Some(error.to_string()),
                file_size_before: footprint_before.total_bytes(),
                ..Default::default()
            },
        };
        finish(status);
    });
}

/// Returns the rate card, preferring the user's on-disk copy over the bundled defaults.
#[tauri::command]
pub fn get_rates() -> RateCard {
    RateCard::load_from_disk().unwrap_or_else(|_| RateCard {
        version: 1,
        currency: "USD".into(),
        unit: "per_1m_tokens".into(),
        source_url: String::new(),
        fetched_at: None,
        models: std::collections::HashMap::new(),
        fallback_model: "codex-mini-latest".into(),
        currencies: std::collections::HashMap::new(),
        fallback_models: std::collections::HashMap::new(),
        api_models: std::collections::HashMap::new(),
        unpriced_models: Vec::new(),
        pricing_catalog: Default::default(),
        ..Default::default()
    })
}

/// Returns the bundled (shipped) rate card, ignoring any on-disk overrides.
/// Used by the "Reset to shipped defaults" button in the rates editor.
#[tauri::command]
pub fn get_bundled_rates() -> RateCard {
    RateCard::load_bundled().unwrap_or_else(|_| RateCard {
        version: 1,
        currency: "USD".into(),
        unit: "per_1m_tokens".into(),
        source_url: String::new(),
        fetched_at: None,
        models: std::collections::HashMap::new(),
        fallback_model: "codex-mini-latest".into(),
        currencies: std::collections::HashMap::new(),
        fallback_models: std::collections::HashMap::new(),
        api_models: std::collections::HashMap::new(),
        unpriced_models: Vec::new(),
        pricing_catalog: Default::default(),
        ..Default::default()
    })
}

/// Persists an updated rate card to disk and emits a rates-updated event so all
/// frontend subscribers can refresh their computed credits immediately.
#[tauri::command]
pub fn set_rates(app: tauri::AppHandle, rates: RateCard) -> Result<(), String> {
    rates.save().map_err(|e| e.to_string())?;
    app.emit("rates-updated", &rates)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Reveals the given file in the system file manager, highlighting it where
/// possible. macOS uses `open -R`; Windows uses `explorer /select,<file>`;
/// Linux falls back to opening the parent directory since `xdg-open` has no
/// portable file-select equivalent across desktop environments.
/// Errors are returned to the UI but treated as best-effort.
#[tauri::command]
pub fn reveal_in_file_manager(path: String) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let cmd = {
        let parent = std::path::Path::new(&path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        std::process::Command::new("xdg-open").arg(&parent).spawn()
    };

    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open")
        .arg("-R")
        .arg(&path)
        .spawn();

    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path))
        .spawn();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let cmd: Result<_, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));

    cmd.map(|_| ()).map_err(|e| e.to_string())
}

/// Opens a local task in the current ChatGPT desktop app through its retained
/// `codex://threads/<id>` compatibility deep link.
#[tauri::command]
pub fn open_task_in_chatgpt(session_id: String) -> Result<(), String> {
    if !valid_session_id(&session_id) {
        return Err("invalid session id".into());
    }
    let url = format!("codex://threads/{session_id}");

    #[cfg(target_os = "linux")]
    let cmd = std::process::Command::new("xdg-open").arg(&url).spawn();

    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg(&url).spawn();

    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("explorer").arg(&url).spawn();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let cmd: Result<_, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));

    cmd.map(|_| ()).map_err(|e| e.to_string())
}

fn valid_session_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(any(windows, test))]
fn powershell_encoded_path(path: &std::path::Path) -> String {
    #[cfg(windows)]
    let units = {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().collect::<Vec<_>>()
    };
    #[cfg(not(windows))]
    let units = path.to_string_lossy().encode_utf16().collect::<Vec<_>>();

    let bytes = units
        .into_iter()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    BASE64.encode(bytes)
}

/// PowerShell's `-EncodedCommand` expects a UTF-16LE Base64 payload.
#[cfg(any(windows, test))]
fn powershell_encoded_command(script: &str) -> String {
    let bytes = script
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    BASE64.encode(bytes)
}

#[cfg(any(windows, test))]
fn normalize_defender_root_key(path: &std::path::Path) -> String {
    let mut normalized = path.to_string_lossy().replace('/', "\\");
    while normalized.len() > 3 && normalized.ends_with('\\') {
        normalized.pop();
    }
    normalized.to_lowercase()
}

#[cfg(any(windows, test))]
fn configured_defender_roots(config: &Config) -> Vec<std::path::PathBuf> {
    let mut seen = HashSet::new();
    config
        .session_roots
        .iter()
        .chain(config.archive_roots.iter())
        .chain(config.claude_session_roots.iter())
        .filter(|path| !path.as_os_str().is_empty())
        .filter(|path| seen.insert(normalize_defender_root_key(path)))
        .cloned()
        .collect()
}

/// Accept only a descendant of a Windows drive or UNC share. This rejects
/// relative paths, drive roots, share roots, device paths, and lexical `..`
/// traversal back to those broad roots before an exclusion reaches elevation.
#[cfg(any(windows, test))]
fn defender_root_is_scoped(path: &std::path::Path) -> bool {
    let Some(value) = path.to_str() else {
        return false;
    };
    let normalized = value.replace('/', "\\");
    let lower = normalized.to_ascii_lowercase();

    let tail = if lower.starts_with(r"\\?\unc\") {
        let mut parts = normalized[8..].split('\\');
        let (Some(server), Some(share)) = (parts.next(), parts.next()) else {
            return false;
        };
        if server.is_empty() || share.is_empty() {
            return false;
        }
        parts.collect::<Vec<_>>().join("\\")
    } else if lower.starts_with(r"\\?\") {
        let drive_path = &normalized[4..];
        let bytes = drive_path.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || bytes[2] != b'\\'
        {
            return false;
        }
        drive_path[3..].to_owned()
    } else if lower.starts_with(r"\\.\") {
        return false;
    } else if let Some(unc_tail) = normalized.strip_prefix(r"\\") {
        let mut parts = unc_tail.split('\\');
        let (Some(server), Some(share)) = (parts.next(), parts.next()) else {
            return false;
        };
        if server.is_empty() || share.is_empty() {
            return false;
        }
        parts.collect::<Vec<_>>().join("\\")
    } else {
        let bytes = normalized.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || bytes[2] != b'\\'
        {
            return false;
        }
        normalized[3..].to_owned()
    };

    let mut depth = 0_usize;
    for segment in tail.split('\\') {
        match segment {
            "" | "." => {}
            ".." if depth == 0 => return false,
            ".." => depth -= 1,
            _ => depth += 1,
        }
    }
    depth > 0
}

#[cfg(windows)]
fn validate_defender_root_scope(path: &std::path::Path) -> Result<(), String> {
    if !defender_root_is_scoped(path) {
        return Err(format!(
            "Odometer only excludes absolute session subfolders, never a drive or network-share root: {}",
            path.display()
        ));
    }
    let resolved = std::fs::canonicalize(path).map_err(|error| {
        format!(
            "could not safely resolve the session folder {}: {error}",
            path.display()
        )
    })?;
    if !defender_root_is_scoped(&resolved) {
        return Err(format!(
            "Odometer will not exclude {} because it resolves to a drive or network-share root",
            path.display()
        ));
    }
    Ok(())
}

/// Builds the elevated script that adds the requested exclusions and then
/// verifies their effective behavior with Microsoft's documented
/// `MpCmdRun.exe -CheckExclusion` contract. The script never lists or exports
/// unrelated exclusions, which may be hidden by device policy.
#[cfg(any(windows, test))]
fn defender_verification_script(paths: &[std::path::PathBuf]) -> String {
    // Never interpolate filesystem paths as PowerShell literals. Windows
    // PowerShell treats several Unicode quote characters as delimiters, so
    // ordinary ASCII apostrophe escaping is insufficient across elevation.
    let path_values = paths
        .iter()
        .map(|path| {
            format!(
                "[Text.Encoding]::Unicode.GetString([Convert]::FromBase64String('{}'))",
                powershell_encoded_path(path)
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "$ErrorActionPreference = 'Stop'; \
         $paths = @({path_values}); \
         try {{ Add-MpPreference -ExclusionPath $paths -ErrorAction Stop }} catch {{ exit 1 }}; \
         try {{ \
           $programData = [Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData); \
           $programFiles = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFiles); \
           $platformRoot = Join-Path $programData 'Microsoft\\Windows Defender\\Platform'; \
           $platform = Get-ChildItem -LiteralPath $platformRoot -Directory -ErrorAction SilentlyContinue | Sort-Object Name -Descending | Where-Object {{ Test-Path -LiteralPath (Join-Path $_.FullName 'MpCmdRun.exe') -PathType Leaf }} | Select-Object -First 1; \
           $mp = if ($platform) {{ Join-Path $platform.FullName 'MpCmdRun.exe' }} else {{ $null }}; \
           if (-not $mp -or -not (Test-Path -LiteralPath $mp -PathType Leaf)) {{ $mp = Join-Path $programFiles 'Windows Defender\\MpCmdRun.exe' }}; \
           if (-not (Test-Path -LiteralPath $mp -PathType Leaf)) {{ exit 4 }} \
         }} catch {{ exit 4 }}; \
         foreach ($path in $paths) {{ \
           try {{ \
             $global:LASTEXITCODE = $null; \
             & $mp -CheckExclusion -Path $path *> $null; \
             $checkExitCode = $LASTEXITCODE \
           }} catch {{ exit 5 }}; \
           if ($checkExitCode -eq 1) {{ exit 3 }}; \
           if ($checkExitCode -ne 0) {{ exit 5 }} \
         }}; \
         exit 0"
    )
}

#[cfg(any(windows, test))]
fn defender_elevation_script(inner: &str) -> String {
    let encoded = powershell_encoded_command(inner);
    format!(
        "try {{ $powershell = Join-Path $PSHOME 'powershell.exe'; \
         $process = Start-Process -FilePath $powershell -Verb RunAs -WindowStyle Hidden \
         -ArgumentList @('-NoProfile','-NonInteractive','-EncodedCommand','{encoded}') \
         -Wait -PassThru -ErrorAction Stop; \
         exit $process.ExitCode }} catch {{ \
           $nativeError = $_.Exception.NativeErrorCode; \
           $hresult = $_.Exception.HResult; \
           if (-not $nativeError -and $_.Exception.InnerException) {{ $nativeError = $_.Exception.InnerException.NativeErrorCode }}; \
           if ($_.Exception.InnerException) {{ $hresult = $_.Exception.InnerException.HResult }}; \
           if ($nativeError -eq 1223 -or $hresult -eq -2147023673) {{ exit 2 }}; \
           exit 6 \
         }}"
    )
}

#[cfg(windows)]
fn windows_powershell_path() -> Result<std::path::PathBuf, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = vec![0_u16; 32_768];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) };
    if length == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    if length as usize >= buffer.len() {
        return Err("Windows system directory path is unexpectedly long".into());
    }
    let mut path = std::path::PathBuf::from(OsString::from_wide(&buffer[..length as usize]));
    path.push("WindowsPowerShell");
    path.push("v1.0");
    path.push("powershell.exe");
    Ok(path)
}

/// Opens Windows' UAC consent flow to add the configured session roots as
/// Windows Defender real-time-scanning path exclusions. Strictly opt-in from
/// the UI: the user clicks the button AND approves the elevation prompt, and
/// only the session-data directories are excluded — never the app itself.
/// Waits for the elevated process, verifies every existing root as effectively
/// excluded, and persists a point-in-time receipt. Opening Settings or starting
/// Odometer never triggers elevation or a Defender status query.
#[tauri::command]
pub async fn add_defender_exclusions(
    _app: AppHandle,
    _state: State<'_, Arc<AppState>>,
) -> Result<DefenderExclusionReceipt, String> {
    #[cfg(windows)]
    {
        let app_state = _state.inner().clone();
        let (configured_roots, existing_roots) = {
            let _transition = app_state.config_transition.lock().unwrap();
            let config = Config::load().map_err(|error| error.to_string())?;
            let configured_roots = configured_defender_roots(&config);
            let existing_roots = configured_roots
                .iter()
                .filter(|path| path.exists())
                .cloned()
                .collect::<Vec<_>>();
            (configured_roots, existing_roots)
        };
        if existing_roots.is_empty() {
            return Err("no existing session folders to exclude".into());
        }
        for root in &existing_roots {
            validate_defender_root_scope(root)?;
        }

        // Elevation happens through Start-Process -Verb RunAs, so Windows
        // itself asks the user for consent; nothing runs silently. The child
        // returns distinct codes for add, verification, and tool failures.
        let inner = defender_verification_script(&existing_roots);
        let outer = defender_elevation_script(&inner);
        let powershell = windows_powershell_path()?;

        let output = tauri::async_runtime::spawn_blocking(move || {
            use std::os::windows::process::CommandExt;

            let mut command = std::process::Command::new(powershell);
            command
                .args(["-NoProfile", "-NonInteractive", "-Command", &outer])
                .creation_flags(0x0800_0000);
            command.output()
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

        match output.status.code() {
            Some(0) => {}
            Some(1) => {
                return Err(
                    "Windows Defender did not accept the exclusions. Another security product or \
                     a device policy may be managing them."
                        .into(),
                );
            }
            Some(2) => {
                return Err(
                    "The Windows security prompt was declined — nothing was changed.".into(),
                );
            }
            Some(3) => {
                return Err(
                    "Windows accepted the request, but one or more session folders are not \
                     effectively excluded. Tamper protection or device policy may be managing \
                     Defender."
                        .into(),
                );
            }
            Some(4) => {
                return Err(
                    "Windows accepted the request, but Odometer could not find Microsoft's \
                     exclusion verification tool. No verification status was saved."
                        .into(),
                );
            }
            Some(5) => {
                return Err(
                    "Windows accepted the request, but Microsoft's exclusion verification tool \
                     did not complete successfully. No verification status was saved."
                        .into(),
                );
            }
            Some(6) => {
                return Err(
                    "Odometer could not start or wait for the elevated Windows Defender action. \
                     Its outcome could not be confirmed, so no verification status was saved."
                        .into(),
                );
            }
            _ => return Err("The Windows Defender exclusion request did not complete.".into()),
        }

        let receipt = DefenderExclusionReceipt {
            version: DEFENDER_EXCLUSION_RECEIPT_VERSION,
            configured_roots,
            verified_roots: existing_roots,
            verified_at: Utc::now(),
        };
        let updated_config = {
            let _transition = app_state.config_transition.lock().unwrap();
            let mut config = Config::load().map_err(|error| error.to_string())?;
            config.defender_exclusion_receipt = Some(receipt.clone());
            config.save().map_err(|error| error.to_string())?;
            config
        };
        if let Err(error) = _app.emit("config-updated", &updated_config) {
            tracing::warn!("could not emit Defender config update: {}", error);
        }
        Ok(receipt)
    }
    #[cfg(not(windows))]
    {
        Err("Defender exclusions are only applicable on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        configured_defender_roots, defender_elevation_script, defender_root_is_scoped,
        defender_verification_script, newest_subscription_usage_by_harness,
        powershell_encoded_command, powershell_encoded_path, preserve_backend_owned_config,
        range_has_data, resolve_projects_from, shorten_display_path, valid_session_id,
        working_directory_info, write_export_file, PerformanceLiveStatus,
    };
    use crate::config::{Config, DefenderExclusionReceipt, DEFENDER_EXCLUSION_RECEIPT_VERSION};
    use crate::model::{
        Harness, RangeTotals, RateLimitSnapshotPoint, RateLimitWindow, Session, SessionSummary,
        SourceAvailability, TokenTotals, ToolMetrics,
    };
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};

    /// Path literals in these tests use the platform separator, since the
    /// display form deliberately keeps it.
    fn sep(value: &str) -> String {
        if cfg!(windows) {
            value.replace('/', "\\")
        } else {
            value.to_string()
        }
    }

    /// Creates a minimal but real repository `gix::discover` will accept.
    fn init_repository(root: &Path) {
        let git = root.join(".git");
        std::fs::create_dir_all(git.join("objects")).unwrap();
        std::fs::create_dir_all(git.join("refs")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(
            git.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        )
        .unwrap();
    }

    #[test]
    fn display_path_collapses_home_and_keeps_the_trailing_segments() {
        let home = PathBuf::from(sep("/users/dev"));

        assert_eq!(
            shorten_display_path(
                Path::new(&sep("/users/dev/projects/odometer")),
                Some(&home),
                3
            ),
            sep("~/projects/odometer")
        );
        // Home itself stays addressable rather than rendering as empty.
        assert_eq!(shorten_display_path(&home, Some(&home), 3), "~");
        // Deeper than the segment budget: keep the tail, mark the elision.
        assert_eq!(
            shorten_display_path(
                Path::new(&sep("/users/dev/documents/codex/2026-08-04/ser")),
                Some(&home),
                3
            ),
            sep("…/codex/2026-08-04/ser")
        );
    }

    #[test]
    fn display_path_outside_home_stays_absolute() {
        let home = PathBuf::from(sep("/users/dev"));

        assert_eq!(
            shorten_display_path(Path::new(&sep("/data/work")), Some(&home), 3),
            sep("/data/work")
        );
        assert_eq!(
            shorten_display_path(Path::new(&sep("/data/work")), None, 3),
            sep("/data/work")
        );
    }

    /// The case that motivated this: a scratch directory whose final segment
    /// identifies nothing must not be presented as a repository name.
    #[test]
    fn a_directory_outside_any_repository_reports_no_repository() {
        let directory = tempfile::tempdir().unwrap();
        let scratch = directory.path().join("2026-08-04").join("ser");
        std::fs::create_dir_all(&scratch).unwrap();

        let info = working_directory_info(&scratch.to_string_lossy(), None);

        assert_eq!(info.repository_name, None);
        assert_eq!(info.relative_path, None);
        assert!(info.display_path.ends_with("ser"), "{}", info.display_path);
    }

    #[test]
    fn a_directory_inside_a_repository_reports_its_root_and_relative_location() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("my-repo");
        let nested = root.join("src").join("lib");
        std::fs::create_dir_all(&nested).unwrap();
        init_repository(&root);

        let at_root = working_directory_info(&root.to_string_lossy(), None);
        assert_eq!(at_root.repository_name.as_deref(), Some("my-repo"));
        // Empty, not absent: the directory *is* the repository root.
        assert_eq!(at_root.relative_path.as_deref(), Some(""));

        let inside = working_directory_info(&nested.to_string_lossy(), None);
        assert_eq!(inside.repository_name.as_deref(), Some("my-repo"));
        assert_eq!(inside.relative_path.as_deref(), Some("src/lib"));
    }
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use chrono::{DateTime, Utc};

    #[test]
    fn validates_deep_link_session_ids() {
        assert!(valid_session_id("019f5d3b-6b2f-75f1-aed9-723e7c488e66"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("task/id"));
        assert!(!valid_session_id("task?id=1"));
    }

    #[test]
    fn export_writer_accepts_only_csv_and_json() {
        let dir = tempfile::tempdir().unwrap();
        let csv = dir.path().join("usage.csv");
        let unicode = "name,note\r\n\"Δelta\",\"comma, quote \"\" and\nnewline\"\r\n";
        write_export_file(&csv, "csv", unicode).unwrap();
        assert_eq!(std::fs::read_to_string(csv).unwrap(), unicode);
        let json = dir.path().join("empty.json");
        write_export_file(&json, "json", "[]\n").unwrap();
        assert_eq!(std::fs::read_to_string(json).unwrap(), "[]\n");
        let text = dir.path().join("usage.txt");
        assert!(write_export_file(&text, "csv", "nope").is_err());
        let missing_parent = dir.path().join("missing/usage.csv");
        assert!(write_export_file(&missing_parent, "csv", "a\r\n").is_err());
    }

    #[test]
    fn range_filter_keeps_tool_only_observations() {
        let range = RangeTotals {
            tokens: TokenTotals::default(),
            buckets: Vec::new(),
            tool_metrics: ToolMetrics {
                calls: 1,
                ..Default::default()
            },
            tool_metrics_by_model: Default::default(),
            optimization_findings_count: 0,
            optimization_summary: Default::default(),
            tool_dimensions: Default::default(),
        };
        assert!(range_has_data(&range));
    }

    #[test]
    fn defender_roots_are_stable_and_case_insensitively_deduplicated() {
        let config = Config {
            session_roots: vec![PathBuf::from(r"C:\Users\dev\.codex\sessions")],
            archive_roots: vec![
                PathBuf::from(r"c:/users/dev/.codex/sessions/"),
                PathBuf::from(r"C:\Users\dev\.codex\archived_sessions"),
            ],
            claude_session_roots: vec![PathBuf::from(r"C:\Users\dev\.claude\projects")],
            ..Default::default()
        };

        assert_eq!(
            configured_defender_roots(&config),
            vec![
                PathBuf::from(r"C:\Users\dev\.codex\sessions"),
                PathBuf::from(r"C:\Users\dev\.codex\archived_sessions"),
                PathBuf::from(r"C:\Users\dev\.claude\projects"),
            ]
        );
    }

    #[test]
    fn defender_scope_rejects_relative_drive_and_share_roots() {
        assert!(defender_root_is_scoped(std::path::Path::new(
            r"C:\sessions"
        )));
        assert!(defender_root_is_scoped(std::path::Path::new(
            r"\\server\share\sessions"
        )));
        assert!(defender_root_is_scoped(std::path::Path::new(
            r"\\?\C:\sessions"
        )));
        assert!(!defender_root_is_scoped(std::path::Path::new("sessions")));
        assert!(!defender_root_is_scoped(std::path::Path::new(
            r"C:sessions"
        )));
        assert!(!defender_root_is_scoped(std::path::Path::new(r"C:\")));
        assert!(!defender_root_is_scoped(std::path::Path::new(
            r"C:\logs\.."
        )));
        assert!(!defender_root_is_scoped(std::path::Path::new(
            r"\\server\share"
        )));
        assert!(!defender_root_is_scoped(std::path::Path::new(
            r"\\?\UNC\server\share"
        )));
        assert!(!defender_root_is_scoped(std::path::Path::new(
            r"\\.\C:\sessions"
        )));
    }

    #[test]
    fn defender_encodings_round_trip_exact_unicode() {
        fn decode_utf16(encoded: &str) -> String {
            let bytes = BASE64.decode(encoded).unwrap();
            assert_eq!(bytes.len() % 2, 0);
            let units = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|chunk| u16::from_le_bytes(*chunk))
                .collect::<Vec<_>>();
            String::from_utf16(&units).unwrap()
        }

        let path = PathBuf::from("C:\\O\u{2019}Brien\\emoji-\u{1f680}");
        assert_eq!(
            decode_utf16(&powershell_encoded_path(&path)),
            path.to_str().unwrap()
        );
        let command = "$value = '\u{2019}\u{1f680}'; exit 0";
        assert_eq!(decode_utf16(&powershell_encoded_command(command)), command);
    }

    #[test]
    fn defender_script_treats_paths_as_data_and_checks_effective_exclusions() {
        let paths = vec![
            PathBuf::from(r"C:\Agent sessions"),
            PathBuf::from(r"C:\O'Brien\$(not-code);still-a-path"),
            PathBuf::from("C:\\proof\u{2019}); exit 77; #"),
        ];
        let inner = defender_verification_script(&paths);
        assert!(inner.contains("[Convert]::FromBase64String"));
        assert!(!inner.contains("O'Brien"));
        assert!(!inner.contains("exit 77"));
        assert!(inner.contains("Add-MpPreference -ExclusionPath $paths -ErrorAction Stop"));
        assert!(inner.contains("MpCmdRun.exe"));
        assert!(inner.contains("Where-Object { Test-Path"));
        assert!(inner.contains("-CheckExclusion -Path $path"));
        assert!(inner.contains("$global:LASTEXITCODE = $null"));
        assert!(inner.contains("if ($checkExitCode -eq 1) { exit 3 }"));
        assert!(inner.contains("if ($checkExitCode -ne 0) { exit 5 }"));
        assert!(!inner.contains("$env:ProgramData"));
        assert!(!inner.contains("$env:ProgramFiles"));

        let outer = defender_elevation_script(&inner);
        assert!(outer.contains("-Verb RunAs"));
        assert!(outer.contains("-WindowStyle Hidden"));
        assert!(outer.contains("-FilePath $powershell"));
        assert!(outer.contains("-EncodedCommand"));
        assert!(outer.contains("-Wait -PassThru"));
        assert!(outer.contains("$nativeError -eq 1223"));
        assert!(outer.contains("exit 6"));
        assert!(!outer.contains("Add-MpPreference"));
    }

    #[test]
    fn ordinary_config_updates_preserve_backend_defender_receipt() {
        let receipt = DefenderExclusionReceipt {
            version: DEFENDER_EXCLUSION_RECEIPT_VERSION,
            configured_roots: vec![PathBuf::from(r"C:\sessions")],
            verified_roots: vec![PathBuf::from(r"C:\sessions")],
            verified_at: DateTime::parse_from_rfc3339("2026-07-29T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        };
        let previous = Config {
            defender_exclusion_receipt: Some(receipt.clone()),
            ..Default::default()
        };
        let mut incoming = Config {
            performance_tracking_enabled: true,
            ..Default::default()
        };

        preserve_backend_owned_config(&previous, &mut incoming);

        assert_eq!(incoming.defender_exclusion_receipt, Some(receipt));
        assert!(incoming.performance_tracking_enabled);
    }

    fn subscription_fixture_session(
        id: &str,
        harness: Harness,
        plan_type: Option<&str>,
        credits_unlimited: Option<bool>,
        credits_balance: Option<f64>,
        rate_limits_history: Vec<RateLimitSnapshotPoint>,
    ) -> Session {
        let timestamp: DateTime<Utc> = "2026-07-29T09:00:00Z".parse().unwrap();
        Session {
            id: id.into(),
            storage_id: format!("{harness:?}:{id}"),
            harness,
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: format!("{id}.jsonl"),
            source_availability: SourceAvailability::Present,
            archived: false,
            started_at: timestamp,
            last_event_at: timestamp,
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
            plan_type: plan_type.map(str::to_owned),
            credits_unlimited,
            credits_balance,
            context_window: None,
            latest_context_tokens: None,
            total_turns: 0,
            first_user_message: None,
            tokens_total: TokenTotals::default(),
            tokens_by_model: Default::default(),
            tokens_history: Vec::new(),
            rate_limits_history,
            turns: Vec::new(),
            tool_observations: Vec::new(),
            tool_metrics: ToolMetrics::default(),
            tool_metrics_by_model: Default::default(),
            category_totals: Default::default(),
            optimization_findings: Vec::new(),
            project_key: None,
            project_label: None,
            project_provenance: None,
        }
    }

    #[test]
    fn subscription_usage_picks_newest_snapshot_per_harness_and_omits_silent_harnesses() {
        let older: DateTime<Utc> = "2026-07-29T10:00:00Z".parse().unwrap();
        let newer: DateTime<Utc> = "2026-07-29T12:00:00Z".parse().unwrap();

        let codex_a = subscription_fixture_session(
            "codex-a",
            crate::provider::codex_provider_id(),
            Some("pro"),
            Some(false),
            Some(12.5),
            vec![RateLimitSnapshotPoint {
                timestamp: older,
                turn_id: None,
                limit_id: None,
                primary: Some(RateLimitWindow {
                    used_percent: 40.0,
                    window_minutes: Some(300),
                    resets_at: None,
                }),
                secondary: None,
                run_started_at: None,
                observation_count: 1,
            }],
        );
        // Second Codex session reports a newer snapshot with different
        // account fields — its plan/credits must win, not codex_a's.
        let codex_b = subscription_fixture_session(
            "codex-b",
            crate::provider::codex_provider_id(),
            Some("plus"),
            Some(true),
            None,
            vec![RateLimitSnapshotPoint {
                timestamp: newer,
                turn_id: None,
                limit_id: None,
                primary: Some(RateLimitWindow {
                    used_percent: 63.0,
                    window_minutes: Some(300),
                    resets_at: Some(newer),
                }),
                secondary: Some(RateLimitWindow {
                    used_percent: 10.0,
                    window_minutes: Some(10_080),
                    resets_at: None,
                }),
                run_started_at: None,
                observation_count: 1,
            }],
        );
        // Claude Code session with no rate-limit history: harness must not
        // appear in the result at all.
        let claude = subscription_fixture_session(
            "claude-a",
            crate::provider::claude_code_provider_id(),
            None,
            None,
            None,
            Vec::new(),
        );

        let sessions: Vec<crate::model::ResidentSession> = [codex_a, codex_b, claude]
            .iter()
            .map(crate::model::ResidentSession::of)
            .collect();
        let result = newest_subscription_usage_by_harness(sessions.iter());

        assert_eq!(result.len(), 1);
        let entry = &result[0];
        assert_eq!(entry.harness, crate::provider::codex_provider_id());
        assert_eq!(entry.captured_at, newer);
        assert_eq!(entry.plan_type.as_deref(), Some("plus"));
        assert_eq!(entry.credits_unlimited, Some(true));
        assert_eq!(entry.credits_balance, None);
        assert_eq!(entry.primary.as_ref().unwrap().used_percent, 63.0);
        assert_eq!(
            entry.secondary.as_ref().unwrap().window_minutes,
            Some(10_080)
        );
    }

    fn project_fixture_session(
        id: &str,
        project_key: Option<&str>,
        project_label: Option<&str>,
    ) -> SessionSummary {
        let mut session = subscription_fixture_session(
            id,
            crate::provider::codex_provider_id(),
            None,
            None,
            None,
            Vec::new(),
        );
        session.storage_id = format!("codex:thread:{id}");
        session.project_key = project_key.map(str::to_owned);
        session.project_label = project_label.map(str::to_owned);
        session.project_provenance = project_key
            .is_some()
            .then_some(crate::project_identity::ProjectProvenance::RepositoryRoot);
        SessionSummary::of(&session)
    }

    #[test]
    fn resolve_projects_groups_sessions_and_omits_sessions_without_a_project() {
        let sessions = [
            project_fixture_session("a1", Some("repo:a"), Some("alpha")),
            project_fixture_session("a2", Some("repo:a"), Some("alpha")),
            project_fixture_session("b1", Some("repo:b"), Some("beta")),
            project_fixture_session("no-project", None, None),
        ];
        let projects = resolve_projects_from(sessions.iter(), &HashMap::new(), &HashMap::new());
        assert_eq!(projects.len(), 2);
        let alpha = projects.iter().find(|p| p.project_key == "repo:a").unwrap();
        assert_eq!(alpha.label, "alpha");
        assert_eq!(alpha.session_count, 2);
        assert_eq!(alpha.member_keys, vec!["repo:a".to_string()]);
        let beta = projects.iter().find(|p| p.project_key == "repo:b").unwrap();
        assert_eq!(beta.session_count, 1);

        // Reconciliation: every session with a project is accounted for
        // exactly once across the resolved projects, and the session with no
        // project is neither silently dropped nor silently grouped in.
        let total_grouped: usize = projects.iter().map(|p| p.session_count).sum();
        let with_project = sessions.iter().filter(|s| s.project_key.is_some()).count();
        assert_eq!(total_grouped, with_project);
        assert_eq!(with_project, sessions.len() - 1);
    }

    #[test]
    fn resolve_projects_applies_merge_and_alias_overrides_and_reconciles_session_counts() {
        let sessions = [
            project_fixture_session("a1", Some("repo:a"), Some("alpha")),
            project_fixture_session("b1", Some("repo:b"), Some("beta")),
            project_fixture_session("b2", Some("repo:b"), Some("beta")),
        ];
        let total_sessions = sessions.len();
        let mut project_overrides = HashMap::new();
        project_overrides.insert(
            "repo:a".to_string(),
            crate::history_store::ProjectOverrideRow {
                project_key: "repo:a".into(),
                display_label: None,
                canonical_project_key: Some("repo:b".into()),
            },
        );
        project_overrides.insert(
            "repo:b".to_string(),
            crate::history_store::ProjectOverrideRow {
                project_key: "repo:b".into(),
                display_label: Some("Beta (renamed)".into()),
                canonical_project_key: None,
            },
        );
        let projects = resolve_projects_from(sessions.iter(), &HashMap::new(), &project_overrides);
        assert_eq!(projects.len(), 1, "a merged into b leaves one project");
        let merged = &projects[0];
        assert_eq!(merged.project_key, "repo:b");
        assert_eq!(merged.label, "Beta (renamed)");
        assert_eq!(merged.session_count, total_sessions);
        assert_eq!(
            merged.member_keys,
            vec!["repo:a".to_string(), "repo:b".to_string()]
        );
    }

    #[test]
    fn resolve_projects_honors_a_manual_session_split_override() {
        let sessions = [
            project_fixture_session("a1", Some("repo:a"), Some("alpha")),
            project_fixture_session("a2", Some("repo:a"), Some("alpha")),
        ];
        let mut session_overrides = HashMap::new();
        session_overrides.insert(
            "codex:thread:a1".to_string(),
            "manual:standalone".to_string(),
        );
        let projects = resolve_projects_from(sessions.iter(), &session_overrides, &HashMap::new());
        assert_eq!(projects.len(), 2);
        let standalone = projects
            .iter()
            .find(|p| p.project_key == "manual:standalone")
            .unwrap();
        assert_eq!(standalone.session_count, 1);
        let remaining = projects.iter().find(|p| p.project_key == "repo:a").unwrap();
        assert_eq!(remaining.session_count, 1);
    }

    /// `PerformanceLiveStatus` wraps `MemoryLiveStatus` behind
    /// `#[serde(flatten)]` (issue #163) so the frontend sees one flat object
    /// rather than a nested `memory` key; `src/lib/types.ts`'s
    /// `PerformanceLiveStatus` interface is written against that flattened
    /// shape, so this pins the wire contract the frontend mock in
    /// `DiagnosticsPanel.test.ts` cannot itself catch a drift in.
    #[test]
    fn performance_live_status_flattens_memory_fields_to_the_top_level() {
        let status = PerformanceLiveStatus {
            memory: crate::memory::MemoryLiveStatus {
                enabled: true,
                active_phase: Some("bulk_scan_parallel".to_string()),
                ..Default::default()
            },
            recent_operations: Vec::new(),
        };
        let json = serde_json::to_value(&status).unwrap();
        let object = json.as_object().unwrap();
        assert!(
            !object.contains_key("memory"),
            "flatten must not leave a nested \"memory\" key: {object:?}"
        );
        assert_eq!(object["enabled"], true);
        assert_eq!(object["active_phase"], "bulk_scan_parallel");
        assert!(object.contains_key("recent_operations"));
    }
}

/// How one session working directory should be labelled in the grid.
///
/// A working directory is not necessarily a repository: scratch directories
/// (for example the dated folders a web client creates) have no repo at all,
/// and their final path segment is frequently a one- or two-character
/// fragment that identifies nothing. Rather than present those as repository
/// names, the grid shows a shortened path for every directory and marks the
/// ones that are genuinely inside a working tree.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkingDirectoryInfo {
    /// The session's working directory, verbatim — the map key the frontend
    /// looks up.
    pub directory: String,
    /// Working-tree root name when inside a repository.
    pub repository_name: Option<String>,
    /// Directory location relative to the repository root. Empty at the root
    /// itself. Only meaningful alongside `repository_name`.
    pub relative_path: Option<String>,
    /// Shortened absolute path, home collapsed to `~`. Shown when there is no
    /// repository, and as the hover title when there is.
    pub display_path: String,
}

/// Collapses the home prefix to `~` and keeps only the trailing segments, so a
/// deep path stays identifiable without carrying the whole ancestry into the
/// grid.
pub(crate) fn shorten_display_path(
    path: &Path,
    home: Option<&Path>,
    max_segments: usize,
) -> String {
    let separator = if cfg!(windows) { '\\' } else { '/' };
    let (mut rendered, prefixed) = match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => (String::from("~"), true),
        Some(rest) => (format!("~{separator}{}", rest.display()), true),
        None => (path.display().to_string(), false),
    };
    let segments: Vec<&str> = rendered
        .split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect();
    if segments.len() > max_segments {
        let kept = segments[segments.len() - max_segments..].join(&separator.to_string());
        rendered = format!("…{separator}{kept}");
    } else if !prefixed && rendered.is_empty() {
        rendered = path.display().to_string();
    }
    rendered
}

fn working_directory_info(directory: &str, home: Option<&Path>) -> WorkingDirectoryInfo {
    let path = Path::new(directory);
    let display_path = shorten_display_path(path, home, 3);
    match crate::git_outcomes::discover_repository_root(path) {
        Some(root) => {
            let repository_name = root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                // A repository at a filesystem root has no final segment.
                .unwrap_or_else(|| root.display().to_string());
            let relative_path = path
                .strip_prefix(&root)
                .ok()
                .map(|rest| rest.to_string_lossy().replace('\\', "/"));
            WorkingDirectoryInfo {
                directory: directory.to_string(),
                repository_name: Some(repository_name),
                relative_path,
                display_path,
            }
        }
        None => WorkingDirectoryInfo {
            directory: directory.to_string(),
            repository_name: None,
            relative_path: None,
            display_path,
        },
    }
}

/// Resolves every distinct session working directory to its repository, if any.
///
/// Distinct directories are far fewer than sessions (roughly 30× fewer in
/// practice), so this walks the filesystem once per directory rather than once
/// per session. Deliberately uncached: a directory that becomes a repository
/// after `git init` should be reflected without restarting the app.
#[tauri::command]
pub fn resolve_working_directories(state: State<'_, Arc<AppState>>) -> Vec<WorkingDirectoryInfo> {
    let started = Instant::now();
    let home = dirs::home_dir();
    let mut directories: Vec<String> = state
        .sessions
        .iter()
        .filter_map(|entry| entry.summary.working_directory.clone())
        .collect();
    directories.sort_unstable();
    directories.dedup();
    let resolved: Vec<WorkingDirectoryInfo> = directories
        .iter()
        .map(|directory| working_directory_info(directory, home.as_deref()))
        .collect();
    let mut metadata = BTreeMap::new();
    metadata.insert("directories".into(), resolved.len().to_string());
    metadata.insert(
        "repositories".into(),
        resolved
            .iter()
            .filter(|info| info.repository_name.is_some())
            .count()
            .to_string(),
    );
    state
        .performance
        .record_backend("ipc.resolve_working_directories", started, true, metadata);
    resolved
}

/// One resolved project (#41): the effective identity after local
/// alias/merge overrides, the sessions it currently contains, and the
/// auto-computed keys folded into it (more than one only when a user has
/// merged projects together). This is the one backend aggregation path for
/// the project dimension — cards, tables, exports, and any future tray/CLI
/// consumer group sessions by `project_key` using this same map rather than
/// recomputing grouping on the frontend.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProjectInfo {
    pub project_key: String,
    pub label: String,
    pub provenance: crate::project_identity::ProjectProvenance,
    pub member_keys: Vec<String>,
    pub session_count: usize,
}

/// Pure grouping logic behind [`resolve_projects`], separated out so it is
/// testable without a Tauri `State`. `session_overrides` maps a durable
/// session key to a manually reassigned raw project key ("split", #41);
/// `project_overrides` is every alias/merge row keyed by raw project key.
/// Takes `&SessionSummary` rather than `&Session` (issue #139):
/// `project_key`/`project_label`/`project_provenance`/`storage_id` are all
/// already summary-shaped fields, so this never needs a ledger read.
fn resolve_projects_from<'a>(
    sessions: impl Iterator<Item = &'a SessionSummary>,
    session_overrides: &HashMap<String, String>,
    project_overrides: &HashMap<String, crate::history_store::ProjectOverrideRow>,
) -> Vec<ProjectInfo> {
    struct RawInfo {
        label: String,
        provenance: crate::project_identity::ProjectProvenance,
        count: usize,
    }
    let mut raw: BTreeMap<String, RawInfo> = BTreeMap::new();
    for session in sessions {
        let effective_raw = session_overrides
            .get(&session.storage_id)
            .cloned()
            .or_else(|| session.project_key.clone());
        let Some(raw_key) = effective_raw else {
            continue;
        };
        let record = raw.entry(raw_key).or_insert_with(|| RawInfo {
            label: session.project_label.clone().unwrap_or_default(),
            provenance: session
                .project_provenance
                .unwrap_or(crate::project_identity::ProjectProvenance::FallbackPathIdentity),
            count: 0,
        });
        record.count += 1;
    }

    let mut grouped: HashMap<String, ProjectInfo> = HashMap::new();
    for (raw_key, info) in &raw {
        let canonical =
            crate::history_store::resolve_canonical_project_key(project_overrides, raw_key);
        let target = grouped
            .entry(canonical.clone())
            .or_insert_with(|| ProjectInfo {
                project_key: canonical.clone(),
                label: String::new(),
                provenance: info.provenance,
                member_keys: Vec::new(),
                session_count: 0,
            });
        target.member_keys.push(raw_key.clone());
        target.session_count += info.count;
        if *raw_key == canonical {
            target.label = info.label.clone();
            target.provenance = info.provenance;
        }
    }
    for project in grouped.values_mut() {
        if let Some(alias) = project_overrides
            .get(&project.project_key)
            .and_then(|row| row.display_label.clone())
        {
            project.label = alias;
        } else if project.label.is_empty() {
            // The canonical key was chosen as a merge target that no longer
            // (or never did) label itself directly — fall back to the
            // lexicographically-first member's auto label, so the row is
            // never displayed with a raw key string.
            if let Some(first) = project.member_keys.first() {
                if let Some(info) = raw.get(first) {
                    project.label = info.label.clone();
                }
            }
        }
    }
    let mut result: Vec<ProjectInfo> = grouped.into_values().collect();
    result.sort_by(|a, b| {
        a.label
            .cmp(&b.label)
            .then_with(|| a.project_key.cmp(&b.project_key))
    });
    result
}

/// Every resolved project, after local alias/merge/split overrides. Reuses
/// the same auto-computed `project_key`/`project_label`/`project_provenance`
/// already carried on each in-memory session (populated durably by
/// `history_store::apply_project_identity`) — this command never re-walks
/// the filesystem itself, matching `resolve_working_directories`' pattern of
/// a cheap, fetch-once-until-refreshed map the frontend joins against by key.
#[tauri::command]
pub fn resolve_projects(state: State<'_, Arc<AppState>>) -> Result<Vec<ProjectInfo>, String> {
    let started = Instant::now();
    let session_overrides = match state.history_ready() {
        Some(history) => history
            .list_session_project_overrides()
            .map_err(|error| error.to_string())?,
        None => HashMap::new(),
    };
    let project_overrides: HashMap<String, crate::history_store::ProjectOverrideRow> =
        match state.history_ready() {
            Some(history) => history
                .list_project_overrides()
                .map_err(|error| error.to_string())?
                .into_iter()
                .map(|row| (row.project_key.clone(), row))
                .collect(),
            None => HashMap::new(),
        };
    let sessions: Vec<Arc<crate::model::ResidentSession>> = state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    let result = resolve_projects_from(
        sessions.iter().map(|session| &session.summary),
        &session_overrides,
        &project_overrides,
    );
    let mut metadata = BTreeMap::new();
    metadata.insert("projects".into(), result.len().to_string());
    metadata.insert("sessions".into(), sessions.len().to_string());
    state
        .performance
        .record_backend("ipc.resolve_projects", started, true, metadata);
    Ok(result)
}

/// Sets (`Some`) or clears (`None`) a local display-label alias for a
/// project. Never rewrites a source transcript or the auto-computed label;
/// reversible by calling again with `None`.
#[tauri::command]
pub fn set_project_alias(
    state: State<'_, Arc<AppState>>,
    project_key: String,
    display_label: Option<String>,
) -> Result<(), String> {
    let history = state
        .history_ready()
        .ok_or("the history store is unavailable")?;
    history
        .set_project_alias(&project_key, display_label.as_deref())
        .map_err(|error| error.to_string())
}

/// Merges `source_project_key` to display under `canonical_project_key`.
/// Rejects a self-merge or a merge that would create a cycle. Reversible via
/// [`unmerge_project`].
#[tauri::command]
pub fn merge_projects(
    state: State<'_, Arc<AppState>>,
    source_project_key: String,
    canonical_project_key: String,
) -> Result<(), String> {
    let history = state
        .history_ready()
        .ok_or("the history store is unavailable")?;
    history
        .merge_project(&source_project_key, &canonical_project_key)
        .map_err(|error| error.to_string())
}

/// Removes a project's merge redirect (its alias, if any, is preserved).
#[tauri::command]
pub fn unmerge_project(state: State<'_, Arc<AppState>>, project_key: String) -> Result<(), String> {
    let history = state
        .history_ready()
        .ok_or("the history store is unavailable")?;
    history
        .unmerge_project(&project_key)
        .map_err(|error| error.to_string())
}

/// Manually reassigns one session to `project_key` (or, when `None`, splits
/// it into a freshly minted standalone project), overriding whatever it
/// would otherwise be auto-grouped or merged into. Returns the effective
/// project key applied. Reversible via [`clear_session_project_override`].
#[tauri::command]
pub fn reassign_session_project(
    state: State<'_, Arc<AppState>>,
    session_key: String,
    project_key: Option<String>,
) -> Result<String, String> {
    let history = state
        .history_ready()
        .ok_or("the history store is unavailable")?;
    history
        .reassign_session_project(&session_key, project_key.as_deref())
        .map_err(|error| error.to_string())
}

/// Reverts a manual session project reassignment back to auto-computed
/// grouping.
#[tauri::command]
pub fn clear_session_project_override(
    state: State<'_, Arc<AppState>>,
    session_key: String,
) -> Result<(), String> {
    let history = state
        .history_ready()
        .ok_or("the history store is unavailable")?;
    history
        .clear_session_project_override(&session_key)
        .map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Quota (issue #43): live-window snapshots, soft budgets, and alerts.
// `crate::quota` is the single service computing pace/forecast/alert math;
// these commands only assemble its inputs from `AppState`/`quota_store` and
// return its output. See `crate::quota`'s module docs for the honesty
// contract and the network seam this deliberately does not implement.
// ---------------------------------------------------------------------------

/// Every registered provider's current quota snapshot, transcript-derived.
/// Always returns one entry per provider (never omits one), so "no data"
/// is always an explicit, honest state rather than an absent row.
#[tauri::command]
pub fn get_quota_snapshots(state: State<'_, Arc<AppState>>) -> Vec<crate::quota::QuotaSnapshot> {
    let started = Instant::now();
    let store = state.quota_store();
    let now = Utc::now();
    let max_cache_age = chrono::Duration::seconds(store.max_cache_age_secs);
    let result = state.quota_snapshots(max_cache_age, now);
    state.performance.record_backend(
        "ipc.get_quota_snapshots",
        started,
        true,
        BTreeMap::from([("providers".into(), result.len().to_string())]),
    );
    result
}

/// Current soft-budget/notification configuration (never includes the
/// internal dedup log — see `QuotaConfigWire`).
#[tauri::command]
pub fn get_quota_config() -> crate::quota_store::QuotaConfigWire {
    let store = crate::quota_store::QuotaStoreFile::load();
    crate::quota_store::QuotaConfigWire::from(&store)
}

/// Replaces the whole soft-budget/notification configuration, mirroring
/// `set_rates`'s whole-object-replace shape. Validates before persisting
/// (fail-closed) and preserves the existing notification dedup log, which
/// is internal bookkeeping the caller never sees or supplies.
#[tauri::command]
pub fn set_quota_config(
    state: State<'_, Arc<AppState>>,
    config: crate::quota_store::QuotaConfigWire,
) -> Result<crate::quota_store::QuotaConfigWire, String> {
    crate::quota_store::validate_quota_config(&config)?;
    let mut store = crate::quota_store::QuotaStoreFile::load();
    store.budgets = config.budgets;
    store.notifications = config.notifications;
    store.max_cache_age_secs = config.max_cache_age_secs;
    store.save().map_err(|error| error.to_string())?;
    let wire = crate::quota_store::QuotaConfigWire::from(&store);
    // Issue #128: `get_quota_snapshots` no longer reloads this file per
    // call, so the in-memory cache must be brought current here, at the
    // one write path — and the (small, one-per-provider) snapshot cache
    // invalidated, since `max_cache_age_secs` feeds it directly.
    state.set_quota_store(store);
    Ok(wire)
}

/// Token usage for one budget's provider/project scope over its rolling
/// period, from the in-memory session projection. Only ever called for
/// `Tokens`-unit budgets; `PercentOfWindow` budgets read their current
/// value directly off the matching `QuotaWindow` instead.
/// `None` means the value could not be computed this poll (issue #139: full
/// `tokens_history` for this budget's scoped sessions is a possible ledger
/// read now that `state.sessions` holds only resident summaries) — the same
/// "unavailable" signal `BudgetEvaluation::current_value` already uses for
/// an unavailable `PercentOfWindow` snapshot, never a fabricated zero.
fn token_budget_current_value(
    state: &AppState,
    budget: &crate::quota_store::QuotaBudget,
    now: DateTime<Utc>,
) -> Option<f64> {
    let period_hours = budget.period_hours.unwrap_or(24).max(1) as i64;
    let since = now - chrono::Duration::hours(period_hours);
    let ids: Vec<String> = state
        .sessions
        .iter()
        .filter(|entry| entry.value().summary.harness == budget.provider)
        .filter(|entry| {
            budget.project_key.is_none()
                || entry.value().summary.project_key.as_deref() == budget.project_key.as_deref()
        })
        .map(|entry| entry.key().clone())
        .collect();
    match state.full_sessions(&ids) {
        Ok(sessions) => Some(
            sessions
                .iter()
                .map(|session| session.range_totals(Some(since), None).tokens.total_tokens as f64)
                .sum(),
        ),
        Err(error) => {
            tracing::warn!(
                "could not compute token budget current value for {:?}: {}",
                budget.provider,
                error
            );
            None
        }
    }
}

/// Recomputes quota snapshots against configured soft budgets and returns
/// any newly crossed thresholds, persisting the updated dedup log. Cheap
/// enough to poll on the same interval as `get_subscription_usage`.
///
/// Issue #131: this used to load `quota-v1.json` from disk and re-derive
/// `Vec<QuotaSnapshot>` from every session on *every* call, unconditionally
/// — a second, independent O(corpus) walk on top of `get_quota_snapshots`'s,
/// invisible in a recording taken with no budgets configured (the early
/// return below skipped instrumentation entirely) but live the moment a
/// user configures one, on this command's own 60s poll from
/// `SubscriptionUsage.svelte`. It now reads the cached quota-config file
/// ([`AppState::quota_store`], issue #128) instead of re-reading
/// `quota-v1.json` every call, and shares [`AppState::quota_snapshots`]
/// with `get_quota_snapshots` instead of maintaining a second, ungated read
/// path — so the two 60s-ish independent pollers (tray, alerts) collapse
/// onto the same `QuotaSnapshotCache`-gated, `QuotaPointsIndex`-backed
/// recompute rather than each paying their own.
#[tauri::command]
pub fn check_quota_alerts(state: State<'_, Arc<AppState>>) -> Vec<crate::quota::QuotaAlert> {
    check_quota_alerts_impl(&state)
}

/// The actual body of [`check_quota_alerts`], split out to take a plain
/// `&AppState` rather than a `tauri::State` so it is directly unit-testable
/// (`tauri::State` cannot be constructed outside a running app without the
/// `tauri` crate's `test` feature, which this crate does not depend on).
/// `AppState`-owning tests live in `store.rs`'s
/// `#[cfg(all(test, not(windows)))]` module for the same reason that module
/// itself gives (constructing the full `AppState` links Wry/tray-icon GUI
/// entry points in a Windows unit-test binary) —
/// `check_quota_alerts_with_a_configured_budget_reads_the_points_index_not_the_corpus`
/// there calls this function directly.
pub(crate) fn check_quota_alerts_impl(state: &AppState) -> Vec<crate::quota::QuotaAlert> {
    let started = Instant::now();
    let store = state.quota_store();
    if store.budgets.is_empty() {
        // Recorded even though there is nothing to do: a metric that goes
        // quiet only because the *feature* (budgets) is unconfigured, not
        // because the work is cheap, hides exactly the cost this command
        // pays the moment someone turns it on.
        state.performance.record_backend(
            "ipc.check_quota_alerts",
            started,
            true,
            BTreeMap::from([
                ("budgets".into(), "0".to_string()),
                ("alerts".into(), "0".to_string()),
            ]),
        );
        return Vec::new();
    }
    let now = Utc::now();
    let max_cache_age = chrono::Duration::seconds(store.max_cache_age_secs);
    let snapshots = state.quota_snapshots(max_cache_age, now);
    let snapshot_by_provider: HashMap<&str, &crate::quota::QuotaSnapshot> = snapshots
        .iter()
        .map(|snapshot| (snapshot.provider.as_str(), snapshot))
        .collect();

    let evaluations: Vec<crate::quota::BudgetEvaluation> = store
        .budgets
        .iter()
        .map(|budget| {
            let current_value = match budget.unit {
                crate::quota_store::BudgetUnit::PercentOfWindow => snapshot_by_provider
                    .get(budget.provider.as_str())
                    .and_then(|snapshot| {
                        snapshot.windows.iter().find(|window| {
                            window.unavailable.is_none()
                                && budget.window_kind.as_deref() == Some(window.kind.as_str())
                        })
                    })
                    .and_then(|window| window.used),
                crate::quota_store::BudgetUnit::Tokens => {
                    token_budget_current_value(state, budget, now)
                }
            };
            crate::quota::BudgetEvaluation {
                budget,
                current_value,
            }
        })
        .collect();

    let local_hour = Local::now().hour() as u8;
    let (alerts, updated_log) = crate::quota::evaluate_alerts(
        &evaluations,
        &store.notifications,
        &store.notification_log,
        now,
        local_hour,
    );
    if let Err(error) = state.persist_quota_notification_log(updated_log, now) {
        tracing::warn!("could not persist quota notification log: {}", error);
    }
    state.performance.record_backend(
        "ipc.check_quota_alerts",
        started,
        true,
        BTreeMap::from([
            ("budgets".into(), store.budgets.len().to_string()),
            ("alerts".into(), alerts.len().to_string()),
        ]),
    );
    alerts
}
