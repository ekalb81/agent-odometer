//! Persistent scan cache backed by SQLite.
//!
//! Entries are validated by source-file size and mtime. A scan touches or
//! replaces individual rows, then removes rows not seen in that generation.
//! This avoids deserializing, cloning, and rewriting the entire cached corpus
//! whenever one rollout changes.
//!
//! The cache is versioned by `PARSE_VERSION`, not the app version: most
//! releases touch UI, packaging, or backend code that has nothing to do with
//! parsing, and must not force every transcript to be re-parsed. On a real
//! mismatch, invalidation drops and recreates the entries table instead of
//! deleting rows one at a time, so `cache_open_ms` stays cheap even for a
//! large cache. Read, decode, or database errors degrade to a cache miss; an
//! unreadable cache file is rebuilt from scratch once before caching is
//! disabled for the run.

use crate::model::Session;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, UNIX_EPOCH};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const LEGACY_CACHE_NAME: &str = "scan-cache.json";

/// Bump ONLY when parser output or the cached `Session` shape changes.
/// An app release alone (packaging, UI, unrelated backend code) must not
/// invalidate the cache — that was the old `APP_VERSION`-keyed behavior,
/// and it turned every release into a full cold re-parse of every
/// transcript.
const PARSE_VERSION: u32 = 1;

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

/// Why this scan could not treat the cache as fully warm. Absent (`None`)
/// means the stored `parse_version` matched and entries were consulted
/// normally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ColdReason {
    /// Stored `parse_version` did not match the current one, including a
    /// legacy `app_version`-only cache being converted for the first time.
    ParseVersionChanged,
    /// No cache existed yet: first run, or the database file was just created.
    CacheMissing,
    /// The cache file was unreadable and had to be rebuilt from scratch.
    CacheCorrupt,
}

/// A failed cache open becomes a disabled cache. Scanning and parsing remain
/// fully functional because this layer is never a source of truth.
#[derive(Default)]
pub struct ScanCache {
    connection: Option<Mutex<Connection>>,
    generation: i64,
    cold_reason: Option<ColdReason>,
    /// Time spent dropping and recreating the entries table on a
    /// parse-version mismatch; 0 when no invalidation happened.
    invalidation_ms: f64,
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
        match Self::open_existing(path) {
            Ok(cache) => Ok(cache),
            Err(error) => {
                // A garbage or partially-written file (e.g. a crash mid-write,
                // or a foreign file at this path) must not permanently disable
                // caching. Rebuild from scratch exactly once; a second failure
                // is a real, unrecoverable error and disables the cache for
                // this run same as before.
                tracing::warn!(
                    "scan cache at {:?} could not be opened, rebuilding: {}",
                    path,
                    error
                );
                remove_sqlite_files(path);
                let mut cache = Self::open_existing(path)?;
                cache.cold_reason = Some(ColdReason::CacheCorrupt);
                Ok(cache)
            }
        }
    }

    fn open_existing(path: &Path) -> anyhow::Result<Self> {
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
        let stored_parse_version: Option<String> = transaction
            .query_row(
                "SELECT value FROM cache_meta WHERE key = 'parse_version'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let cold_reason = match &stored_parse_version {
            Some(value) if value.parse::<u32>().ok() == Some(PARSE_VERSION) => None,
            Some(_) => Some(ColdReason::ParseVersionChanged),
            None => {
                // No `parse_version` yet. A cache written before parser
                // versioning existed still has `app_version`: treat it as one
                // final mismatch, after which it converts to parse_version
                // keying below. No `app_version` either means this cache
                // never held anything.
                let had_legacy_app_version: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM cache_meta WHERE key = 'app_version')",
                    [],
                    |row| row.get(0),
                )?;
                if had_legacy_app_version {
                    Some(ColdReason::ParseVersionChanged)
                } else {
                    Some(ColdReason::CacheMissing)
                }
            }
        };

        let mut invalidation_ms = 0.0;
        if cold_reason == Some(ColdReason::ParseVersionChanged) {
            tracing::info!("scan cache parse version mismatch; invalidating entries");
            let invalidation_started = Instant::now();
            // Drop and recreate rather than deleting rows one at a time: a
            // row-wise DELETE over a large cache is what made every release
            // cost tens of seconds of cache_open_ms even though nothing about
            // the cached data actually needed to change.
            transaction.execute_batch(
                "DROP TABLE sessions;
                 CREATE TABLE sessions (
                     path TEXT PRIMARY KEY,
                     size INTEGER NOT NULL,
                     mtime_ms INTEGER NOT NULL,
                     session_json BLOB NOT NULL,
                     seen_generation INTEGER NOT NULL
                 );",
            )?;
            invalidation_ms = invalidation_started.elapsed().as_secs_f64() * 1_000.0;
        }
        transaction.execute(
            "INSERT INTO cache_meta(key, value) VALUES('app_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [APP_VERSION],
        )?;
        transaction.execute(
            "INSERT INTO cache_meta(key, value) VALUES('parse_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [PARSE_VERSION.to_string()],
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
            cold_reason,
            invalidation_ms,
        })
    }

    /// Why this scan's cache could not be treated as fully warm; `None` when
    /// the stored parse version matched.
    pub fn cold_reason(&self) -> Option<ColdReason> {
        self.cold_reason
    }

    /// Milliseconds spent invalidating entries on a parse-version mismatch;
    /// 0 when no invalidation was needed.
    pub fn invalidation_ms(&self) -> f64 {
        self.invalidation_ms
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

/// Removes the SQLite main file and its WAL/SHM/journal siblings so a
/// corrupt or foreign file at this path can be rebuilt from scratch.
fn remove_sqlite_files(path: &Path) {
    let base = path.as_os_str().to_os_string();
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut candidate = base.clone();
        candidate.push(suffix);
        let candidate = PathBuf::from(candidate);
        if let Err(error) = std::fs::remove_file(&candidate) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("could not remove {:?}: {}", candidate, error);
            }
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
            storage_id: format!("codex:thread:{id}"),
            harness: Harness::Codex,
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            source_availability: Default::default(),
            archived: false,
            started_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            last_event_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            working_directory: None,
            originator: None,
            source: None,
            subagent_id_is_path_fallback: false,
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
            rate_limits_history: Vec::new(),
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
    fn matching_parse_version_across_opens_is_a_cache_hit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let cache = ScanCache::load(&path);
        // First-ever open of a brand new file: cold, but not a mismatch.
        assert_eq!(cache.cold_reason(), Some(ColdReason::CacheMissing));
        cache.store("a.jsonl", 100, 5_000, &session("s1"));
        cache.finish_scan();
        drop(cache);

        let cache = ScanCache::load(&path);
        assert_eq!(cache.cold_reason(), None, "same parse version reopens warm");
        assert_eq!(cache.lookup("a.jsonl", 100, 5_000).unwrap().id, "s1");
    }

    #[test]
    fn parse_version_mismatch_invalidates_without_row_by_row_deletes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        let cache = ScanCache::load(&path);
        cache.store("a.jsonl", 100, 5_000, &session("s1"));
        cache.finish_scan();
        assert_eq!(cache.len(), 1);
        drop(cache);

        // Simulate a parser change: bump the stored parse_version behind the
        // cache's back, as a future PARSE_VERSION bump would.
        {
            let raw = Connection::open(&path).unwrap();
            raw.execute(
                "UPDATE cache_meta SET value = '999' WHERE key = 'parse_version'",
                [],
            )
            .unwrap();
        }

        let cache = ScanCache::load(&path);
        assert_eq!(cache.cold_reason(), Some(ColdReason::ParseVersionChanged));
        assert_eq!(cache.len(), 0, "mismatch drops all entries");
        assert!(cache.lookup("a.jsonl", 100, 5_000).is_none());
        // The drop+recreate happens once, synchronously, inside open(); it
        // does not scale with the number of invalidated rows the way a
        // row-wise DELETE would.
        assert!(
            cache.invalidation_ms() < 2_000.0,
            "invalidation should be O(1), not proportional to cache size: {}ms",
            cache.invalidation_ms()
        );

        let raw = Connection::open(&path).unwrap();
        let stored_parse_version: String = raw
            .query_row(
                "SELECT value FROM cache_meta WHERE key = 'parse_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_parse_version, PARSE_VERSION.to_string());
    }

    #[test]
    fn legacy_app_version_cache_converts_after_one_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        {
            // Hand-build a pre-parse-version cache: cache_meta has
            // 'app_version' but no 'parse_version', with an existing entry.
            let raw = Connection::open(&path).unwrap();
            raw.execute_batch(
                "CREATE TABLE cache_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 CREATE TABLE sessions (
                     path TEXT PRIMARY KEY,
                     size INTEGER NOT NULL,
                     mtime_ms INTEGER NOT NULL,
                     session_json BLOB NOT NULL,
                     seen_generation INTEGER NOT NULL
                 );
                 INSERT INTO cache_meta(key, value) VALUES ('app_version', '0.1.0');
                 INSERT INTO cache_meta(key, value) VALUES ('generation', '1');",
            )
            .unwrap();
            raw.execute(
                "INSERT INTO sessions(path, size, mtime_ms, session_json, seen_generation)
                 VALUES ('a.jsonl', 100, 5000, ?1, 1)",
                params![serde_json::to_vec(&session("s1")).unwrap()],
            )
            .unwrap();
        }

        let cache = ScanCache::load(&path);
        assert_eq!(cache.cold_reason(), Some(ColdReason::ParseVersionChanged));
        assert_eq!(
            cache.len(),
            0,
            "legacy app_version-keyed cache is invalidated once"
        );
        drop(cache);

        let cache = ScanCache::load(&path);
        assert_eq!(
            cache.cold_reason(),
            None,
            "second open is warm under parse_version keying"
        );
    }

    #[test]
    fn corrupt_cache_file_recovers_to_a_working_cache() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.sqlite3");
        std::fs::write(&path, b"not a sqlite database, just garbage bytes").unwrap();

        let cache = ScanCache::load(&path);
        assert!(
            cache.is_enabled(),
            "a corrupt file must be rebuilt, not just disable the cache"
        );
        assert_eq!(cache.cold_reason(), Some(ColdReason::CacheCorrupt));
        cache.store("a.jsonl", 100, 5_000, &session("s1"));
        cache.finish_scan();
        assert_eq!(cache.len(), 1);
        drop(cache);

        let cache = ScanCache::load(&path);
        assert_eq!(cache.lookup("a.jsonl", 100, 5_000).unwrap().id, "s1");
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
