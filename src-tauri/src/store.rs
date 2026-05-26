use dashmap::DashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use crate::model::Session;
use crate::watcher::WatcherHandle;

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
