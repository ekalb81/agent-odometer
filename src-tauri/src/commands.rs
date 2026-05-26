use crate::config::Config;
use crate::model::Session;
use crate::rates::RateCard;
use crate::store::AppState;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

/// Returns the list of all known sessions.
/// Sessions are populated at startup by the initial scan + file watcher;
/// this command just reads the in-memory map.
#[tauri::command]
pub fn list_sessions(state: State<'_, Arc<AppState>>) -> Vec<Session> {
    state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .collect()
}

/// Returns the current configuration.
#[tauri::command]
pub fn get_config() -> Result<Config, String> {
    Config::load().map_err(|e| e.to_string())
}

/// Persists a new configuration, clears the session cache, re-scans with the
/// new roots, restarts the file watcher, and emits a "config-updated" event.
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

    let found = crate::scanner::initial_scan(&config.session_roots, &config.archive_roots);
    for (id, session) in found {
        state.sessions.insert(id, session);
    }
    state.scanned.store(true, Ordering::Release);

    let handle = crate::watcher::start(
        app.clone(),
        state.inner().clone(),
        config.session_roots.clone(),
        config.archive_roots.clone(),
    )
    .map_err(|e| e.to_string())?;
    *state.watcher.lock().unwrap() = Some(handle);

    app.emit("config-updated", &config).map_err(|e| e.to_string())?;

    Ok(())
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
    })
}

/// Persists an updated rate card to disk and emits a rates-updated event so all
/// frontend subscribers can refresh their computed credits immediately.
#[tauri::command]
pub fn set_rates(app: tauri::AppHandle, rates: RateCard) -> Result<(), String> {
    rates.save().map_err(|e| e.to_string())?;
    app.emit("rates-updated", &rates).map_err(|e| e.to_string())?;
    Ok(())
}

/// Opens the parent directory of the given path in the system file manager.
/// Uses xdg-open on Linux, open on macOS, and explorer on Windows.
/// Errors are logged but not propagated — the UI treats this as best-effort.
#[tauri::command]
pub fn reveal_in_file_manager(path: String) -> Result<(), String> {
    let target = std::path::Path::new(&path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or(path);

    #[cfg(target_os = "linux")]
    let cmd = std::process::Command::new("xdg-open").arg(&target).spawn();

    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("open").arg(&target).spawn();

    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("explorer").arg(&target).spawn();

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    let cmd: Result<_, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unsupported platform",
    ));

    cmd.map(|_| ()).map_err(|e| e.to_string())
}
