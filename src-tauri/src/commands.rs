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
    std::thread::spawn(move || {
        crate::scanner::scan_all(
            &config.session_roots,
            &config.archive_roots,
            &config.claude_session_roots,
            |session| {
                let summary = SessionSummary::of(&session);
                state.sessions.insert(session.id.clone(), session);
                if let Err(e) = app.emit("session-updated", &summary) {
                    tracing::warn!("emit session-updated failed: {}", e);
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
        tracing::info!(
            "scan complete: {} sessions loaded, {} thread names from index",
            state.sessions.len(),
            names.len()
        );
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
