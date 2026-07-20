//! Persistent scan cache: parsed `Session`s keyed by source file path and
//! validated by (size, mtime). On a warm launch, unchanged session files are
//! served from one sequential cache read instead of re-reading and re-parsing
//! gigabytes of JSONL — the difference between a minute-plus cold start and a
//! couple of seconds.
//!
//! The cache is versioned by the app version: any release may change parser
//! semantics or the `Session` shape, and a stale cache must lose to a fresh
//! parse, never win. Corrupt or mismatched caches are silently discarded.

use crate::model::Session;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Serialize, Deserialize, Clone)]
pub struct CacheEntry {
    pub size: u64,
    pub mtime_ms: u64,
    pub session: Session,
}

#[derive(Serialize, Deserialize, Default)]
pub struct ScanCache {
    version: String,
    entries: HashMap<String, CacheEntry>,
}

/// (size, mtime in ms since epoch) for a file; None when it can't be stat'ed.
pub fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some((meta.len(), mtime_ms))
}

impl ScanCache {
    /// Loads the cache, returning an empty one on any error or version
    /// mismatch — the cache is an optimization, never a source of truth.
    pub fn load(path: &Path) -> Self {
        let raw = match std::fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("scan cache unreadable at {:?}: {}", path, e);
                }
                return Self::default();
            }
        };
        match serde_json::from_str::<Self>(&raw) {
            Ok(cache) if cache.version == APP_VERSION => cache,
            Ok(_) => {
                tracing::info!("scan cache version mismatch; discarding");
                Self::default()
            }
            Err(e) => {
                tracing::warn!("scan cache corrupt at {:?}: {}; discarding", path, e);
                Self::default()
            }
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the cached session for `key` when the stored stamp matches.
    pub fn lookup(&self, key: &str, size: u64, mtime_ms: u64) -> Option<&Session> {
        self.entries
            .get(key)
            .filter(|e| e.size == size && e.mtime_ms == mtime_ms)
            .map(|e| &e.session)
    }

    /// Consumes the cache, handing ownership of the entries to the caller —
    /// lets the scanner MOVE hits into the refreshed cache instead of holding
    /// a third full copy of every session during the scan.
    pub fn into_entries(self) -> HashMap<String, CacheEntry> {
        self.entries
    }

    /// Atomically writes a new cache built from `entries`.
    pub fn save(path: &Path, entries: HashMap<String, CacheEntry>) {
        let cache = Self {
            version: APP_VERSION.to_owned(),
            entries,
        };
        let write = || -> anyhow::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let tmp = path.with_extension("json.tmp");
            let file = std::fs::File::create(&tmp)?;
            serde_json::to_writer(std::io::BufWriter::new(file), &cache)?;
            std::fs::rename(&tmp, path)?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::warn!("could not write scan cache to {:?}: {}", path, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Harness, TokenTotals};
    use std::collections::HashMap as StdHashMap;

    fn session(id: &str) -> Session {
        Session {
            id: id.into(),
            harness: Harness::Codex,
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            archived: false,
            started_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            last_event_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            working_directory: None,
            originator: None,
            source: None,
            history_mode: None,
            memory_mode: None,
            cli_version: None,
            model_provider: None,
            model: None,
            service_tier: None,
            plan_type: None,
            credits_unlimited: None,
            credits_balance: None,
            context_window: None,
            latest_context_tokens: None,
            total_turns: 0,
            first_user_message: None,
            tokens_total: TokenTotals::default(),
            tokens_by_model: StdHashMap::new(),
            tokens_history: Vec::new(),
            turns: Vec::new(),
        }
    }

    #[test]
    fn roundtrip_and_stamp_matching() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let mut entries = HashMap::new();
        entries.insert(
            "a.jsonl".to_owned(),
            CacheEntry {
                size: 100,
                mtime_ms: 5_000,
                session: session("s1"),
            },
        );
        ScanCache::save(&path, entries);

        let cache = ScanCache::load(&path);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.lookup("a.jsonl", 100, 5_000).unwrap().id, "s1");
        // Any stamp difference is a miss.
        assert!(cache.lookup("a.jsonl", 101, 5_000).is_none());
        assert!(cache.lookup("a.jsonl", 100, 5_001).is_none());
        assert!(cache.lookup("b.jsonl", 100, 5_000).is_none());
    }

    #[test]
    fn missing_or_corrupt_cache_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ScanCache::load(&dir.path().join("nope.json")).is_empty());
        let bad = dir.path().join("bad.json");
        std::fs::write(&bad, b"{ not json").unwrap();
        assert!(ScanCache::load(&bad).is_empty());
    }

    #[test]
    fn version_mismatch_discards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        let stale = serde_json::json!({ "version": "0.0.0-old", "entries": {} });
        std::fs::write(&path, serde_json::to_string(&stale).unwrap()).unwrap();
        assert!(ScanCache::load(&path).is_empty());
    }
}
