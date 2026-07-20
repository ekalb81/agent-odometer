use crate::model::Session;
use crate::watcher::WatcherHandle;
use dashmap::DashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use std::sync::Mutex;

pub struct AppState {
    pub sessions: DashMap<String, Session>,
    pub scanned: AtomicBool,
    /// Files processed / discovered by the current bulk scan, for the UI's
    /// startup progress indicator.
    pub scan_done: AtomicUsize,
    pub scan_total: AtomicUsize,
    pub watcher: Mutex<Option<WatcherHandle>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: DashMap::new(),
            scanned: AtomicBool::new(false),
            scan_done: AtomicUsize::new(0),
            scan_total: AtomicUsize::new(0),
            watcher: Mutex::new(None),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
