use crate::model::Session;
use crate::watcher::WatcherHandle;
use dashmap::DashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;

pub struct AppState {
    pub sessions: DashMap<String, Session>,
    pub scanned: AtomicBool,
    pub watcher: Mutex<Option<WatcherHandle>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            scanned: AtomicBool::new(false),
            watcher: Mutex::new(None),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
