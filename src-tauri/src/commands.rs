use crate::config::Config;
use crate::model::{RangeTotals, Session, SessionSummary};
use crate::rates::RateCard;
use crate::store::AppState;
use chrono::{DateTime, Utc};
use std::collections::{BTreeMap, HashMap};
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
pub fn get_performance_status(
    state: State<'_, Arc<AppState>>,
) -> crate::performance::PerformanceStatus {
    state.performance.status()
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
    let sessions: Vec<_> = app_state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    let session_count = sessions.len();
    let event_count = query.events.len();
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::correlation::correlate(&sessions, query)
    })
    .await
    .map_err(|error| error.to_string());
    app_state.performance.record_backend(
        "ipc.correlate_events",
        started,
        result.is_ok(),
        BTreeMap::from([
            ("sessions".into(), session_count.to_string()),
            ("events".into(), event_count.to_string()),
        ]),
    );
    result
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
        let sessions: Vec<_> = state
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
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
        outcomes
    })
    .await
    .map_err(|error| error.to_string())
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
#[tauri::command]
pub fn list_sessions(state: State<'_, Arc<AppState>>) -> Vec<SessionSummary> {
    let started = Instant::now();
    let result: Vec<_> = state
        .sessions
        .iter()
        .map(|entry| SessionSummary::of(entry.value().as_ref()))
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
/// detail drawer.
#[tauri::command]
pub fn get_session_details(state: State<'_, Arc<AppState>>, session_id: String) -> Option<Session> {
    let started = Instant::now();
    let result = state
        .sessions
        .get(&session_id)
        .map(|entry| entry.value().as_ref().clone());
    state.performance.record_backend(
        "ipc.get_session_details",
        started,
        result.is_some(),
        BTreeMap::from([("found".into(), result.is_some().to_string())]),
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
    let sessions: Vec<_> = match session_ids {
        Some(ids) => ids
            .into_iter()
            .filter_map(|id| {
                app_state
                    .sessions
                    .get(&id)
                    .map(|entry| (id, entry.value().clone()))
            })
            .collect(),
        None => app_state
            .sessions
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect(),
    };
    let session_count = sessions.len();
    let range_count = bounds.len();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let mut out: Vec<HashMap<String, RangeTotals>> = vec![HashMap::new(); bounds.len()];
        for (id, session) in sessions {
            for (i, rt) in session.range_totals_multi(&bounds).into_iter().enumerate() {
                if range_has_data(&rt) {
                    out[i].insert(id.clone(), rt);
                }
            }
        }
        out
    })
    .await
    .map_err(|e| e.to_string());
    app_state.performance.record_backend(
        "ipc.sessions_in_ranges",
        started,
        result.is_ok(),
        BTreeMap::from([
            ("sessions".into(), session_count.to_string()),
            ("ranges".into(), range_count.to_string()),
        ]),
    );
    result
}

fn range_has_data(range: &RangeTotals) -> bool {
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

/// Persists a new configuration and emits "config-updated". Performance-only
/// changes apply live. Session-source changes also clear the session cache,
/// restart watchers, and rescan in the background.
#[tauri::command]
pub fn set_config(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    config: Config,
) -> Result<(), String> {
    let _transition = state.config_transition.lock().unwrap();
    let started = Instant::now();
    if !(1..=1_024).contains(&config.performance_log_max_mb) {
        return Err("performance log size must be between 1 and 1024 MiB".into());
    }
    let previous = Config::load().map_err(|e| e.to_string())?;
    let session_sources_changed = !previous.session_sources_equal(&config);
    // Performance-only changes take effect immediately and do not restart
    // watchers or force a full corpus rescan.
    if !session_sources_changed {
        config.save().map_err(|e| e.to_string())?;
        state.performance.configure(
            config.performance_tracking_enabled,
            config.performance_log_max_mb,
        );
        app.emit("config-updated", &config)
            .map_err(|e| e.to_string())?;
        state.performance.record_backend(
            "settings.save_performance",
            started,
            true,
            BTreeMap::new(),
        );
        return Ok(());
    }

    // Stage the replacement before changing durable or live state. If watcher
    // construction fails, the existing configuration remains fully active.
    let replacement = crate::watcher::start(
        app.clone(),
        state.inner().clone(),
        config.session_roots.clone(),
        config.archive_roots.clone(),
        config.claude_session_roots.clone(),
        config.session_index_path.clone(),
    )
    .map_err(|e| e.to_string())?;
    config.save().map_err(|e| e.to_string())?;
    state.performance.configure(
        config.performance_tracking_enabled,
        config.performance_log_max_mb,
    );

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

    spawn_scan(app.clone(), state.inner().clone(), config.clone());

    app.emit("config-updated", &config)
        .map_err(|e| e.to_string())?;

    state.performance.record_backend(
        "settings.save_session_sources",
        started,
        true,
        BTreeMap::new(),
    );

    Ok(())
}

/// Scans all configured roots on a background thread, inserting sessions
/// into state and emitting a "session-updated" summary for each as it
/// parses. Applies the session-index name overlay and sets `scanned` when
/// done. Shared by startup (lib.rs) and set_config.
pub fn spawn_scan(app: AppHandle, state: Arc<AppState>, config: Config) {
    let generation = state.current_scan_generation();
    state.scanned.store(false, Ordering::Release);
    state.scan_done.store(0, Ordering::Release);
    state.scan_total.store(0, Ordering::Release);

    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let cache_path =
            dirs::cache_dir().map(|d| d.join("agent-odometer").join("scan-cache-v2.sqlite3"));

        let report = crate::scanner::scan_all(
            &config.session_roots,
            &config.archive_roots,
            &config.claude_session_roots,
            cache_path.as_deref(),
            |path, session| {
                if state.current_scan_generation() != generation {
                    return;
                }
                let summary = SessionSummary::of(&session);
                if state.publish_scanned_session(generation, path, session) {
                    if let Err(e) = app.emit("session-updated", &summary) {
                        tracing::warn!("emit session-updated failed: {}", e);
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
                        },
                    );
                }
            },
        );

        if state.current_scan_generation() != generation {
            return;
        }

        // Overlay thread names from the session index, if present.
        let names = crate::session_index::read(&config.session_index_path);
        let changed = crate::session_index::apply(&state.sessions, &names);
        for id in changed {
            if let Some(session) = state.sessions.get(&id) {
                if let Err(e) = app.emit(
                    "session-updated",
                    &SessionSummary::of(session.value().as_ref()),
                ) {
                    tracing::warn!("emit session-updated failed: {}", e);
                }
            }
        }

        if state.current_scan_generation() != generation {
            return;
        }
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
            report.parse_failures == 0,
            BTreeMap::from([
                ("files".into(), report.files.to_string()),
                ("discovery_ms".into(), format!("{:.3}", report.discovery_ms)),
                (
                    "processing_ms".into(),
                    format!("{:.3}", report.processing_ms),
                ),
                (
                    "cache_open_ms".into(),
                    format!("{:.3}", report.cache_open_ms),
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
            ]),
        );
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
    }
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

/// Escapes a path for a single-quoted PowerShell string literal.
#[cfg(windows)]
fn ps_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "''"))
}

/// Opens Windows' UAC consent flow to add the configured session roots as
/// Windows Defender real-time-scanning path exclusions. Strictly opt-in from
/// the UI: the user clicks the button AND approves the elevation prompt, and
/// only the session-data directories are excluded — never the app itself.
/// Waits for the elevated process and reports the real outcome — non-admin
/// processes cannot read the exclusion list back, so the exit code relayed
/// through the launcher is the only verification available.
#[tauri::command]
pub async fn add_defender_exclusions() -> Result<(), String> {
    #[cfg(windows)]
    {
        let config = Config::load().map_err(|e| e.to_string())?;
        let paths: Vec<String> = config
            .session_roots
            .iter()
            .chain(config.archive_roots.iter())
            .chain(config.claude_session_roots.iter())
            .filter(|p| p.exists())
            .map(|p| ps_quote(&p.to_string_lossy()))
            .collect();
        if paths.is_empty() {
            return Err("no existing session folders to exclude".into());
        }

        // Elevation happens through Start-Process -Verb RunAs, so Windows
        // itself asks the user for consent; nothing runs silently. The
        // elevated shell exits 0/1 by Add-MpPreference outcome; a declined
        // UAC prompt makes Start-Process throw, mapped to exit 2.
        let inner = format!(
            "try {{ Add-MpPreference -ExclusionPath {} -ErrorAction Stop; exit 0 }} catch {{ exit 1 }}",
            paths.join(",")
        );
        let arg_list = ps_quote(&format!("-NoProfile -Command {inner}"));
        let outer = format!(
            "try {{ $p = Start-Process powershell -Verb RunAs -ArgumentList {arg_list} -Wait -PassThru -ErrorAction Stop; exit $p.ExitCode }} catch {{ exit 2 }}"
        );

        let output = tauri::async_runtime::spawn_blocking(move || {
            std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &outer])
                .output()
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

        match output.status.code() {
            Some(0) => Ok(()),
            Some(2) => {
                Err("The Windows security prompt was declined — nothing was changed.".into())
            }
            _ => Err(
                "Windows Defender did not accept the exclusions. Another security product or a \
                 policy may be managing it."
                    .into(),
            ),
        }
    }
    #[cfg(not(windows))]
    {
        Err("Defender exclusions are only applicable on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::{range_has_data, valid_session_id, write_export_file};
    use crate::model::{RangeTotals, TokenTotals, ToolMetrics};

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
        write_export_file(&csv, "csv", "a,b\r\n1,2\r\n").unwrap();
        assert_eq!(std::fs::read_to_string(csv).unwrap(), "a,b\r\n1,2\r\n");
        let text = dir.path().join("usage.txt");
        assert!(write_export_file(&text, "csv", "nope").is_err());
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
        };
        assert!(range_has_data(&range));
    }
}
