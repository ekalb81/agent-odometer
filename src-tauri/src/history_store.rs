//! Durable, local session history.
//!
//! This is deliberately distinct from `scan_cache`: it keeps durable sessions
//! and append-only normalized token events even when transcript files move or
//! vanish. Full `Session` blobs are replaceable current materializations so a
//! growing transcript is not copied into an unbounded snapshot history. The
//! store has no application-version invalidation path and never removes a
//! session or token event during a scan.

use crate::model::{Harness, Session, SourceAvailability, TokenHistoryPoint};
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const SCHEMA_VERSION: i64 = 2;
const SNAPSHOT_FORMAT_VERSION: i64 = 1;

/// One path at which an archived transcript has been observed. Paths are
/// availability observations, not logical-session identities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLocation {
    pub path: String,
    pub present: bool,
    pub first_seen_at_ms: i64,
    pub last_seen_at_ms: i64,
}

/// A materialized durable session together with source availability metadata.
#[derive(Debug, Clone)]
pub struct StoredSession {
    /// Durable internal key. It remains stable if a source moves.
    pub key: String,
    /// Provider/harness identity before collision disambiguation.
    pub identity_key: String,
    pub first_event_fingerprint: String,
    pub available: bool,
    /// True when more than one distinct transcript claims this provider ID.
    pub collision: bool,
    pub locations: Vec<SourceLocation>,
    pub session: Session,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HistoryStats {
    pub sessions: usize,
    pub available_sessions: usize,
    pub locations: usize,
    pub token_events: usize,
    pub collisions: usize,
}

/// Durable archive database. Errors are deliberately surfaced to callers: a
/// failed archive must not quietly behave like a disposable cache.
pub struct HistoryStore {
    connection: Mutex<Connection>,
}

impl HistoryStore {
    /// The history archive is application data, deliberately separate from
    /// the versioned scan cache. Keeping it under the platform local-data
    /// directory makes its retention independent from transcript locations
    /// and from cache eviction.
    pub fn default_path() -> Result<PathBuf> {
        let base = dirs::data_local_dir()
            .or_else(dirs::data_dir)
            .ok_or_else(|| anyhow!("could not determine a local data directory"))?;
        Ok(base.join("agent-odometer").join("history-v1.sqlite3"))
    }

    pub fn open_default() -> Result<Self> {
        let path = Self::default_path()?;
        Self::open(&path)
    }

    /// Opens or creates the archive and applies durable, forward-only schema
    /// migrations. Nothing here is keyed to `CARGO_PKG_VERSION`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!(
                    "could not create history-store directory {}",
                    parent.display()
                )
            })?;
        }
        let mut connection = Connection::open(path)
            .with_context(|| format!("could not open history store {}", path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        migrate(&mut connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    /// Starts a generation used only to determine whether a *location* was
    /// seen. Finishing a scan can mark paths missing but never purges history.
    pub fn begin_scan(&self) -> Result<i64> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let generation: i64 = transaction.query_row(
            "INSERT INTO history_meta(key, value) VALUES('scan_generation', '1')
             ON CONFLICT(key) DO UPDATE SET value = CAST(history_meta.value AS INTEGER) + 1
             RETURNING CAST(value AS INTEGER)",
            [],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(generation)
    }

    /// Records a fully parsed session at `source_path`, reconciles it to its
    /// durable identity, and returns the materialized stored session.
    pub fn observe(
        &self,
        source_path: &Path,
        session: &Session,
        generation: i64,
    ) -> Result<StoredSession> {
        Ok(self
            .observe_with_displaced(source_path, session, generation)?
            .0)
    }

    /// Like [`Self::observe`], but also returns the previous logical session
    /// displaced from this exact source location (for an in-place transcript
    /// replacement/truncation). Callers can immediately publish its updated
    /// availability instead of leaving a stale Present projection until the
    /// next bulk scan.
    pub fn observe_with_displaced(
        &self,
        source_path: &Path,
        session: &Session,
        generation: i64,
    ) -> Result<(StoredSession, Option<StoredSession>)> {
        let path = source_path_key(source_path);
        let identity = provider_identity(session)?;
        let fingerprint = first_event_fingerprint(session);
        let lineage = history_lineage(session);
        let fingerprint_is_final = !session.tokens_history.is_empty();
        let now = now_ms();

        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let displaced_key: Option<String> = transaction
            .query_row(
                "SELECT session_key FROM source_locations WHERE path = ?1",
                [path.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let key = reconcile_session(
            &transaction,
            &path,
            &identity,
            &fingerprint,
            fingerprint_is_final,
            session,
            now,
        )?;
        let mut archived_session = session.clone();
        archived_session.storage_id = key.clone();
        archived_session.source_availability = SourceAvailability::Present;
        archived_session.file_path = path.clone();
        let raw_snapshot =
            serde_json::to_vec(&archived_session).context("could not encode session snapshot")?;
        let snapshot_hash = stable_hash_bytes(&raw_snapshot);
        let artifact_key = format!(
            "artifact-{}",
            stable_hash(&format!("{identity}\u{1f}{lineage}"))
        );
        transaction.execute(
            "INSERT INTO source_artifacts(artifact_key, identity_key, first_event_fingerprint, session_key, created_at_ms, last_seen_at_ms)
             VALUES(?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(artifact_key) DO UPDATE SET last_seen_at_ms = excluded.last_seen_at_ms",
            params![artifact_key, identity, fingerprint, key, now],
        )?;
        transaction.execute(
            "INSERT INTO source_locations(path, artifact_key, session_key, present, first_seen_at_ms, last_seen_at_ms, seen_generation)
             VALUES(?1, ?2, ?3, 1, ?4, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET artifact_key = excluded.artifact_key,
                 session_key = excluded.session_key, present = 1,
                 last_seen_at_ms = excluded.last_seen_at_ms,
                 seen_generation = MAX(source_locations.seen_generation, excluded.seen_generation)",
            params![path, artifact_key, key, now, generation],
        )?;
        // Most scans see an unchanged, fully parsed snapshot. Its stable hash
        // makes it safe to skip walking/re-inserting the complete history;
        // appends and resumes necessarily change the snapshot and still take
        // the idempotent normalized-event path below.
        if store_snapshot(
            &transaction,
            &key,
            &archived_session,
            &raw_snapshot,
            &snapshot_hash,
            now,
            SnapshotPolicy::Source,
        )? {
            store_token_events(&transaction, &key, &session.tokens_history)?;
        }
        refresh_collision_flags(&transaction, &identity)?;
        transaction.commit()?;
        drop(connection);
        let current = self.load_one(&key)?;
        let displaced = displaced_key
            .filter(|previous| previous != &key)
            .map(|previous| self.load_one(&previous))
            .transpose()?;
        Ok((current, displaced))
    }

    /// Marks locations not seen in this completed, newest scan as missing.
    /// No session, artifact, snapshot, or normalized event is deleted.
    pub fn finish_scan(&self, generation: i64) -> Result<usize> {
        let connection = self.connection()?;
        let changed = connection.execute(
            "UPDATE source_locations SET present = 0
             WHERE present = 1 AND seen_generation < ?1
               AND ?1 = (SELECT CAST(value AS INTEGER) FROM history_meta WHERE key = 'scan_generation')",
            [generation],
        )?;
        Ok(changed)
    }

    /// Marks a single source observation missing without touching its archive.
    pub fn mark_path_missing(&self, path: &Path) -> Result<Option<StoredSession>> {
        let connection = self.connection()?;
        let path = source_path_key(path);
        let session_key: Option<String> = connection
            .query_row(
                "SELECT session_key FROM source_locations WHERE path = ?1",
                [path.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(session_key) = session_key else {
            return Ok(None);
        };
        connection.execute(
            "UPDATE source_locations SET present = 0 WHERE path = ?1",
            [path.as_str()],
        )?;
        Ok(Some(load_one(&connection, &session_key)?))
    }

    /// Stores a metadata-only revision of a materialized session. This is
    /// used for local overlays such as `session_index` thread names after the
    /// transcript has already been observed; it deliberately does not create
    /// or alter source locations.
    pub fn update_snapshot(&self, session: &Session) -> Result<StoredSession> {
        let key = session.effective_storage_id();
        let mut archived = session.clone();
        archived.storage_id = key.clone();
        let raw_snapshot = serde_json::to_vec(&archived)
            .context("could not encode metadata-only session snapshot")?;
        let snapshot_hash = stable_hash_bytes(&raw_snapshot);
        let now = now_ms();
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exists: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM durable_sessions WHERE session_key = ?1",
                [key.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            bail!("cannot update snapshot for unknown durable session {key}");
        }
        store_snapshot(
            &transaction,
            &key,
            &archived,
            &raw_snapshot,
            &snapshot_hash,
            now,
            SnapshotPolicy::MetadataOverlay,
        )?;
        transaction.execute(
            "UPDATE durable_sessions SET last_seen_at_ms = ?2 WHERE session_key = ?1",
            params![key, now],
        )?;
        transaction.commit()?;
        drop(connection);
        self.load_one(&key)
    }

    /// Returns all archived sessions, including those whose sources are gone.
    pub fn load_sessions(&self) -> Result<Vec<StoredSession>> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            "SELECT session_key FROM durable_sessions ORDER BY last_seen_at_ms DESC, session_key",
        )?;
        let keys = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        keys.iter().map(|key| load_one(&connection, key)).collect()
    }

    pub fn stats(&self) -> Result<HistoryStats> {
        let connection = self.connection()?;
        let sessions = count(&connection, "SELECT COUNT(*) FROM durable_sessions")?;
        let available_sessions = count(
            &connection,
            "SELECT COUNT(DISTINCT session_key) FROM source_locations WHERE present = 1",
        )?;
        let locations = count(&connection, "SELECT COUNT(*) FROM source_locations")?;
        let token_events = count(&connection, "SELECT COUNT(*) FROM durable_token_events")?;
        let collisions = count(
            &connection,
            "SELECT COUNT(*) FROM durable_sessions WHERE collision = 1",
        )?;
        Ok(HistoryStats {
            sessions,
            available_sessions,
            locations,
            token_events,
            collisions,
        })
    }

    fn load_one(&self, key: &str) -> Result<StoredSession> {
        let connection = self.connection()?;
        load_one(&connection, key)
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| anyhow!("history-store connection lock poisoned"))
    }
}

/// A source location has the same identity rules as the live path overlay:
/// notify and scanners may disagree on separators, verbatim prefixes, and
/// (on Windows) casing. Persist the normalized location key so an observe and
/// a later remove always address the same durable record.
fn source_path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    let value = value.strip_prefix(r"\\?\").unwrap_or(&value);
    let normalized = value.replace('\\', "/");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn migrate(connection: &mut Connection) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("history store schema {version} is newer than this application supports");
    }
    if version == 0 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE history_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE durable_sessions (
               session_key TEXT PRIMARY KEY,
               identity_key TEXT NOT NULL,
               first_event_fingerprint TEXT NOT NULL,
               fingerprint_is_final INTEGER NOT NULL,
               collision INTEGER NOT NULL DEFAULT 0,
               current_snapshot_version INTEGER NOT NULL DEFAULT 0,
               current_snapshot_hash TEXT,
               created_at_ms INTEGER NOT NULL,
               last_seen_at_ms INTEGER NOT NULL
             );
             CREATE INDEX durable_sessions_identity_idx ON durable_sessions(identity_key);
             CREATE TABLE source_artifacts (
               artifact_key TEXT PRIMARY KEY,
               identity_key TEXT NOT NULL,
               first_event_fingerprint TEXT NOT NULL,
               session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
               created_at_ms INTEGER NOT NULL,
               last_seen_at_ms INTEGER NOT NULL
             );
             CREATE TABLE source_locations (
               path TEXT PRIMARY KEY,
               artifact_key TEXT NOT NULL REFERENCES source_artifacts(artifact_key),
               session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
               present INTEGER NOT NULL,
               first_seen_at_ms INTEGER NOT NULL,
               last_seen_at_ms INTEGER NOT NULL,
               seen_generation INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX source_locations_session_idx ON source_locations(session_key, present);
             CREATE TABLE session_snapshots (
               session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
               version INTEGER NOT NULL,
               format_version INTEGER NOT NULL,
               snapshot_hash TEXT NOT NULL,
               captured_at_ms INTEGER NOT NULL,
               session_json BLOB NOT NULL,
               PRIMARY KEY(session_key, version)
             );
             CREATE TABLE durable_token_events (
               session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
               event_key TEXT NOT NULL,
               event_index INTEGER NOT NULL,
               timestamp_ms INTEGER NOT NULL,
               model TEXT,
               service_tier TEXT,
               request_input_tokens INTEGER,
               cumulative_total_tokens INTEGER NOT NULL,
               input_tokens INTEGER NOT NULL,
               cached_input_tokens INTEGER NOT NULL,
               output_tokens INTEGER NOT NULL,
               reasoning_output_tokens INTEGER NOT NULL,
               total_tokens INTEGER NOT NULL,
               PRIMARY KEY(session_key, event_key)
             );
             CREATE INDEX durable_token_events_session_timestamp_idx ON durable_token_events(session_key, timestamp_ms);",
        )?;
        transaction.execute(
            "INSERT INTO history_meta(key, value) VALUES('schema_version', ?1)",
            [SCHEMA_VERSION.to_string()],
        )?;
        transaction.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))?;
        transaction.commit()?;
    }
    if version == 1 {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "ALTER TABLE durable_token_events ADD COLUMN request_input_tokens INTEGER;
             INSERT INTO history_meta(key, value) VALUES('schema_version', '2')
               ON CONFLICT(key) DO UPDATE SET value = excluded.value;
             PRAGMA user_version = 2;",
        )?;
        transaction.commit()?;
    }
    Ok(())
}

fn reconcile_session(
    transaction: &Transaction<'_>,
    path: &str,
    identity: &str,
    fingerprint: &str,
    fingerprint_is_final: bool,
    incoming: &Session,
    now: i64,
) -> Result<String> {
    // A provider identity plus its first event is not sufficient to merge two
    // transcripts: copied files may later diverge. Exact histories and prefix
    // histories are one lineage; a mismatch after their shared prefix is a
    // collision that must receive its own durable session.
    let mut statement = transaction.prepare(
        "SELECT d.session_key, s.session_json
         FROM durable_sessions d JOIN session_snapshots s
           ON s.session_key = d.session_key AND s.version = d.current_snapshot_version
         WHERE d.identity_key = ?1 AND d.first_event_fingerprint = ?2",
    )?;
    let candidates = statement
        .query_map(params![identity, fingerprint], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    for (key, raw) in candidates {
        let stored: Session = serde_json::from_slice(&raw)
            .with_context(|| format!("corrupt durable session snapshot for {key}"))?;
        if histories_share_lineage(&incoming.tokens_history, &stored.tokens_history) {
            transaction.execute(
                "UPDATE durable_sessions SET last_seen_at_ms = ?2 WHERE session_key = ?1",
                params![key, now],
            )?;
            return Ok(key);
        }
    }

    // Older Claude subagent rollouts can lack a provider `agentId`, leaving
    // the parser to use the filename stem as `id`. A rename then changes the
    // normal storage key. Limit this fallback strictly to Claude subagents
    // with the same parent and a non-empty compatible event lineage.
    if is_legacy_claude_subagent(incoming) {
        let parent = incoming.parent_thread_id.as_deref().expect("checked above");
        let mut statement = transaction.prepare(
            "SELECT d.session_key, s.session_json
             FROM durable_sessions d JOIN session_snapshots s
               ON s.session_key = d.session_key AND s.version = d.current_snapshot_version",
        )?;
        let candidates = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(statement);
        for (key, raw) in candidates {
            let stored: Session = serde_json::from_slice(&raw)
                .with_context(|| format!("corrupt durable session snapshot for {key}"))?;
            if key != identity
                && is_legacy_claude_subagent(&stored)
                && stored.parent_thread_id.as_deref() == Some(parent)
                && !incoming.tokens_history.is_empty()
                && !stored.tokens_history.is_empty()
                && histories_share_lineage(&incoming.tokens_history, &stored.tokens_history)
            {
                transaction.execute(
                    "UPDATE durable_sessions SET last_seen_at_ms = ?2 WHERE session_key = ?1",
                    params![key, now],
                )?;
                return Ok(key);
            }
        }
    }

    // A short transcript can be observed before its first token event. If the
    // same path later supplies that first event, finalize that provisional row
    // rather than falsely reporting a collision with itself.
    if fingerprint_is_final {
        let provisional_from_path: Option<String> = transaction
            .query_row(
                "SELECT d.session_key FROM source_locations l JOIN durable_sessions d ON d.session_key = l.session_key
                 WHERE l.path = ?1 AND d.identity_key = ?2 AND d.fingerprint_is_final = 0",
                params![path, identity],
                |row| row.get(0),
            )
            .optional()?;
        let provisional = match provisional_from_path {
            Some(key) => Some(key),
            None => {
                let mut statement = transaction.prepare(
                    "SELECT d.session_key, s.session_json
                     FROM durable_sessions d JOIN session_snapshots s
                       ON s.session_key = d.session_key AND s.version = d.current_snapshot_version
                     WHERE d.identity_key = ?1 AND d.fingerprint_is_final = 0",
                )?;
                let candidates = statement
                    .query_map([identity], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                drop(statement);
                if let [(key, raw)] = candidates.as_slice() {
                    let stored: Session = serde_json::from_slice(raw).with_context(|| {
                        format!("corrupt provisional durable session snapshot for {key}")
                    })?;
                    provisional_metadata_matches(incoming, &stored).then_some(key.clone())
                } else {
                    None
                }
            }
        };
        if let Some(key) = provisional {
            transaction.execute(
                "UPDATE durable_sessions SET first_event_fingerprint = ?2, fingerprint_is_final = 1, last_seen_at_ms = ?3 WHERE session_key = ?1",
                params![key, fingerprint, now],
            )?;
            return Ok(key);
        }
    }

    // Provider identities are already harness-namespaced and path-independent.
    // Keeping that readable key lets every layer share one stable identifier.
    let base_key = identity.to_owned();
    let existing: Option<String> = transaction
        .query_row(
            "SELECT session_key FROM durable_sessions WHERE session_key = ?1",
            [&base_key],
            |row| row.get(0),
        )
        .optional()?;
    let key = if existing.is_some() {
        format!("{base_key}:collision:{}", &history_lineage(incoming)[..12])
    } else {
        base_key
    };
    transaction.execute(
        "INSERT INTO durable_sessions(session_key, identity_key, first_event_fingerprint, fingerprint_is_final, created_at_ms, last_seen_at_ms)
         VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
        params![key, identity, fingerprint, fingerprint_is_final as i64, now],
    )?;
    Ok(key)
}

#[derive(Clone, Copy)]
enum SnapshotPolicy {
    /// Transcript observations must never replace a more complete/newer
    /// materialized snapshot from a different copy of the same lineage.
    Source,
    /// Local overlays intentionally change display metadata without a source
    /// scan and must remain writable even when their token history is older.
    MetadataOverlay,
}

fn store_snapshot(
    transaction: &Transaction<'_>,
    key: &str,
    incoming: &Session,
    raw: &[u8],
    snapshot_hash: &str,
    now: i64,
    policy: SnapshotPolicy,
) -> Result<bool> {
    let current: Option<(i64, Option<String>, Option<Vec<u8>>)> = transaction
        .query_row(
            "SELECT d.current_snapshot_version, d.current_snapshot_hash, s.session_json
             FROM durable_sessions d LEFT JOIN session_snapshots s
               ON s.session_key = d.session_key AND s.version = d.current_snapshot_version
             WHERE d.session_key = ?1",
            [key],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((version, current_hash, current_raw)) = current else {
        bail!("missing durable session {key} while storing snapshot");
    };
    if current_hash.as_deref() == Some(snapshot_hash) {
        return Ok(false);
    }
    if matches!(policy, SnapshotPolicy::Source) {
        if let Some(current_raw) = current_raw {
            let current: Session = serde_json::from_slice(&current_raw)
                .with_context(|| format!("corrupt durable session snapshot for {key}"))?;
            if !source_snapshot_is_monotonic(incoming, &current) {
                return Ok(false);
            }
        }
    }
    let next_version = version + 1;
    transaction.execute(
        "INSERT INTO session_snapshots(session_key, version, format_version, snapshot_hash, captured_at_ms, session_json)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        params![key, next_version, SNAPSHOT_FORMAT_VERSION, snapshot_hash, now, raw],
    )?;
    transaction.execute(
        "UPDATE durable_sessions SET current_snapshot_version = ?2, current_snapshot_hash = ?3 WHERE session_key = ?1",
        params![key, next_version, snapshot_hash],
    )?;
    transaction.execute(
        "DELETE FROM session_snapshots WHERE session_key = ?1 AND version <> ?2",
        params![key, next_version],
    )?;
    Ok(true)
}

fn store_token_events(
    transaction: &Transaction<'_>,
    key: &str,
    events: &[TokenHistoryPoint],
) -> Result<()> {
    let next_event_index: usize = transaction
        .query_row(
            "SELECT COALESCE(MAX(event_index) + 1, 0) FROM durable_token_events WHERE session_key = ?1",
            [key],
            |row| row.get::<_, i64>(0),
        )?
        .try_into()
        .context("stored token event index exceeded usize")?;

    // Schema-v1 rows did not retain direct request-input evidence. Backfill
    // it once when the source is next parsed, without replaying unchanged
    // normalized events on every later append.
    let missing_request_evidence: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM durable_token_events
         WHERE session_key = ?1 AND request_input_tokens IS NULL",
        [key],
        |row| row.get(0),
    )?;
    if missing_request_evidence > 0 {
        let mut update = transaction.prepare(
            "UPDATE durable_token_events SET request_input_tokens = ?3
             WHERE session_key = ?1 AND event_key = ?2 AND request_input_tokens IS NULL",
        )?;
        for event in events.iter().take(next_event_index) {
            let Some(request_input_tokens) = event.request_input_tokens else {
                continue;
            };
            update.execute(params![
                key,
                token_event_key(event),
                to_i64(request_input_tokens)?,
            ])?;
        }
    }

    // Histories are append-only within one reconciled lineage, so only the
    // new suffix needs insertion. Preparing once also avoids reparsing the
    // statement for each new event.
    let mut statement = transaction.prepare(
        "INSERT INTO durable_token_events(
           session_key, event_key, event_index, timestamp_ms, model, service_tier, request_input_tokens,
           cumulative_total_tokens, input_tokens, cached_input_tokens, output_tokens,
           reasoning_output_tokens, total_tokens)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(session_key, event_key) DO UPDATE SET
           request_input_tokens = COALESCE(excluded.request_input_tokens, durable_token_events.request_input_tokens)",
    )?;
    for (index, event) in events.iter().enumerate().skip(next_event_index) {
        statement.execute(params![
            key,
            token_event_key(event),
            i64::try_from(index).context("token event index exceeded SQLite integer range")?,
            event.timestamp.timestamp_millis(),
            event.model,
            event.service_tier,
            event.request_input_tokens.map(to_i64).transpose()?,
            to_i64(event.total_tokens)?,
            to_i64(event.delta.input_tokens)?,
            to_i64(event.delta.cached_input_tokens)?,
            to_i64(event.delta.output_tokens)?,
            to_i64(event.delta.reasoning_output_tokens)?,
            to_i64(event.delta.total_tokens)?,
        ])?;
    }
    Ok(())
}

fn token_event_key(event: &TokenHistoryPoint) -> String {
    stable_hash(&token_event_signature(event))
}

fn refresh_collision_flags(transaction: &Transaction<'_>, identity: &str) -> Result<()> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM durable_sessions WHERE identity_key = ?1 AND fingerprint_is_final = 1",
        [identity],
        |row| row.get(0),
    )?;
    transaction.execute(
        "UPDATE durable_sessions SET collision = ?2 WHERE identity_key = ?1",
        params![identity, (count > 1) as i64],
    )?;
    Ok(())
}

fn load_one(connection: &Connection, key: &str) -> Result<StoredSession> {
    let (identity_key, fingerprint, collision, raw): (String, String, bool, Vec<u8>) = connection
        .query_row(
        "SELECT d.identity_key, d.first_event_fingerprint, d.collision, s.session_json
         FROM durable_sessions d JOIN session_snapshots s
           ON s.session_key = d.session_key AND s.version = d.current_snapshot_version
         WHERE d.session_key = ?1",
        [key],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let mut session: Session = serde_json::from_slice(&raw)
        .with_context(|| format!("corrupt durable session snapshot for {key}"))?;
    let mut statement = connection.prepare(
        "SELECT path, present, first_seen_at_ms, last_seen_at_ms FROM source_locations
         WHERE session_key = ?1 ORDER BY path",
    )?;
    let locations = statement
        .query_map([key], |row| {
            Ok(SourceLocation {
                path: row.get(0)?,
                present: row.get(1)?,
                first_seen_at_ms: row.get(2)?,
                last_seen_at_ms: row.get(3)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let available = locations.iter().any(|location| location.present);
    session.storage_id = key.to_owned();
    session.source_availability = if available {
        SourceAvailability::Present
    } else {
        SourceAvailability::Missing
    };
    if let Some(location) = locations
        .iter()
        .filter(|location| location.present)
        .max_by_key(|location| location.last_seen_at_ms)
        .or_else(|| {
            locations
                .iter()
                .max_by_key(|location| location.last_seen_at_ms)
        })
    {
        session.file_path = location.path.clone();
    }
    Ok(StoredSession {
        key: key.to_owned(),
        identity_key,
        first_event_fingerprint: fingerprint,
        available,
        collision,
        locations,
        session,
    })
}

fn provider_identity(session: &Session) -> Result<String> {
    if session.id.trim().is_empty() {
        bail!("cannot archive a session without a provider session id");
    }
    Ok(session.effective_storage_id())
}

fn first_event_fingerprint(session: &Session) -> String {
    let first_turn = session.turns.first();
    let first_event = session.tokens_history.first();
    let value = format!(
        "v1\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        session.started_at.timestamp_millis(),
        first_turn
            .map(|turn| turn.turn_id.as_str())
            .unwrap_or_default(),
        first_event
            .map(|event| event.timestamp.timestamp_millis())
            .unwrap_or_default(),
        first_event
            .and_then(|event| event.model.as_deref())
            .unwrap_or_default(),
        first_event
            .map(|event| event.total_tokens)
            .unwrap_or_default(),
        first_event
            .map(|event| event.delta.total_tokens)
            .unwrap_or_default(),
        first_event
            .map(|event| event.delta.input_tokens)
            .unwrap_or_default(),
    );
    format!("first-{}", stable_hash(&value))
}

fn is_legacy_claude_subagent(session: &Session) -> bool {
    session.harness == Harness::ClaudeCode
        && session.source.as_deref() == Some("subagent")
        && session.subagent_id_is_path_fallback
        && session
            .parent_thread_id
            .as_deref()
            .is_some_and(|parent| !parent.trim().is_empty())
}

fn histories_share_lineage(left: &[TokenHistoryPoint], right: &[TokenHistoryPoint]) -> bool {
    left.iter()
        .zip(right)
        .all(|(left, right)| token_event_signature(left) == token_event_signature(right))
}

fn provisional_metadata_matches(incoming: &Session, stored: &Session) -> bool {
    if incoming.harness != stored.harness || incoming.started_at != stored.started_at {
        return false;
    }
    if !optional_metadata_matches(&incoming.parent_thread_id, &stored.parent_thread_id)
        || !optional_metadata_matches(&incoming.forked_from_id, &stored.forked_from_id)
        || !optional_metadata_matches(&incoming.agent_path, &stored.agent_path)
        || !optional_metadata_matches(&incoming.first_user_message, &stored.first_user_message)
    {
        return false;
    }
    match (incoming.turns.first(), stored.turns.first()) {
        (Some(incoming), Some(stored)) => {
            incoming.turn_id == stored.turn_id
                && optional_metadata_matches(&incoming.started_at, &stored.started_at)
                && optional_metadata_matches(&incoming.user_message, &stored.user_message)
        }
        _ => true,
    }
}

fn optional_metadata_matches<T: PartialEq>(left: &Option<T>, right: &Option<T>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn history_lineage(session: &Session) -> String {
    let mut value = String::new();
    for event in &session.tokens_history {
        value.push_str(&token_event_signature(event));
        value.push('\u{1e}');
    }
    // Include the empty history marker so provisional sessions do not share a
    // collision suffix with an actual, but unusually small, event sequence.
    if value.is_empty() {
        value.push_str("empty");
    }
    stable_hash(&value)
}

fn token_event_signature(event: &TokenHistoryPoint) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}\u{1f}{}",
        event.timestamp.timestamp_millis(),
        event.model.as_deref().unwrap_or_default(),
        event.service_tier.as_deref().unwrap_or_default(),
        event.total_tokens,
        event.delta.input_tokens,
        event.delta.cached_input_tokens,
        event.delta.output_tokens,
        event.delta.reasoning_output_tokens,
    )
}

fn source_snapshot_is_monotonic(incoming: &Session, current: &Session) -> bool {
    incoming.tokens_history.len() > current.tokens_history.len()
        || (incoming.tokens_history.len() == current.tokens_history.len()
            && incoming.last_event_at >= current.last_event_at)
}

fn stable_hash(value: &str) -> String {
    stable_hash_bytes(value.as_bytes())
}

/// FNV-1a is sufficient for deterministic local reconciliation keys. It is
/// not used as a security boundary and avoids adding a hashing dependency.
fn stable_hash_bytes(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn to_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("token count exceeded SQLite integer range")
}

fn count(connection: &Connection, query: &str) -> Result<usize> {
    let count: i64 = connection.query_row(query, [], |row| row.get(0))?;
    usize::try_from(count).context("history-store count exceeded usize")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        CategoryMetric, Harness, OptimizationFinding, RateLimitSnapshotPoint, TokenTotals,
        ToolMetrics, ToolObservation, TurnInfo,
    };
    use chrono::{DateTime, Utc};
    use std::collections::{BTreeMap, HashMap};
    use tempfile::tempdir;

    fn timestamp(value: &str) -> DateTime<Utc> {
        value.parse().unwrap()
    }

    fn totals(input: u64) -> TokenTotals {
        TokenTotals {
            input_tokens: input,
            cached_input_tokens: input / 4,
            output_tokens: input / 2,
            reasoning_output_tokens: input / 8,
            total_tokens: input + input / 2,
        }
    }

    fn session(id: &str, first_input: u64) -> Session {
        let first = TokenHistoryPoint {
            timestamp: timestamp("2026-01-01T00:00:01Z"),
            model: Some("gpt-test".into()),
            service_tier: None,
            request_input_tokens: Some(first_input),
            total_tokens: totals(first_input).total_tokens,
            delta: totals(first_input),
        };
        Session {
            id: id.into(),
            storage_id: crate::model::storage_id_for_session(Harness::Codex, id),
            harness: Harness::Codex,
            thread_name: Some("Test".into()),
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: "ignored-by-history-store.jsonl".into(),
            source_availability: SourceAvailability::Present,
            archived: false,
            started_at: timestamp("2026-01-01T00:00:00Z"),
            last_event_at: first.timestamp,
            working_directory: None,
            originator: None,
            source: None,
            subagent_id_is_path_fallback: false,
            history_mode: None,
            memory_mode: None,
            cli_version: None,
            model_provider: None,
            model: Some("gpt-test".into()),
            service_tier: None,
            plan_type: None,
            credits_unlimited: None,
            credits_balance: None,
            context_window: None,
            latest_context_tokens: None,
            total_turns: 0,
            first_user_message: None,
            tokens_total: first.delta.clone(),
            tokens_by_model: HashMap::new(),
            tokens_history: vec![first],
            rate_limits_history: Vec::<RateLimitSnapshotPoint>::new(),
            turns: Vec::<TurnInfo>::new(),
            tool_observations: Vec::<ToolObservation>::new(),
            tool_metrics: ToolMetrics::default(),
            tool_metrics_by_model: BTreeMap::new(),
            category_totals: BTreeMap::<crate::model::TaskCategory, CategoryMetric>::new(),
            optimization_findings: Vec::<OptimizationFinding>::new(),
        }
    }

    fn store() -> (tempfile::TempDir, HistoryStore) {
        let directory = tempdir().unwrap();
        let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
        (directory, store)
    }

    #[test]
    fn migrates_schema_v1_request_evidence_forward() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE history_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO history_meta(key, value) VALUES('schema_version', '1');
                 CREATE TABLE durable_token_events (
                   session_key TEXT NOT NULL,
                   event_key TEXT NOT NULL,
                   event_index INTEGER NOT NULL,
                   timestamp_ms INTEGER NOT NULL,
                   model TEXT,
                   service_tier TEXT,
                   cumulative_total_tokens INTEGER NOT NULL,
                   input_tokens INTEGER NOT NULL,
                   cached_input_tokens INTEGER NOT NULL,
                   output_tokens INTEGER NOT NULL,
                   reasoning_output_tokens INTEGER NOT NULL,
                   total_tokens INTEGER NOT NULL,
                   PRIMARY KEY(session_key, event_key)
                 );
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        drop(connection);

        let store = HistoryStore::open(&path).unwrap();
        let connection = store.connection().unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        let request_column_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('durable_token_events') WHERE name = 'request_input_tokens'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(request_column_count, 1);
    }

    fn append_event(session: &mut Session, input: u64, at: &str) {
        let delta = totals(input);
        let point = TokenHistoryPoint {
            timestamp: timestamp(at),
            model: Some("gpt-test".into()),
            service_tier: None,
            request_input_tokens: Some(delta.input_tokens),
            total_tokens: session.tokens_total.total_tokens + delta.total_tokens,
            delta: delta.clone(),
        };
        session.tokens_total.input_tokens += delta.input_tokens;
        session.tokens_total.cached_input_tokens += delta.cached_input_tokens;
        session.tokens_total.output_tokens += delta.output_tokens;
        session.tokens_total.reasoning_output_tokens += delta.reasoning_output_tokens;
        session.tokens_total.total_tokens += delta.total_tokens;
        session.last_event_at = point.timestamp;
        session.tokens_history.push(point);
    }

    #[test]
    fn move_and_copy_reconcile_to_one_session_with_multiple_locations() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let first = store
            .observe(Path::new("one.jsonl"), &session("thread", 10), generation)
            .unwrap();
        let second = store
            .observe(Path::new("two.jsonl"), &session("thread", 10), generation)
            .unwrap();
        assert_eq!(first.key, second.key);
        assert_eq!(store.load_sessions().unwrap().len(), 1);
        assert_eq!(second.locations.len(), 2);
        assert!(second.available);
        assert_eq!(store.stats().unwrap().token_events, 1);
    }

    #[test]
    fn unchanged_observe_keeps_snapshot_and_normalized_event_counts() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let parsed = session("thread", 10);
        store
            .observe(Path::new("unchanged.jsonl"), &parsed, generation)
            .unwrap();
        let before = store.stats().unwrap();
        store
            .observe(Path::new("unchanged.jsonl"), &parsed, generation)
            .unwrap();
        assert_eq!(store.stats().unwrap(), before);
        let connection = store.connection().unwrap();
        let snapshots: i64 = connection
            .query_row("SELECT COUNT(*) FROM session_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(snapshots, 1);
    }

    #[test]
    fn older_copy_cannot_regress_a_newer_shared_lineage_snapshot() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let old = session("thread", 10);
        let mut newer = old.clone();
        append_event(&mut newer, 20, "2026-01-01T00:00:02Z");

        store
            .observe(Path::new("newer.jsonl"), &newer, generation)
            .unwrap();
        store
            .observe(Path::new("older-copy.jsonl"), &old, generation)
            .unwrap();

        let stored = store.load_sessions().unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].session.tokens_history.len(), 2);
        assert_eq!(store.stats().unwrap().token_events, 2);
        let connection = store.connection().unwrap();
        let snapshots: i64 = connection
            .query_row("SELECT COUNT(*) FROM session_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(snapshots, 1);
    }

    #[test]
    fn same_first_event_with_divergent_later_events_is_a_collision() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let mut original = session("same-provider-id", 10);
        append_event(&mut original, 20, "2026-01-01T00:00:02Z");
        let mut divergent = session("same-provider-id", 10);
        append_event(&mut divergent, 99, "2026-01-01T00:00:02Z");

        let first = store
            .observe(Path::new("original.jsonl"), &original, generation)
            .unwrap();
        let second = store
            .observe(Path::new("divergent.jsonl"), &divergent, generation)
            .unwrap();
        assert_ne!(first.key, second.key);
        let sessions = store.load_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().all(|session| session.collision));
    }

    #[test]
    fn legacy_claude_subagent_rename_reconciles_by_parent_and_lineage() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let mut original = session("old-file-stem", 10);
        original.harness = Harness::ClaudeCode;
        original.source = Some("subagent".into());
        original.subagent_id_is_path_fallback = true;
        original.parent_thread_id = Some("parent-session".into());
        original.storage_id =
            crate::model::storage_id_for_claude_subagent("parent-session", "old-file-stem");
        let mut renamed = original.clone();
        renamed.id = "new-file-stem".into();
        renamed.storage_id =
            crate::model::storage_id_for_claude_subagent("parent-session", "new-file-stem");

        let first = store
            .observe(
                Path::new("agent-old-file-stem.jsonl"),
                &original,
                generation,
            )
            .unwrap();
        let second = store
            .observe(Path::new("agent-new-file-stem.jsonl"), &renamed, generation)
            .unwrap();
        assert_eq!(first.key, second.key);
        assert_eq!(store.load_sessions().unwrap().len(), 1);
        assert_eq!(second.locations.len(), 2);
    }

    #[test]
    fn provider_identified_claude_subagents_do_not_merge_by_lineage() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let mut first = session("provider-agent-a", 10);
        first.harness = Harness::ClaudeCode;
        first.source = Some("subagent".into());
        first.parent_thread_id = Some("parent-session".into());
        first.storage_id =
            crate::model::storage_id_for_claude_subagent("parent-session", "provider-agent-a");
        let mut second = first.clone();
        second.id = "provider-agent-b".into();
        second.storage_id =
            crate::model::storage_id_for_claude_subagent("parent-session", "provider-agent-b");

        let first = store
            .observe(Path::new("agent-a.jsonl"), &first, generation)
            .unwrap();
        let second = store
            .observe(Path::new("agent-b.jsonl"), &second, generation)
            .unwrap();

        assert_ne!(first.key, second.key);
        assert_eq!(store.load_sessions().unwrap().len(), 2);
    }

    #[test]
    fn missing_source_retains_session_and_it_reconnects_when_unchanged() {
        let (_directory, store) = store();
        let first_generation = store.begin_scan().unwrap();
        let observed = store
            .observe(
                Path::new("missing.jsonl"),
                &session("thread", 10),
                first_generation,
            )
            .unwrap();
        assert!(store
            .mark_path_missing(Path::new("missing.jsonl"))
            .unwrap()
            .is_some());
        let archived = store.load_sessions().unwrap();
        assert_eq!(archived.len(), 1);
        assert!(!archived[0].available);

        let second_generation = store.begin_scan().unwrap();
        let returned = store
            .observe(
                Path::new("missing.jsonl"),
                &session("thread", 10),
                second_generation,
            )
            .unwrap();
        assert_eq!(returned.key, observed.key);
        assert!(returned.available);
        assert_eq!(store.stats().unwrap().token_events, 1);
    }

    #[test]
    fn normalized_source_location_key_matches_verbatim_and_separator_variants() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        store
            .observe(
                Path::new(r"\\?\C:\sessions\thread.jsonl"),
                &session("thread", 10),
                generation,
            )
            .unwrap();
        let marked = store
            .mark_path_missing(Path::new("C:/sessions/thread.jsonl"))
            .unwrap()
            .expect("normalized path should resolve the stored location");
        assert!(!marked.available);
    }

    #[test]
    fn in_place_replacement_returns_displaced_session_with_updated_availability() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let (first, displaced) = store
            .observe_with_displaced(Path::new("same.jsonl"), &session("thread", 10), generation)
            .unwrap();
        assert!(displaced.is_none());

        let (_, displaced) = store
            .observe_with_displaced(Path::new("same.jsonl"), &session("thread", 99), generation)
            .unwrap();
        let displaced = displaced.expect("replacement should report prior logical session");
        assert_eq!(displaced.key, first.key);
        assert!(!displaced.available);
    }

    #[test]
    fn metadata_snapshot_survives_when_source_is_missing() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let observed = store
            .observe(Path::new("named.jsonl"), &session("thread", 10), generation)
            .unwrap();
        let mut renamed = observed.session;
        renamed.thread_name = Some("Remembered name".into());
        store.update_snapshot(&renamed).unwrap();
        store.mark_path_missing(Path::new("named.jsonl")).unwrap();

        let restored = store.load_sessions().unwrap();
        assert_eq!(restored.len(), 1);
        assert_eq!(
            restored[0].session.thread_name.as_deref(),
            Some("Remembered name")
        );
        assert!(!restored[0].available);
    }

    #[test]
    fn appended_snapshot_replaces_the_previous_materialization() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        store
            .observe(
                Path::new("append.jsonl"),
                &session("thread", 10),
                generation,
            )
            .unwrap();
        let mut appended = session("thread", 10);
        appended.tokens_history.push(TokenHistoryPoint {
            timestamp: timestamp("2026-01-01T00:00:02Z"),
            model: Some("gpt-test".into()),
            service_tier: Some("fast".into()),
            request_input_tokens: Some(20),
            total_tokens: totals(30).total_tokens,
            delta: totals(20),
        });
        appended.tokens_total = totals(30);
        store
            .observe(Path::new("append.jsonl"), &appended, generation)
            .unwrap();
        store
            .observe(Path::new("append.jsonl"), &appended, generation)
            .unwrap();
        assert_eq!(store.stats().unwrap().token_events, 2);

        let connection = store.connection().unwrap();
        let snapshot_count: i64 = connection
            .query_row("SELECT COUNT(*) FROM session_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(snapshot_count, 1);
        let current_version: i64 = connection
            .query_row(
                "SELECT current_snapshot_version FROM durable_sessions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(current_version, 2);
        drop(connection);
        assert_eq!(
            store.load_sessions().unwrap()[0]
                .session
                .tokens_history
                .len(),
            2
        );
    }

    #[test]
    fn moved_provisional_session_requires_matching_start_metadata() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let mut provisional = session("reused-provider-id", 10);
        provisional.tokens_history.clear();
        provisional.tokens_total = TokenTotals::default();
        provisional.last_event_at = provisional.started_at;

        let original = store
            .observe(Path::new("old-location.jsonl"), &provisional, generation)
            .unwrap();
        let mut reused = session("reused-provider-id", 20);
        reused.started_at = timestamp("2026-02-01T00:00:00Z");
        let replacement = store
            .observe(Path::new("new-location.jsonl"), &reused, generation)
            .unwrap();

        assert_ne!(original.key, replacement.key);
        assert_eq!(store.load_sessions().unwrap().len(), 2);
    }

    #[test]
    fn moved_provisional_session_finalizes_when_start_metadata_matches() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let mut provisional = session("moved-provider-id", 10);
        provisional.tokens_history.clear();
        provisional.tokens_total = TokenTotals::default();
        provisional.last_event_at = provisional.started_at;

        let original = store
            .observe(Path::new("old-location.jsonl"), &provisional, generation)
            .unwrap();
        let finalized = store
            .observe(
                Path::new("new-location.jsonl"),
                &session("moved-provider-id", 10),
                generation,
            )
            .unwrap();

        assert_eq!(original.key, finalized.key);
        assert_eq!(store.load_sessions().unwrap().len(), 1);
        assert_eq!(finalized.locations.len(), 2);
    }

    #[test]
    fn repeated_event_signature_remains_idempotent_during_append() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let original = session("duplicate-event", 10);
        store
            .observe(Path::new("duplicate-event.jsonl"), &original, generation)
            .unwrap();

        let mut appended = original.clone();
        appended
            .tokens_history
            .push(original.tokens_history[0].clone());
        append_event(&mut appended, 20, "2026-01-01T00:00:02Z");
        store
            .observe(Path::new("duplicate-event.jsonl"), &appended, generation)
            .unwrap();

        // The full snapshot keeps every parsed record; the normalized ledger
        // deduplicates the repeated provider event signature.
        assert_eq!(
            store.load_sessions().unwrap()[0]
                .session
                .tokens_history
                .len(),
            3
        );
        assert_eq!(store.stats().unwrap().token_events, 2);
    }

    #[test]
    fn divergent_first_event_creates_and_flags_a_collision() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let original = store
            .observe(
                Path::new("original.jsonl"),
                &session("same-provider-id", 10),
                generation,
            )
            .unwrap();
        let collision = store
            .observe(
                Path::new("replacement.jsonl"),
                &session("same-provider-id", 99),
                generation,
            )
            .unwrap();
        assert_ne!(original.key, collision.key);
        let sessions = store.load_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.iter().all(|item| item.collision));
        assert_eq!(store.stats().unwrap().collisions, 2);
    }

    #[test]
    fn finished_scan_marks_unseen_locations_missing_without_deleting_history() {
        let (_directory, store) = store();
        let first_generation = store.begin_scan().unwrap();
        store
            .observe(
                Path::new("seen.jsonl"),
                &session("seen", 10),
                first_generation,
            )
            .unwrap();
        store
            .observe(
                Path::new("gone.jsonl"),
                &session("gone", 10),
                first_generation,
            )
            .unwrap();
        let second_generation = store.begin_scan().unwrap();
        store
            .observe(
                Path::new("seen.jsonl"),
                &session("seen", 10),
                second_generation,
            )
            .unwrap();
        assert_eq!(store.finish_scan(second_generation).unwrap(), 1);
        let sessions = store.load_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.iter().filter(|item| item.available).count(), 1);
        assert_eq!(store.stats().unwrap().token_events, 2);
    }
}
