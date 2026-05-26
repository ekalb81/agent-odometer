use crate::config::Config;
use crate::model::Session;
use crate::rates::RateCard;
use crate::store::AppState;
use std::sync::atomic::Ordering;
use tauri::State;

/// Returns the list of all known sessions.
/// On the first call, runs an initial scan against the current config.
/// Phase 3 will replace the scan trigger with a file watcher.
#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Vec<Session> {
    if state.scanned.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_ok() {
        let config = Config::load().unwrap_or_default();
        let found = crate::scanner::initial_scan(&config.session_roots, &config.archive_roots);
        for (id, session) in found {
            state.sessions.insert(id, session);
        }
    }

    state
        .sessions
        .iter()
        .map(|entry| entry.value().clone())
        .collect()
}

/// Returns the current configuration. Phase 3 will persist and reload config.
#[tauri::command]
pub fn get_config() -> Config {
    Config::default()
}

/// Persists a new configuration. Phase 3 will implement persistence.
#[tauri::command]
pub fn set_config(_config: Config) -> Result<(), String> {
    Ok(())
}

/// Returns the bundled rate card. Phase 5 will return a live-fetched card.
#[tauri::command]
pub fn get_rates() -> RateCard {
    RateCard::load_bundled().unwrap_or_else(|_| RateCard {
        version: 1,
        currency: "USD".into(),
        unit: "per_1m_tokens".into(),
        source_url: String::new(),
        fetched_at: None,
        models: std::collections::HashMap::new(),
        fallback_model: "gpt-5-codex".into(),
    })
}

/// Persists an updated rate card. Phase 5 will implement persistence.
#[tauri::command]
pub fn set_rates(_rates: RateCard) -> Result<(), String> {
    Ok(())
}

/// Opens the given path in the system file manager. Phase 4 will implement this fully.
#[tauri::command]
pub fn reveal_in_file_manager(_path: String) -> Result<(), String> {
    Ok(())
}
