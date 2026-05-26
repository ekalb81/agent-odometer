use dashmap::DashMap;
use crate::model::Session;

/// Shared application state managed by Tauri.
/// Phase 3 will populate sessions via the file watcher.
pub struct AppState {
    pub sessions: DashMap<String, Session>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
        }
    }
}
