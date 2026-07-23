//! Persistent scan cache backed by SQLite.
//!
//! Entries are validated by source-file size and mtime. A scan touches or
//! replaces individual rows, then removes rows not seen in that generation.
//! This avoids deserializing, cloning, and rewriting the entire cached corpus
//! whenever one rollout changes.
//!
//! The cache is versioned by the app version: any release may change parser
//! semantics or the `Session` shape, and a stale cache must lose to a fresh
//! parse, never win. Read, decode, or database errors degrade to a cache miss.

use crate::model::Session;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, UNIX_EPOCH};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const LEGACY_CACHE_NAME: &str = "scan-cache.json";

#[derive(Serialize, Deserialize, Clone)]
pub struct CacheEntry {
    pub size: u64,
    pub mtime_ms: u64,
    pub session: Session,
}

#[derive(Deserialize)]
struct LegacyScanCache {
    version: String,
    entries: HashMap<String, CacheEntry>,
}

/// A failed cache open becomes a disabled cache. Scanning and parsing remain
/// fully functional because this layer is never a source of truth.
#[derive(Default)]
pub struct ScanCache {
    connection: Option<Mutex<Connection>>,
    generation: i64,
}

/// (size, mtime in ms since epoch) for a file; None when it can't be stat'ed.
pub fn file_stamp(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ms = meta
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis()
        .try_into()
        .ok()?;
    Some((meta.len(), mtime_ms))
}

impl ScanCache {
    /// Opens the cache and starts a new scan generation. Any initialization
    /// error disables caching for this scan rather than blocking startup.
    pub fn load(path: &Path) -> Self {
        match Self::open(path) {
            Ok(cache) => cache,
            Err(error) => {
                tracing::warn!("scan cache unavailable at {:?}: {}", path, error);
                Self::default()
            }
        }
    }

    fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA temp_store = MEMORY;
             CREATE TABLE IF NOT EXISTS cache_meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS sessions (
                 path TEXT PRIMARY KEY,
                 size INTEGER NOT NULL,
                 mtime_ms INTEGER NOT NULL,
                 session_json BLOB NOT NULL,
                 seen_generation INTEGER NOT NULL
             );",
        )?;

        // Serialize version validation and generation allocation across all
        // processes/concurrent scans using SQLite's write lock. A read followed
        // by a later write can hand two scans the same generation.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stored_version: Option<String> = transaction
            .query_row(
                "SELECT value FROM cache_meta WHERE key = 'app_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if stored_version.as_deref().is_some_and(|v| v != APP_VERSION) {
            tracing::info!("scan cache version mismatch; invalidating entries");
            transaction.execute("DELETE FROM sessions", [])?;
        }
        transaction.execute(
            "INSERT INTO cache_meta(key, value) VALUES('app_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [APP_VERSION],
        )?;
        let generation: i64 = transaction.query_row(
            "INSERT INTO cache_meta(key, value) VALUES('generation', '1')
             ON CONFLICT(key) DO UPDATE SET
                 value = CAST(cache_meta.value AS INTEGER) + 1
             RETURNING CAST(value AS INTEGER)",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;

        migrate_legacy_cache(&mut connection, path)?;

        Ok(Self {
            connection: Some(Mutex::new(connection)),
            generation,
        })
    }

    pub fn len(&self) -> usize {
        let Some(connection) = &self.connection else {
            return 0;
        };
        connection
            .lock()
            .ok()
            .and_then(|connection| {
                connection
                    .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .ok()
            })
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn is_enabled(&self) -> bool {
        self.connection.is_some()
    }

    /// Returns an owned cached session when the stored stamp matches, and
    /// marks the row as seen in this scan generation.
    pub fn lookup(&self, key: &str, size: u64, mtime_ms: u64) -> Option<Session> {
        let size = i64::try_from(size).ok()?;
        let mtime_ms = i64::try_from(mtime_ms).ok()?;
        let connection = self.connection.as_ref()?;
        let raw: Vec<u8> = {
            let connection = connection.lock().ok()?;
            connection
                .query_row(
                    "UPDATE sessions
                     SET seen_generation = MAX(seen_generation, ?4)
                     WHERE path = ?1 AND size = ?2 AND mtime_ms = ?3
                     RETURNING session_json",
                    params![key, size, mtime_ms, self.generation],
                    |row| row.get(0),
                )
                .optional()
                .ok()??
        };
        match serde_json::from_slice(&raw) {
            Ok(session) => Some(session),
            Err(error) => {
                tracing::warn!("corrupt scan-cache entry {:?}: {}; discarding", key, error);
                if let Ok(connection) = connection.lock() {
                    let _ = connection.execute("DELETE FROM sessions WHERE path = ?1", [key]);
                }
                None
            }
        }
    }

    /// Inserts or replaces one parsed session without materializing the rest
    /// of the cache in memory.
    pub fn store(&self, key: &str, size: u64, mtime_ms: u64, session: &Session) {
        let Some(connection) = &self.connection else {
            return;
        };
        let (Ok(size), Ok(mtime_ms), Ok(raw)) = (
            i64::try_from(size),
            i64::try_from(mtime_ms),
            serde_json::to_vec(session),
        ) else {
            tracing::warn!("could not encode scan-cache entry {:?}", key);
            return;
        };
        let Ok(connection) = connection.lock() else {
            tracing::warn!("scan-cache lock poisoned while storing {:?}", key);
            return;
        };
        if let Err(error) = connection.execute(
            "INSERT INTO sessions(path, size, mtime_ms, session_json, seen_generation)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET
                 size = excluded.size,
                 mtime_ms = excluded.mtime_ms,
                 session_json = excluded.session_json,
                 seen_generation = MAX(sessions.seen_generation, excluded.seen_generation)",
            params![key, size, mtime_ms, raw, self.generation],
        ) {
            tracing::warn!("could not store scan-cache entry {:?}: {}", key, error);
        }
    }

    /// Removes entries whose files were not observed in the completed scan.
    pub fn finish_scan(&self) {
        let Some(connection) = &self.connection else {
            return;
        };
        let Ok(connection) = connection.lock() else {
            tracing::warn!("scan-cache lock poisoned during cleanup");
            return;
        };
        // Only the newest scan may prune. Older overlapping scans must not
        // delete rows touched by a newer generation, and generation touches
        // are monotonic for the same reason.
        if let Err(error) = connection.execute(
            "DELETE FROM sessions
             WHERE seen_generation < ?1
               AND ?1 = (
                   SELECT CAST(value AS INTEGER)
                   FROM cache_meta
                   WHERE key = 'generation'
               )",
            [self.generation],
        ) {
            tracing::warn!("could not prune stale scan-cache entries: {}", error);
        }
    }
}

fn migrate_legacy_cache(connection: &mut Connection, sqlite_path: &Path) -> anyhow::Result<()> {
    let legacy_path: PathBuf = sqlite_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(LEGACY_CACHE_NAME);
    let row_count: i64 =
        connection.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
    if row_count != 0 {
        remove_legacy_cache(&legacy_path);
        return Ok(());
    }
    let raw = match std::fs::read(&legacy_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            tracing::warn!(
                "legacy scan cache unreadable at {:?}: {}",
                legacy_path,
                error
            );
            return Ok(());
        }
    };
    let legacy = match serde_json::from_slice::<LegacyScanCache>(&raw) {
        Ok(cache) if cache.version == APP_VERSION => cache,
        Ok(_) => return Ok(()),
        Err(error) => {
            tracing::warn!("legacy scan cache corrupt at {:?}: {}", legacy_path, error);
            return Ok(());
        }
    };
    let transaction = connection.transaction()?;
    {
        let mut statement = transaction.prepare(
            "INSERT OR REPLACE INTO sessions
             (path, size, mtime_ms, session_json, seen_generation)
             VALUES(?1, ?2, ?3, ?4, 0)",
        )?;
        for (path, entry) in legacy.entries {
            let (Ok(size), Ok(mtime_ms), Ok(session_json)) = (
                i64::try_from(entry.size),
                i64::try_from(entry.mtime_ms),
                serde_json::to_vec(&entry.session),
            ) else {
                continue;
            };
            statement.execute(params![path, size, mtime_ms, session_json])?;
        }
    }
    transaction.commit()?;
    tracing::info!("migrated legacy scan cache from {:?}", legacy_path);
    remove_legacy_cache(&legacy_path);
    Ok(())
}

fn remove_legacy_cache(legacy_path: &Path) {
    if let Err(error) = std::fs::remove_file(legacy_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                "legacy scan cache migrated but could not be removed at {:?}: {}",
                legacy_path,
                error
            );
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
            tool_observations: Vec::new(),
            tool_metrics: Default::default(),
            tool_metrics_by_model: Default::default(),
            category_totals: Default::default(),
            optimization_findings: Vec::new(),
        }
    }

    #[test]
    fn roundtrip_stamp_matching_and_generation_pruning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let cache = ScanCache::load(&path);
        cache.store("a.jsonl", 100, 5_000, &session("s1"));
        cache.store("removed.jsonl", 10, 500, &session("removed"));
        cache.finish_scan();
        assert_eq!(cache.len(), 2);
        drop(cache);

        let cache = ScanCache::load(&path);
        assert_eq!(cache.lookup("a.jsonl", 100, 5_000).unwrap().id, "s1");
        assert!(cache.lookup("a.jsonl", 101, 5_000).is_none());
        assert!(cache.lookup("a.jsonl", 100, 5_001).is_none());
        cache.finish_scan();
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn unavailable_parent_disables_cache() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, "x").unwrap();
        let cache = ScanCache::load(&file.join("cache.sqlite3"));
        assert!(cache.is_empty());
        assert!(!cache.is_enabled());
    }

    #[test]
    fn overlapping_generations_do_not_prune_newer_touches() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let older = ScanCache::load(&path);
        older.store("a.jsonl", 100, 5_000, &session("s1"));

        let newer = ScanCache::load(&path);
        assert_eq!(newer.lookup("a.jsonl", 100, 5_000).unwrap().id, "s1");

        // A late touch/cleanup from the older scan must neither lower the
        // generation nor remove a row already observed by the newer scan.
        assert_eq!(older.lookup("a.jsonl", 100, 5_000).unwrap().id, "s1");
        older.finish_scan();
        assert_eq!(newer.lookup("a.jsonl", 100, 5_000).unwrap().id, "s1");
        newer.finish_scan();
        assert_eq!(newer.len(), 1);
    }

    #[test]
    fn migrates_legacy_monolithic_cache() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = serde_json::json!({
            "version": APP_VERSION,
            "entries": {
                "a.jsonl": { "size": 100, "mtime_ms": 5000, "session": session("s1") }
            }
        });
        std::fs::write(
            dir.path().join(LEGACY_CACHE_NAME),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();
        let cache = ScanCache::load(&dir.path().join("cache.sqlite3"));
        assert_eq!(cache.lookup("a.jsonl", 100, 5_000).unwrap().id, "s1");
        assert!(!dir.path().join(LEGACY_CACHE_NAME).exists());
    }

    #[test]
    #[ignore = "performance probe; run with --release --ignored --nocapture"]
    fn performance_incremental_cache_1000_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let cache = ScanCache::load(&path);
        let started = std::time::Instant::now();
        for index in 0..1_000 {
            let key = format!("{index}.jsonl");
            cache.store(&key, index, index, &session(&key));
        }
        cache.finish_scan();
        let write_elapsed = started.elapsed();
        drop(cache);

        let cache = ScanCache::load(&path);
        let started = std::time::Instant::now();
        for index in 0..1_000 {
            let key = format!("{index}.jsonl");
            assert!(cache.lookup(&key, index, index).is_some());
        }
        eprintln!(
            "1000 incremental writes: {:?}; 1000 warm reads: {:?}",
            write_elapsed,
            started.elapsed()
        );
    }
}
