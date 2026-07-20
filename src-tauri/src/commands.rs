use crate::config::Config;
use crate::model::{RangeTotals, Session, SessionSummary};
use crate::rates::RateCard;
use crate::store::AppState;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

/// Returns lightweight summaries of all known sessions. Full sessions
/// (turns, token history) are fetched per-id via `get_session_details` —
/// shipping them all here measured ~200 MB of JSON on a real corpus.
#[tauri::command]
pub fn list_sessions(state: State<'_, Arc<AppState>>) -> Vec<SessionSummary> {
    state
        .sessions
        .iter()
        .map(|entry| SessionSummary::of(entry.value()))
        .collect()
}

/// Returns one full session (turns and token history included), for the
/// detail drawer.
#[tauri::command]
pub fn get_session_details(state: State<'_, Arc<AppState>>, session_id: String) -> Option<Session> {
    state
        .sessions
        .get(&session_id)
        .map(|entry| entry.value().clone())
}

/// Date-scoped token/credit rollups for every session, computed from the
/// in-memory event histories. Bounds are inclusive RFC3339 instants; None is
/// an open bound.
#[tauri::command]
pub fn sessions_in_range(
    state: State<'_, Arc<AppState>>,
    from: Option<String>,
    to: Option<String>,
) -> Result<HashMap<String, RangeTotals>, String> {
    let parse = |v: Option<String>| -> Result<Option<DateTime<Utc>>, String> {
        v.map(|s| s.parse().map_err(|e| format!("invalid timestamp: {e}")))
            .transpose()
    };
    let from = parse(from)?;
    let to = parse(to)?;
    Ok(state
        .sessions
        .iter()
        .map(|entry| (entry.key().clone(), entry.value().range_totals(from, to)))
        .collect())
}

/// Returns the current configuration.
#[tauri::command]
pub fn get_config() -> Result<Config, String> {
    Config::load().map_err(|e| e.to_string())
}

/// Persists a new configuration, clears the session cache, restarts the file
/// watcher, and emits "config-updated". The rescan itself runs on a
/// background thread, emitting a "session-updated" summary per parsed file
/// so the UI repopulates progressively instead of freezing.
#[tauri::command]
pub fn set_config(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    config: Config,
) -> Result<(), String> {
    config.save().map_err(|e| e.to_string())?;

    // Stop the old watcher before clearing sessions so no stale events arrive.
    state.watcher.lock().unwrap().take();

    state.sessions.clear();
    state.scanned.store(false, Ordering::Release);

    // Start the new watcher immediately; the background scan fills in
    // existing files while the watcher covers live changes.
    let handle = crate::watcher::start(
        app.clone(),
        state.inner().clone(),
        config.session_roots.clone(),
        config.archive_roots.clone(),
        config.claude_session_roots.clone(),
        config.session_index_path.clone(),
    )
    .map_err(|e| e.to_string())?;
    *state.watcher.lock().unwrap() = Some(handle);

    spawn_scan(app.clone(), state.inner().clone(), config.clone());

    app.emit("config-updated", &config)
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Scans all configured roots on a background thread, inserting sessions
/// into state and emitting a "session-updated" summary for each as it
/// parses. Applies the session-index name overlay and sets `scanned` when
/// done. Shared by startup (lib.rs) and set_config.
pub fn spawn_scan(app: AppHandle, state: Arc<AppState>, config: Config) {
    state.scan_done.store(0, Ordering::Release);
    state.scan_total.store(0, Ordering::Release);

    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let cache_path =
            dirs::cache_dir().map(|d| d.join("agent-odometer").join("scan-cache.json"));

        crate::scanner::scan_all(
            &config.session_roots,
            &config.archive_roots,
            &config.claude_session_roots,
            cache_path.as_deref(),
            |session| {
                let summary = SessionSummary::of(&session);
                state.sessions.insert(session.id.clone(), session);
                if let Err(e) = app.emit("session-updated", &summary) {
                    tracing::warn!("emit session-updated failed: {}", e);
                }
            },
            |done, total| {
                state.scan_done.store(done, Ordering::Release);
                state.scan_total.store(total, Ordering::Release);
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

        // Overlay thread names from the session index, if present.
        let names = crate::session_index::read(&config.session_index_path);
        let changed = crate::session_index::apply(&state.sessions, &names);
        for id in changed {
            if let Some(session) = state.sessions.get(&id) {
                if let Err(e) = app.emit("session-updated", &SessionSummary::of(session.value())) {
                    tracing::warn!("emit session-updated failed: {}", e);
                }
            }
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
/// Returns Ok once the elevation prompt was launched; the user may still
/// decline it there.
#[tauri::command]
pub fn add_defender_exclusions() -> Result<(), String> {
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
        // itself asks the user for consent; nothing runs silently.
        let inner = format!("Add-MpPreference -ExclusionPath {}", paths.join(","));
        let arg_list = ps_quote(&format!("-NoProfile -Command {inner}"));
        let outer = format!("Start-Process powershell -Verb RunAs -ArgumentList {arg_list}");

        std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &outer])
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        Err("Defender exclusions are only applicable on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::valid_session_id;

    #[test]
    fn validates_deep_link_session_ids() {
        assert!(valid_session_id("019f5d3b-6b2f-75f1-aed9-723e7c488e66"));
        assert!(!valid_session_id(""));
        assert!(!valid_session_id("task/id"));
        assert!(!valid_session_id("task?id=1"));
    }
}
