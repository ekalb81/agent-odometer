use crate::config::Config;
use crate::model::Session;
use crate::rates::RateCard;
use crate::store::AppState;
use std::sync::Arc;
use tauri::State;

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

/// Persists a new configuration and logs that a restart is needed
/// for the watcher to pick up the new roots (live re-watching is Phase 6).
#[tauri::command]
pub fn set_config(config: Config) -> Result<(), String> {
    config.save().map_err(|e| e.to_string())?;
    tracing::info!("config saved; restart the app for the new roots to take effect");
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
