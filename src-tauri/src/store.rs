use dashmap::DashMap;
use std::sync::atomic::AtomicBool;
use crate::model::Session;

/// Shared application state managed by Tauri.
/// Phase 3 will populate sessions via the file watcher.
pub struct AppState {
    pub sessions: DashMap<String, Session>,
    pub scanned: AtomicBool,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            scanned: AtomicBool::new(false),
        }
    }
}
