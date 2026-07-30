use crate::config::{Config, DefenderExclusionReceipt, DEFENDER_EXCLUSION_RECEIPT_VERSION};
use crate::model::{RangeTotals, Session, SessionSummary};
use crate::rates::RateCard;
use crate::store::AppState;
#[cfg(any(windows, test))]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::{DateTime, Utc};
#[cfg(any(windows, test))]
use std::collections::HashSet;
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
pub async fn list_instruction_files(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
) -> Result<crate::instructions::InstructionInventory, String> {
    let started = Instant::now();
    let (config, scan_id) = {
        // Capture the durable roots and allocate their scan generation in the
        // same config transition. A settings save can therefore only happen
        // wholly before or wholly after this snapshot.
        let _transition = state.config_transition.lock().unwrap();
        let config = Config::load().map_err(|error| error.to_string())?;
        let scan_id = state.begin_instruction_scan();
        (config, scan_id)
    };
    let sessions = state
        .sessions
        .iter()
        .filter_map(|entry| {
            let session = entry.value();
            session.working_directory.as_ref().map(|working_directory| {
                crate::instructions::InstructionSessionContext {
                    working_directory: std::path::PathBuf::from(working_directory),
                    last_event_at: session.last_event_at,
                }
            })
        })
        .collect::<Vec<_>>();
    let app_state = state.inner().clone();
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
        app_state.publish_instruction_paths_if_current(scan_id, paths);
    }
    app_state.performance.record_backend(
        "ipc.list_instruction_files",
        started,
        result.is_ok(),
        BTreeMap::from([(
            "files".into(),
            result
                .as_ref()
                .map(|inventory| inventory.files.len())
                .unwrap_or(0)
                .to_string(),
        )]),
    );
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

fn tool_impact_sessions(
    app_state: &Arc<AppState>,
    session_ids: Option<Vec<String>>,
) -> Vec<Arc<Session>> {
    match session_ids {
        Some(ids) => ids
            .into_iter()
            .filter_map(|id| {
                app_state
                    .sessions
                    .get(&id)
                    .map(|entry| entry.value().clone())
            })
            .collect(),
        None => app_state
            .sessions
            .iter()
            .map(|entry| entry.value().clone())
            .collect(),
    }
}

#[tauri::command]
pub async fn list_tool_impact_targets(
    state: State<'_, Arc<AppState>>,
    query: ToolImpactScopeQuery,
) -> Result<Vec<crate::tool_impact::ToolImpactTarget>, String> {
    let started = Instant::now();
    let (from, to) = parse_tool_impact_range(query.from, query.to)?;
    let app_state = state.inner().clone();
    let sessions = tool_impact_sessions(&app_state, query.session_ids);
    let session_count = sessions.len();
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::tool_impact::list_targets(&sessions, from, to)
    })
    .await
    .map_err(|error| error.to_string());
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
    let sessions = tool_impact_sessions(&app_state, query.session_ids);
    let session_count = sessions.len();
    let target_kind = query.target_kind;
    let result = tauri::async_runtime::spawn_blocking(move || {
        crate::tool_impact::compare(&sessions, target_kind, &target_key, from, to)
    })
    .await
    .map_err(|error| error.to_string());
    app_state.performance.record_backend(
        "ipc.compare_tool_impact",
        started,
        result.is_ok(),
        BTreeMap::from([("sessions".into(), session_count.to_string())]),
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

/// Reports whether each requested harness hook is actually installed and the
/// last bounded, local hook result. This makes setup observable without
/// exposing transcript contents or arbitrary configuration data.
#[tauri::command]
pub fn get_turn_receipt_status(
) -> Result<crate::harness_integration::TurnReceiptIntegrationStatus, String> {
    let config = Config::load().map_err(|error| error.to_string())?;
    Ok(crate::harness_integration::status(&config))
}

/// Reconciles the installed handlers with the already-saved opt-in settings.
/// It is intentionally separate from startup so Odometer never repairs or
/// writes harness configuration merely because the app was opened.
#[tauri::command]
pub fn repair_turn_receipt_integrations(
) -> Result<crate::harness_integration::TurnReceiptIntegrationStatus, String> {
    let config = Config::load().map_err(|error| error.to_string())?;
    let transaction =
        crate::harness_integration::sync(&config).map_err(|error| error.to_string())?;
    transaction.commit();
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
        config.save().map_err(|e| e.to_string())?;
        if instruction_sources_changed {
            state.cancel_instruction_scan_and_clear_paths();
            let previous_watcher = state
                .config_watcher
                .lock()
                .unwrap()
                .replace(config_watcher_replacement.expect("replacement was staged"));
            drop(previous_watcher);
        }
        if let Some(transaction) = integration {
            transaction.commit();
        }
        state.performance.configure(
            config.performance_tracking_enabled,
            config.performance_log_max_mb,
        );
        app.emit("config-updated", &config)
            .map_err(|e| e.to_string())?;
        state.performance.record_backend(
            if instruction_settings_changed {
                "settings.save_instructions"
            } else if receipt_settings_changed {
                "settings.save_turn_receipts"
            } else {
                "settings.save_performance"
            },
            started,
            true,
            BTreeMap::new(),
        );
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
    config.save().map_err(|e| e.to_string())?;
    state.cancel_instruction_scan_and_clear_paths();
    if let Some(transaction) = integration {
        transaction.commit();
    }
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

    spawn_scan(
        app.clone(),
        state.inner().clone(),
        config.clone(),
        provider_sources,
        true,
    );

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

fn preserve_backend_owned_config(previous: &Config, next: &mut Config) {
    next.defender_exclusion_receipt = previous.defender_exclusion_receipt.clone();
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
    // Allocate the archive generation before workers start. Live watcher
    // writes during discovery use the same generation, preventing a newly
    // created file from being falsely marked missing at completion.
    let history_generation = source_configuration_valid
        .then(|| state.begin_history_scan())
        .flatten();
    state.scanned.store(false, Ordering::Release);
    state.scan_done.store(0, Ordering::Release);
    state.scan_total.store(0, Ordering::Release);

    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        // An invalid legacy source configuration must not prune an otherwise
        // healthy cache merely because the fail-closed scan has no roots.
        let cache_path = source_configuration_valid.then(|| {
            dirs::cache_dir().map(|d| d.join("agent-odometer").join("scan-cache-v2.sqlite3"))
        });
        let cache_path = cache_path.flatten();

        let report = crate::scanner::scan_all(
            &provider_sources,
            cache_path.as_deref(),
            |path, session| {
                if state.current_scan_generation() != generation {
                    return;
                }
                let Some(reconciled) = state.reconcile_scanned_session_if_current(
                    generation,
                    path,
                    session,
                    history_generation,
                ) else {
                    return;
                };
                let summary = SessionSummary::of(&reconciled.session);
                if state.publish_scanned_session(generation, path, reconciled.session) {
                    if let Err(e) = app.emit("session-updated", &summary) {
                        tracing::warn!("emit session-updated failed: {}", e);
                    }
                }
                if let Some(displaced) = reconciled.displaced {
                    if let Err(e) = app.emit("session-updated", &SessionSummary::of(&displaced)) {
                        tracing::warn!("emit displaced session-updated failed: {}", e);
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

        // A parser/read failure makes a complete source observation
        // untrustworthy. Retain stale-present history rather than incorrectly
        // marking a transcript missing.
        if !source_configuration_valid {
            tracing::warn!(
                "durable source availability was not finalized: invalid source configuration"
            );
        } else if report.parse_failures == 0 {
            if let Some(history_generation) = history_generation {
                for session in state.finish_history_scan(history_generation) {
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

        if state.current_scan_generation() != generation {
            return;
        }

        // Overlay thread names from the session index, if present.
        let names = crate::session_index::read(&config.session_index_path);
        let changed = crate::session_index::apply(&state.sessions, &names);
        for id in changed {
            if let Some(session) = state
                .sessions
                .get(&id)
                .map(|session| session.value().as_ref().clone())
            {
                state.persist_session_metadata(&session);
                if let Err(e) = app.emit("session-updated", &SessionSummary::of(&session)) {
                    tracing::warn!("emit session-updated failed: {}", e);
                }
            }
        }

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
        unpriced_models: Vec::new(),
        pricing_catalog: Default::default(),
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
        defender_verification_script, powershell_encoded_command, powershell_encoded_path,
        preserve_backend_owned_config, range_has_data, valid_session_id, write_export_file,
    };
    use crate::config::{Config, DefenderExclusionReceipt, DEFENDER_EXCLUSION_RECEIPT_VERSION};
    use crate::model::{RangeTotals, TokenTotals, ToolMetrics};
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;

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
                .chunks_exact(2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
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
}
