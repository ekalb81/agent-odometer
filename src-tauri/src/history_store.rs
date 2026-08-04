//! Durable, local session history.
//!
//! This is deliberately distinct from `scan_cache`: it keeps durable sessions
//! and append-only normalized token events even when transcript files move or
//! vanish. Full `Session` blobs are replaceable current materializations so a
//! growing transcript is not copied into an unbounded snapshot history. The
//! store has no application-version invalidation path and never removes a
//! session or token event during a scan.

use crate::model::{
    OptimizationFinding, OptimizationSummary, RangeTotals, RangeWindow, Session,
    SourceAvailability, TierBucket, TokenHistoryPoint, TokenTotals, ToolKind, ToolMetrics,
    ToolObservation, ToolOutcome,
};
use crate::provider::{claude_code_provider_id, codex_provider_id};
use anyhow::{anyhow, bail, Context, Result};
use chrono::TimeZone;
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

const SCHEMA_VERSION: i64 = 5;
const SNAPSHOT_FORMAT_VERSION: i64 = 1;
/// Rollup grain for the durable-ledger read path (#107): every hour bucket
/// is `floor(timestamp_ms / HOUR_MS)`, an integer that both Rust and the
/// migration's SQL use identically so the two never disagree on bucketing.
const HOUR_MS: i64 = 3_600_000;

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
    /// Retained so aggregation can open dedicated read connections instead
    /// of serializing behind the writer mutex (WAL permits concurrent reads).
    path: PathBuf,
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
            path: path.to_path_buf(),
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
            store_tool_events(&transaction, &key, &session.tool_observations)?;
            store_finding_events(&transaction, &key, &session.optimization_findings)?;
        }
        // A successful observe realigns snapshot and facts; any overlay-set
        // dirty marking is resolved.
        transaction.execute(
            "UPDATE durable_sessions SET ledger_dirty = 0 WHERE session_key = ?1",
            [key.as_str()],
        )?;
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
        // A metadata overlay may legitimately carry history that differs from
        // the durable snapshot (its caller's in-memory copy can be ahead when
        // an observe failed, or behind by design). Fact tables are only ever
        // written by the observe path's monotonic flow — an overlay that
        // advances the snapshot past the facts marks the session dirty so
        // aggregation computes it from memory, durably, until the next
        // successful observe realigns everything and clears the flag.
        let history_matches_durable = {
            let current: Option<Vec<u8>> = transaction
                .query_row(
                    "SELECT s.session_json FROM durable_sessions d
                     JOIN session_snapshots s
                       ON s.session_key = d.session_key
                      AND s.version = d.current_snapshot_version
                     WHERE d.session_key = ?1",
                    [key.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            match current.and_then(|raw| serde_json::from_slice::<Session>(&raw).ok()) {
                Some(durable) => {
                    durable.tokens_history.len() == archived.tokens_history.len()
                        && durable.tokens_history.last().map(|point| point.timestamp)
                            == archived.tokens_history.last().map(|point| point.timestamp)
                        && durable.tool_observations.len() == archived.tool_observations.len()
                        && durable.optimization_findings.len()
                            == archived.optimization_findings.len()
                }
                None => false,
            }
        };
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
            "UPDATE durable_sessions SET last_seen_at_ms = ?2, ledger_dirty = ?3 WHERE session_key = ?1",
            params![key, now, !history_matches_durable],
        )?;
        transaction.commit()?;
        drop(connection);
        self.load_one(&key)
    }

    /// Window-scoped rollups computed from the normalized ledger (#107).
    /// Whole hour buckets are served from the `rollup_*` tables — a few rows
    /// per session, not a re-materialization of every event — and only the
    /// (at most two) sub-hour edges of each window fall back to exact event
    /// reads. The three non-additive `ToolMetrics` fields are never summed
    /// across buckets: they are always derived, via the same
    /// `telemetry::mutation_chain_fields` formula the in-memory path uses,
    /// from merged `(turn_id, target)` chain counts, so they stay exact
    /// regardless of how a window's buckets and edges are combined.
    /// Timestamps carry the ledger's millisecond storage granularity, which
    /// the oracle also compares at. Returns one map per window containing
    /// only sessions with data in that window (the `sessions_in_ranges` wire
    /// contract).
    pub fn range_totals_multi(
        &self,
        session_keys: &[String],
        windows: &[RangeWindow],
    ) -> Result<Vec<HashMap<String, RangeTotals>>> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("could not open history reader {}", self.path.display()))?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let mut out: Vec<HashMap<String, RangeTotals>> = vec![HashMap::new(); windows.len()];

        let window_ms: Vec<(Option<i64>, Option<i64>)> = windows
            .iter()
            .map(|(from, to)| {
                (
                    from.map(|value| value.timestamp_millis()),
                    to.map(|value| value.timestamp_millis()),
                )
            })
            .collect();
        let plans: Vec<WindowPlan> = window_ms
            .iter()
            .map(|(from_ms, to_ms)| plan_window(*from_ms, *to_ms))
            .collect();
        // The union of every window's sub-hour edges, deduplicated, so each
        // session issues at most one edge query per fact table regardless of
        // how many windows were requested.
        let mut edge_ranges: Vec<(i64, i64)> = Vec::new();
        for plan in &plans {
            for edge in &plan.edges {
                if !edge_ranges.contains(edge) {
                    edge_ranges.push(*edge);
                }
            }
        }

        let mut token_rollup_query = connection.prepare(
            "SELECT hour_bucket, model, service_tier, input_tokens, cached_input_tokens,
                    output_tokens, reasoning_output_tokens, total_tokens, cache_creation_input_tokens
             FROM rollup_token_totals WHERE session_key = ?1",
        )?;
        let mut tool_rollup_query = connection.prepare(
            "SELECT hour_bucket, model, calls, reads, searches, mutations, commands, other,
                    successes, failures, unknown, duration_ms, output_bytes
             FROM rollup_tool_metrics WHERE session_key = ?1",
        )?;
        let mut chain_rollup_query = connection.prepare(
            "SELECT hour_bucket, model, turn_id, target, mutation_count
             FROM rollup_mutation_chains WHERE session_key = ?1",
        )?;
        let mut finding_query = connection.prepare(
            "SELECT timestamp_ms, rule_id, severity, avoidable_calls
             FROM durable_finding_events WHERE session_key = ?1",
        )?;
        let edge_predicate = edge_predicate_sql(&edge_ranges);
        let mut token_edge_query = (!edge_ranges.is_empty())
            .then(|| {
                connection.prepare(&format!(
                    "SELECT timestamp_ms, model, service_tier, request_input_tokens,
                            cumulative_total_tokens, input_tokens, cached_input_tokens,
                            output_tokens, reasoning_output_tokens, total_tokens,
                            cache_creation_input_tokens
                     FROM durable_token_events WHERE session_key = ?1 AND ({edge_predicate})
                     ORDER BY timestamp_ms, event_index"
                ))
            })
            .transpose()?;
        let mut tool_edge_query = (!edge_ranges.is_empty())
            .then(|| {
                connection.prepare(&format!(
                    "SELECT timestamp_ms, model, kind, outcome, turn_id, target, duration_ms, output_bytes
                     FROM durable_tool_events WHERE session_key = ?1 AND ({edge_predicate})
                     ORDER BY timestamp_ms"
                ))
            })
            .transpose()?;

        for key in session_keys {
            let token_rows: Vec<(i64, String, String, TokenTotals)> = token_rollup_query
                .query_map([key.as_str()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        TokenTotals {
                            input_tokens: row.get::<_, i64>(3)? as u64,
                            cached_input_tokens: row.get::<_, i64>(4)? as u64,
                            output_tokens: row.get::<_, i64>(5)? as u64,
                            reasoning_output_tokens: row.get::<_, i64>(6)? as u64,
                            total_tokens: row.get::<_, i64>(7)? as u64,
                            cache_creation_input_tokens: row.get::<_, i64>(8)? as u64,
                        },
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?;
            let tool_rows: Vec<(i64, String, ToolMetrics)> = tool_rollup_query
                .query_map([key.as_str()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        ToolMetrics {
                            calls: row.get::<_, i64>(2)? as u64,
                            reads: row.get::<_, i64>(3)? as u64,
                            searches: row.get::<_, i64>(4)? as u64,
                            mutations: row.get::<_, i64>(5)? as u64,
                            commands: row.get::<_, i64>(6)? as u64,
                            other: row.get::<_, i64>(7)? as u64,
                            successes: row.get::<_, i64>(8)? as u64,
                            failures: row.get::<_, i64>(9)? as u64,
                            unknown: row.get::<_, i64>(10)? as u64,
                            duration_ms: row.get::<_, i64>(11)? as u64,
                            output_bytes: row.get::<_, i64>(12)? as u64,
                            ..Default::default()
                        },
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?;
            let chain_rows: Vec<(i64, String, String, String, u64)> = chain_rollup_query
                .query_map([key.as_str()], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, i64>(4)? as u64,
                    ))
                })?
                .collect::<std::result::Result<_, _>>()?;
            let edge_params = |key: &str| -> Vec<Box<dyn rusqlite::ToSql>> {
                let mut params: Vec<Box<dyn rusqlite::ToSql>> =
                    Vec::with_capacity(1 + edge_ranges.len() * 2);
                params.push(Box::new(key.to_owned()));
                for (a, b) in &edge_ranges {
                    params.push(Box::new(*a));
                    params.push(Box::new(*b));
                }
                params
            };
            let edge_tokens: Vec<TokenHistoryPoint> = match token_edge_query.as_mut() {
                Some(query) => {
                    let params = edge_params(key);
                    let refs: Vec<&dyn rusqlite::ToSql> =
                        params.iter().map(|value| value.as_ref()).collect();
                    query
                        .query_map(refs.as_slice(), |row| {
                            Ok(TokenHistoryPoint {
                                timestamp: chrono::Utc
                                    .timestamp_millis_opt(row.get::<_, i64>(0)?)
                                    .single()
                                    .unwrap_or_default(),
                                model: row.get(1)?,
                                service_tier: row.get(2)?,
                                request_input_tokens: row
                                    .get::<_, Option<i64>>(3)?
                                    .map(|value| value as u64),
                                total_tokens: row.get::<_, i64>(4)? as u64,
                                delta: TokenTotals {
                                    input_tokens: row.get::<_, i64>(5)? as u64,
                                    cached_input_tokens: row.get::<_, i64>(6)? as u64,
                                    output_tokens: row.get::<_, i64>(7)? as u64,
                                    reasoning_output_tokens: row.get::<_, i64>(8)? as u64,
                                    total_tokens: row.get::<_, i64>(9)? as u64,
                                    cache_creation_input_tokens: row.get::<_, i64>(10)? as u64,
                                },
                            })
                        })?
                        .collect::<std::result::Result<_, _>>()?
                }
                None => Vec::new(),
            };
            let edge_tools: Vec<ToolObservation> = match tool_edge_query.as_mut() {
                Some(query) => {
                    let params = edge_params(key);
                    let refs: Vec<&dyn rusqlite::ToSql> =
                        params.iter().map(|value| value.as_ref()).collect();
                    query
                        .query_map(refs.as_slice(), |row| {
                            Ok(ToolObservation {
                                call_id: String::new(),
                                turn_id: row.get(4)?,
                                // Not persisted: metric reconstruction reads
                                // only kind/outcome/model/turn/target/
                                // duration/bytes. Any future metric keyed on
                                // harness or name must extend the fact schema
                                // first.
                                harness: codex_provider_id(),
                                model: row.get(1)?,
                                timestamp: chrono::Utc
                                    .timestamp_millis_opt(row.get::<_, i64>(0)?)
                                    .single()
                                    .unwrap_or_default(),
                                kind: tool_kind_from_str(&row.get::<_, String>(2)?),
                                name: String::new(),
                                providers: Vec::new(),
                                effective_tools: Vec::new(),
                                target: row.get(5)?,
                                resource_id: None,
                                outcome: tool_outcome_from_str(&row.get::<_, String>(3)?),
                                duration_ms: row
                                    .get::<_, Option<i64>>(6)?
                                    .map(|value| value as u64),
                                output_bytes: row.get::<_, i64>(7)? as u64,
                            })
                        })?
                        .collect::<std::result::Result<_, _>>()?
                }
                None => Vec::new(),
            };
            let findings: Vec<OptimizationFinding> = finding_query
                .query_map([key.as_str()], |row| {
                    Ok(OptimizationFinding {
                        timestamp: row
                            .get::<_, Option<i64>>(0)?
                            .and_then(|ms| chrono::Utc.timestamp_millis_opt(ms).single()),
                        rule_id: row.get(1)?,
                        severity: row.get(2)?,
                        avoidable_calls: row.get::<_, i64>(3)? as u64,
                        ..OptimizationFinding::default()
                    })
                })?
                .collect::<std::result::Result<_, _>>()?;

            for (window_index, ((from_ms, to_ms), plan)) in window_ms.iter().zip(&plans).enumerate()
            {
                let range = compute_range_totals(
                    *from_ms,
                    *to_ms,
                    plan,
                    &token_rows,
                    &tool_rows,
                    &chain_rows,
                    &edge_tokens,
                    &edge_tools,
                    &findings,
                );
                if crate::commands::range_has_data(&range) {
                    out[window_index].insert(key.clone(), range);
                }
            }
        }
        Ok(out)
    }

    /// Sessions whose ledger facts cannot be trusted (an overlay advanced
    /// their snapshot while an observe failure left facts behind). Survives
    /// restarts, unlike the in-process stale set.
    pub fn dirty_session_keys(&self) -> Result<Vec<String>> {
        let connection = self.connection()?;
        let mut query = connection
            .prepare("SELECT session_key FROM durable_sessions WHERE ledger_dirty = 1")?;
        let keys = query
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(keys)
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
    crate::paths::normalized_path_key(path, false)
}

/// Hour-bucket grain shared by the write path (rollup maintenance), the v4
/// migration's SQL backfill, and the read path's bucket-range math. Plain
/// truncating division, matching SQLite's integer `/`; realistic timestamps
/// are always non-negative so this never needs `div_euclid`.
fn hour_bucket(timestamp_ms: i64) -> i64 {
    timestamp_ms / HOUR_MS
}

/// Rollup grouping columns (model/tier/turn/target) use an empty string, not
/// SQL NULL, as the "absent" sentinel: a `UNIQUE INDEX` treats every NULL as
/// distinct, which would silently defeat upsert accumulation for the
/// unattributed-event bucket. Real values are never empty (parsers only ever
/// produce `None` or a genuine identifier), so the sentinel cannot collide.
fn key_sentinel(value: Option<&str>) -> &str {
    value.unwrap_or("")
}

fn sentinel_to_option(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_owned())
    }
}

/// One `[from, to]` window decomposed into a whole-hour range servable
/// entirely from rollups, plus the (at most two) millisecond sub-ranges at
/// its ends that still need exact per-event reads. A window that does not
/// fully contain any hour bucket — a genuinely sub-hour query, or one that
/// lands entirely between two hour starts — has no rollup-coverable range at
/// all; its whole span becomes the sole edge.
struct WindowPlan {
    full_buckets: Option<(i64, i64)>,
    edges: Vec<(i64, i64)>,
}

fn plan_window(from_ms: Option<i64>, to_ms: Option<i64>) -> WindowPlan {
    if let (Some(from_ms), Some(to_ms)) = (from_ms, to_ms) {
        if from_ms > to_ms {
            return WindowPlan {
                full_buckets: None,
                edges: Vec::new(),
            };
        }
    }
    // The first bucket entirely at-or-after `from`, and the last bucket
    // entirely at-or-before `to` (both inclusive bounds, matching
    // `Session::range_totals_multi`'s own event comparisons).
    let first_full = from_ms.map(|from_ms| {
        if from_ms % HOUR_MS == 0 {
            from_ms / HOUR_MS
        } else {
            from_ms / HOUR_MS + 1
        }
    });
    let last_full = to_ms.map(|to_ms| {
        if (to_ms + 1) % HOUR_MS == 0 {
            to_ms / HOUR_MS
        } else {
            to_ms / HOUR_MS - 1
        }
    });
    if let (Some(first), Some(last)) = (first_full, last_full) {
        if first > last {
            return WindowPlan {
                full_buckets: None,
                edges: vec![(from_ms.unwrap(), to_ms.unwrap())],
            };
        }
    }
    let mut edges = Vec::new();
    if let (Some(from_ms), Some(first)) = (from_ms, first_full) {
        let boundary = first * HOUR_MS;
        if boundary > from_ms {
            edges.push((from_ms, boundary - 1));
        }
    }
    if let (Some(to_ms), Some(last)) = (to_ms, last_full) {
        let boundary = (last + 1) * HOUR_MS;
        if boundary <= to_ms {
            edges.push((boundary, to_ms));
        }
    }
    WindowPlan {
        full_buckets: Some((
            first_full.unwrap_or(i64::MIN),
            last_full.unwrap_or(i64::MAX),
        )),
        edges,
    }
}

/// Builds the `(timestamp_ms BETWEEN ?n AND ?n+1) OR ...` predicate for the
/// union of edge ranges, with parameter placeholders starting at `?2` (`?1`
/// is reserved for `session_key`).
fn edge_predicate_sql(edge_ranges: &[(i64, i64)]) -> String {
    (0..edge_ranges.len())
        .map(|index| {
            format!(
                "(timestamp_ms BETWEEN ?{} AND ?{})",
                index * 2 + 2,
                index * 2 + 3
            )
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Merges one session's hour-bucket rollup rows with its exact sub-hour edge
/// events into the `RangeTotals` for one window. The additive fields are
/// plain sums; `mutation_targets`/`one_shot_mutations`/`retry_count` are
/// always derived from merged `(turn_id, target)` chain counts via
/// `telemetry::mutation_chain_fields`, never summed across buckets, so a
/// chain that straddles a bucket or day boundary is still counted exactly
/// once. `token_rows`/`tool_rows`/`chain_rows` hold every rollup row for the
/// session (a handful, cheap to filter in Rust); `edge_tokens`/`edge_tools`
/// hold the pooled edge events for every window in the batch, so a row that
/// belongs to a *different* window's edge but happens to fall inside *this*
/// window's full-bucket range is excluded — it is already counted via that
/// bucket's rollup row.
#[allow(clippy::too_many_arguments)]
fn compute_range_totals(
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    plan: &WindowPlan,
    token_rows: &[(i64, String, String, TokenTotals)],
    tool_rows: &[(i64, String, ToolMetrics)],
    chain_rows: &[(i64, String, String, String, u64)],
    edge_tokens: &[TokenHistoryPoint],
    edge_tools: &[ToolObservation],
    findings: &[OptimizationFinding],
) -> RangeTotals {
    let in_bucket_range = |bucket: i64| {
        plan.full_buckets
            .is_some_and(|(first, last)| bucket >= first && bucket <= last)
    };
    let in_window =
        |ms: i64| from_ms.is_none_or(|from| ms >= from) && to_ms.is_none_or(|to| ms <= to);

    let mut tokens = TokenTotals::default();
    let mut bucket_map: BTreeMap<(String, Option<String>), TokenTotals> = BTreeMap::new();
    for (bucket, model, tier, delta) in token_rows {
        if !in_bucket_range(*bucket) {
            continue;
        }
        tokens += delta;
        if !model.is_empty() {
            *bucket_map
                .entry((model.clone(), sentinel_to_option(tier)))
                .or_default() += delta;
        }
    }
    for point in edge_tokens {
        let ms = point.timestamp.timestamp_millis();
        if !in_window(ms) || in_bucket_range(hour_bucket(ms)) {
            continue;
        }
        tokens += &point.delta;
        if let Some(model) = &point.model {
            *bucket_map
                .entry((model.clone(), point.service_tier.clone()))
                .or_default() += &point.delta;
        }
    }
    let mut buckets: Vec<TierBucket> = bucket_map
        .into_iter()
        .map(|((model, service_tier), tokens)| TierBucket {
            model,
            service_tier,
            tokens,
        })
        .collect();
    buckets.sort_by(|a, b| {
        a.model
            .cmp(&b.model)
            .then_with(|| a.service_tier.cmp(&b.service_tier))
    });

    let mut all_counters = ToolMetrics::default();
    let mut by_model_counters: BTreeMap<String, ToolMetrics> = BTreeMap::new();
    let mut all_chain: HashMap<(String, String), u64> = HashMap::new();
    let mut by_model_chain: BTreeMap<String, HashMap<(String, String), u64>> = BTreeMap::new();

    for (bucket, model, counters) in tool_rows {
        if !in_bucket_range(*bucket) {
            continue;
        }
        all_counters.add_assign(counters);
        if !model.is_empty() {
            by_model_counters
                .entry(model.clone())
                .or_default()
                .add_assign(counters);
        }
    }
    for (bucket, model, turn, target, count) in chain_rows {
        if !in_bucket_range(*bucket) {
            continue;
        }
        *all_chain.entry((turn.clone(), target.clone())).or_insert(0) += *count;
        if !model.is_empty() {
            *by_model_chain
                .entry(model.clone())
                .or_default()
                .entry((turn.clone(), target.clone()))
                .or_insert(0) += *count;
        }
    }
    for item in edge_tools {
        let ms = item.timestamp.timestamp_millis();
        if !in_window(ms) || in_bucket_range(hour_bucket(ms)) {
            continue;
        }
        crate::telemetry::accumulate_observation(&mut all_counters, item);
        let is_mutation = item.kind == ToolKind::Mutation;
        let chain_key = (
            item.turn_id.clone().unwrap_or_default(),
            item.target.clone().unwrap_or_default(),
        );
        if is_mutation {
            *all_chain.entry(chain_key.clone()).or_insert(0) += 1;
        }
        if let Some(model) = &item.model {
            let entry = by_model_counters.entry(model.clone()).or_default();
            crate::telemetry::accumulate_observation(entry, item);
            if is_mutation {
                *by_model_chain
                    .entry(model.clone())
                    .or_default()
                    .entry(chain_key)
                    .or_insert(0) += 1;
            }
        }
    }

    let (mutation_targets, one_shot_mutations, retry_count) =
        crate::telemetry::mutation_chain_fields(all_chain.values().copied());
    all_counters.mutation_targets = mutation_targets;
    all_counters.one_shot_mutations = one_shot_mutations;
    all_counters.retry_count = retry_count;

    let mut tool_metrics_by_model = BTreeMap::new();
    for (model, mut counters) in by_model_counters {
        let (targets, one_shot, retry) = crate::telemetry::mutation_chain_fields(
            by_model_chain
                .get(&model)
                .into_iter()
                .flat_map(|chains| chains.values().copied()),
        );
        counters.mutation_targets = targets;
        counters.one_shot_mutations = one_shot;
        counters.retry_count = retry;
        tool_metrics_by_model.insert(model, counters);
    }

    let selected_findings: Vec<&OptimizationFinding> = findings
        .iter()
        .filter(|finding| match finding.timestamp {
            Some(timestamp) => in_window(timestamp.timestamp_millis()),
            None => from_ms.is_none() && to_ms.is_none(),
        })
        .collect();
    let optimization_findings_count = selected_findings.len() as u64;
    let optimization_summary = OptimizationSummary::from_findings(selected_findings);

    RangeTotals {
        tokens,
        buckets,
        tool_metrics: all_counters,
        tool_metrics_by_model,
        optimization_findings_count,
        optimization_summary,
    }
}

/// True when `table` already has a column named `column`. Used to guard an
/// `ALTER TABLE ... ADD COLUMN` in a migration step that may run against a
/// table whose exact history (freshly created this call vs. pre-existing)
/// cannot be inferred reliably from the schema-version transition alone.
fn table_has_column(connection: &Transaction<'_>, table: &str, column: &str) -> Result<bool> {
    let count: i64 = connection.query_row(
        &format!("SELECT COUNT(*) FROM pragma_table_info('{table}') WHERE name = ?1"),
        [column],
        |row| row.get(0),
    )?;
    Ok(count > 0)
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
               last_seen_at_ms INTEGER NOT NULL,
               ledger_dirty INTEGER NOT NULL DEFAULT 0
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
               cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0,
               output_tokens INTEGER NOT NULL,
               reasoning_output_tokens INTEGER NOT NULL,
               total_tokens INTEGER NOT NULL,
               PRIMARY KEY(session_key, event_key)
             );
             CREATE INDEX durable_token_events_session_timestamp_idx ON durable_token_events(session_key, timestamp_ms);
             CREATE TABLE durable_tool_events (
               session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
               timestamp_ms INTEGER NOT NULL,
               model TEXT,
               kind TEXT NOT NULL,
               outcome TEXT NOT NULL,
               turn_id TEXT,
               target TEXT,
               duration_ms INTEGER,
               output_bytes INTEGER NOT NULL
             );
             CREATE INDEX durable_tool_events_session_timestamp_idx ON durable_tool_events(session_key, timestamp_ms);
             CREATE TABLE durable_finding_events (
               session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
               timestamp_ms INTEGER,
               rule_id TEXT NOT NULL,
               severity TEXT NOT NULL,
               avoidable_calls INTEGER NOT NULL
             );
             CREATE INDEX durable_finding_events_session_idx ON durable_finding_events(session_key);"
        )?;
        transaction.execute_batch(ROLLUP_SCHEMA_SQL)?;
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
    if (1..=2).contains(&version) {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "CREATE TABLE durable_tool_events (
               session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
               timestamp_ms INTEGER NOT NULL,
               model TEXT,
               kind TEXT NOT NULL,
               outcome TEXT NOT NULL,
               turn_id TEXT,
               target TEXT,
               duration_ms INTEGER,
               output_bytes INTEGER NOT NULL
             );
             CREATE INDEX durable_tool_events_session_timestamp_idx ON durable_tool_events(session_key, timestamp_ms);
             CREATE TABLE durable_finding_events (
               session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
               timestamp_ms INTEGER,
               rule_id TEXT NOT NULL,
               severity TEXT NOT NULL,
               avoidable_calls INTEGER NOT NULL
             );
             CREATE INDEX durable_finding_events_session_idx ON durable_finding_events(session_key);
             ALTER TABLE durable_sessions ADD COLUMN ledger_dirty INTEGER NOT NULL DEFAULT 0;",
        )?;
        // Backfill facts for every existing session from its current snapshot
        // so ledger-backed range queries cover historical data immediately.
        // A snapshot that no longer deserializes is skipped with a warning:
        // its facts repopulate on that session's next observe.
        {
            // Streamed: snapshots can serialize to megabytes each, so the
            // cursor row is the only blob resident at a time. Interleaved
            // inserts on other tables are safe while the cursor is open.
            let mut select = transaction.prepare(
                "SELECT d.session_key, s.session_json
                 FROM durable_sessions d JOIN session_snapshots s
                   ON s.session_key = d.session_key
                  AND s.version = d.current_snapshot_version",
            )?;
            let mut rows = select.query([])?;
            while let Some(row) = rows.next()? {
                let key: String = row.get(0)?;
                let raw: Vec<u8> = row.get(1)?;
                match serde_json::from_slice::<Session>(&raw) {
                    Ok(session) => {
                        // Facts only: the rollup tables (#107) do not exist
                        // yet at this schema version, and the v3->v4 step
                        // right after this one populates them from these
                        // same facts via one SQL aggregate pass.
                        store_tool_event_facts(&transaction, &key, &session.tool_observations)?;
                        store_finding_events(&transaction, &key, &session.optimization_findings)?;
                    }
                    Err(error) => {
                        // NOTE: a skipped session only heals when its snapshot
                        // next changes (the observe hash gate); it stays
                        // fact-less until then and is undercounted by ledger
                        // aggregation. In practice a blob that fails here also
                        // fails observe-side deserialization and gets marked
                        // stale, routing it through memory.
                        tracing::warn!(
                            "could not backfill facts for {key}: snapshot did not deserialize: {error}"
                        );
                    }
                }
            }
        }
        transaction.execute_batch(
            "INSERT INTO history_meta(key, value) VALUES('schema_version', '3')
               ON CONFLICT(key) DO UPDATE SET value = excluded.value;
             PRAGMA user_version = 3;",
        )?;
        transaction.commit()?;
    }
    // Re-read rather than trust the initial `version` local: the two blocks
    // above run only when the *original* version matched, so a database that
    // started at 1 or 2 has already been brought to 3 by the time execution
    // reaches here, in the same `migrate()` call.
    let version_before_rollups: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version_before_rollups == 3 {
        // Adds the #107 hour-bucket rollups. Populated once here directly
        // from the existing normalized fact tables via SQL aggregation —
        // never by re-parsing transcripts or re-materializing full Session
        // snapshots. `timestamp_ms / HOUR_MS` matches the Rust `hour_bucket`
        // helper the write path and read path both use, and `COALESCE(...,
        // '')` matches `key_sentinel`'s empty-string stand-in for a missing
        // model/tier/turn/target so grouping here and later upserts agree.
        // `durable_token_events` does not have `cache_creation_input_tokens`
        // yet at this point in a fresh v3->v4 upgrade (the v4->v5 step below
        // adds it), so this backfill cannot and does not reference it;
        // `rollup_token_totals.cache_creation_input_tokens` keeps its schema
        // default of 0 here and is reconciled by the v4->v5 step instead.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(ROLLUP_SCHEMA_SQL)?;
        transaction.execute_batch(
            "INSERT INTO rollup_token_totals(
               session_key, hour_bucket, model, service_tier,
               input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens, total_tokens)
             SELECT session_key, timestamp_ms / 3600000, COALESCE(model, ''), COALESCE(service_tier, ''),
               SUM(input_tokens), SUM(cached_input_tokens), SUM(output_tokens),
               SUM(reasoning_output_tokens), SUM(total_tokens)
             FROM durable_token_events
             GROUP BY session_key, timestamp_ms / 3600000, COALESCE(model, ''), COALESCE(service_tier, '');

             INSERT INTO rollup_tool_metrics(
               session_key, hour_bucket, model, calls, reads, searches, mutations, commands, other,
               successes, failures, unknown, duration_ms, output_bytes)
             SELECT session_key, timestamp_ms / 3600000, COALESCE(model, ''),
               COUNT(*),
               SUM(CASE WHEN kind = 'read' THEN 1 ELSE 0 END),
               SUM(CASE WHEN kind = 'search' THEN 1 ELSE 0 END),
               SUM(CASE WHEN kind = 'mutation' THEN 1 ELSE 0 END),
               SUM(CASE WHEN kind = 'command' THEN 1 ELSE 0 END),
               SUM(CASE WHEN kind = 'other' THEN 1 ELSE 0 END),
               SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END),
               SUM(CASE WHEN outcome = 'failure' THEN 1 ELSE 0 END),
               SUM(CASE WHEN outcome NOT IN ('success', 'failure') THEN 1 ELSE 0 END),
               SUM(COALESCE(duration_ms, 0)),
               SUM(output_bytes)
             FROM durable_tool_events
             GROUP BY session_key, timestamp_ms / 3600000, COALESCE(model, '');

             INSERT INTO rollup_mutation_chains(session_key, hour_bucket, model, turn_id, target, mutation_count)
             SELECT session_key, timestamp_ms / 3600000, COALESCE(model, ''), COALESCE(turn_id, ''), COALESCE(target, ''), COUNT(*)
             FROM durable_tool_events
             WHERE kind = 'mutation'
             GROUP BY session_key, timestamp_ms / 3600000, COALESCE(model, ''), COALESCE(turn_id, ''), COALESCE(target, '');",
        )?;
        transaction.execute_batch(
            "INSERT INTO history_meta(key, value) VALUES('schema_version', '4')
               ON CONFLICT(key) DO UPDATE SET value = excluded.value;
             PRAGMA user_version = 4;",
        )?;
        transaction.commit()?;
    }
    let version_before_cache_creation: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version_before_cache_creation == 4 {
        // Cache-creation ("cache write") tokens are a normalized price
        // dimension distinct from cache reads (issue #42), layered on top of
        // the #107 rollup schema. Each `ALTER TABLE` is guarded by a direct
        // `pragma_table_info` check rather than inferred from *how* this
        // migrate() call reached version 4: `rollup_token_totals` already
        // carries the column when the v3->v4 step above just created it
        // fresh in this same call (`ROLLUP_SCHEMA_SQL` declares it), but a
        // real pre-#42 database that was already sitting at version 4 does
        // not have it on either table, and re-running `ALTER TABLE ADD
        // COLUMN` on a column that already exists is a hard SQLite error —
        // an inferred-from-path guard would be correct for every real
        // upgrade sequence but wrong the moment a database's tables don't
        // precisely match what that inference assumes.
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !table_has_column(
            &transaction,
            "durable_token_events",
            "cache_creation_input_tokens",
        )? {
            transaction.execute_batch(
                "ALTER TABLE durable_token_events
                   ADD COLUMN cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        if !table_has_column(
            &transaction,
            "rollup_token_totals",
            "cache_creation_input_tokens",
        )? {
            transaction.execute_batch(
                "ALTER TABLE rollup_token_totals
                   ADD COLUMN cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        // Backfill via the same SQL `GROUP BY` shape the v3->v4 rollup
        // migration above uses: one aggregate pass over
        // `durable_token_events`, never by re-parsing transcripts or
        // materializing a `Session` in Rust (the ledger can hold millions of
        // token events). This always runs, unconditionally: it is a no-op
        // (every source row is 0) for genuinely historical data that never
        // recorded a cache-write count separately from ordinary input, but
        // it is exactly what reconciles `rollup_token_totals` when
        // `durable_token_events` already held real cache-creation data that
        // an earlier, narrower backfill pass could not have copied forward
        // (that column did not exist on either table at that point in the
        // migration sequence) — a `rollup_token_totals` row must not
        // silently keep serving a stale 0 in that case.
        transaction.execute_batch(
            "UPDATE rollup_token_totals
             SET cache_creation_input_tokens = agg.total
             FROM (
               SELECT session_key, timestamp_ms / 3600000 AS hour_bucket,
                      COALESCE(model, '') AS model, COALESCE(service_tier, '') AS service_tier,
                      SUM(cache_creation_input_tokens) AS total
               FROM durable_token_events
               GROUP BY session_key, timestamp_ms / 3600000, COALESCE(model, ''), COALESCE(service_tier, '')
             ) AS agg
             WHERE rollup_token_totals.session_key = agg.session_key
               AND rollup_token_totals.hour_bucket = agg.hour_bucket
               AND rollup_token_totals.model = agg.model
               AND rollup_token_totals.service_tier = agg.service_tier;",
        )?;
        transaction.execute_batch(
            "INSERT INTO history_meta(key, value) VALUES('schema_version', '5')
               ON CONFLICT(key) DO UPDATE SET value = excluded.value;
             PRAGMA user_version = 5;",
        )?;
        transaction.commit()?;
    }
    Ok(())
}

/// Shared by both the fresh-database path (`version == 0`, already at the
/// latest schema) and the `version == 3` upgrade path, so the table shapes
/// can never drift between them.
const ROLLUP_SCHEMA_SQL: &str = "
    CREATE TABLE IF NOT EXISTS rollup_token_totals (
      session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
      hour_bucket INTEGER NOT NULL,
      model TEXT NOT NULL DEFAULT '',
      service_tier TEXT NOT NULL DEFAULT '',
      input_tokens INTEGER NOT NULL DEFAULT 0,
      cached_input_tokens INTEGER NOT NULL DEFAULT 0,
      cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0,
      output_tokens INTEGER NOT NULL DEFAULT 0,
      reasoning_output_tokens INTEGER NOT NULL DEFAULT 0,
      total_tokens INTEGER NOT NULL DEFAULT 0
    );
    CREATE UNIQUE INDEX IF NOT EXISTS rollup_token_totals_key_idx
      ON rollup_token_totals(session_key, hour_bucket, model, service_tier);
    CREATE TABLE IF NOT EXISTS rollup_tool_metrics (
      session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
      hour_bucket INTEGER NOT NULL,
      model TEXT NOT NULL DEFAULT '',
      calls INTEGER NOT NULL DEFAULT 0,
      reads INTEGER NOT NULL DEFAULT 0,
      searches INTEGER NOT NULL DEFAULT 0,
      mutations INTEGER NOT NULL DEFAULT 0,
      commands INTEGER NOT NULL DEFAULT 0,
      other INTEGER NOT NULL DEFAULT 0,
      successes INTEGER NOT NULL DEFAULT 0,
      failures INTEGER NOT NULL DEFAULT 0,
      unknown INTEGER NOT NULL DEFAULT 0,
      duration_ms INTEGER NOT NULL DEFAULT 0,
      output_bytes INTEGER NOT NULL DEFAULT 0
    );
    CREATE UNIQUE INDEX IF NOT EXISTS rollup_tool_metrics_key_idx
      ON rollup_tool_metrics(session_key, hour_bucket, model);
    CREATE TABLE IF NOT EXISTS rollup_mutation_chains (
      session_key TEXT NOT NULL REFERENCES durable_sessions(session_key),
      hour_bucket INTEGER NOT NULL,
      model TEXT NOT NULL DEFAULT '',
      turn_id TEXT NOT NULL DEFAULT '',
      target TEXT NOT NULL DEFAULT '',
      mutation_count INTEGER NOT NULL DEFAULT 0
    );
    CREATE UNIQUE INDEX IF NOT EXISTS rollup_mutation_chains_key_idx
      ON rollup_mutation_chains(session_key, hour_bucket, model, turn_id, target);
";

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
           reasoning_output_tokens, total_tokens, cache_creation_input_tokens)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
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
            to_i64(event.delta.cache_creation_input_tokens)?,
        ])?;
    }

    // Maintain the #107 hour-bucket token rollup incrementally: token events
    // are append-only (the same new-suffix slice inserted above), so each
    // event needs to contribute its delta exactly once, additively, to keep
    // read-path reconstruction exact.
    if next_event_index < events.len() {
        let mut upsert = transaction.prepare(
            "INSERT INTO rollup_token_totals(
               session_key, hour_bucket, model, service_tier,
               input_tokens, cached_input_tokens, output_tokens, reasoning_output_tokens, total_tokens,
               cache_creation_input_tokens)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(session_key, hour_bucket, model, service_tier) DO UPDATE SET
               input_tokens = input_tokens + excluded.input_tokens,
               cached_input_tokens = cached_input_tokens + excluded.cached_input_tokens,
               output_tokens = output_tokens + excluded.output_tokens,
               reasoning_output_tokens = reasoning_output_tokens + excluded.reasoning_output_tokens,
               total_tokens = total_tokens + excluded.total_tokens,
               cache_creation_input_tokens = cache_creation_input_tokens + excluded.cache_creation_input_tokens",
        )?;
        for event in events.iter().skip(next_event_index) {
            upsert.execute(params![
                key,
                hour_bucket(event.timestamp.timestamp_millis()),
                key_sentinel(event.model.as_deref()),
                key_sentinel(event.service_tier.as_deref()),
                to_i64(event.delta.input_tokens)?,
                to_i64(event.delta.cached_input_tokens)?,
                to_i64(event.delta.output_tokens)?,
                to_i64(event.delta.reasoning_output_tokens)?,
                to_i64(event.delta.total_tokens)?,
                to_i64(event.delta.cache_creation_input_tokens)?,
            ])?;
        }
    }
    Ok(())
}

fn tool_outcome_str(outcome: ToolOutcome) -> &'static str {
    match outcome {
        ToolOutcome::Pending => "pending",
        ToolOutcome::Success => "success",
        ToolOutcome::Failure => "failure",
        ToolOutcome::Unknown => "unknown",
    }
}

fn tool_outcome_from_str(value: &str) -> ToolOutcome {
    match value {
        "pending" => ToolOutcome::Pending,
        "success" => ToolOutcome::Success,
        "failure" => ToolOutcome::Failure,
        _ => ToolOutcome::Unknown,
    }
}

fn tool_kind_from_str(value: &str) -> ToolKind {
    match value {
        "read" => ToolKind::Read,
        "search" => ToolKind::Search,
        "mutation" => ToolKind::Mutation,
        "command" => ToolKind::Command,
        _ => ToolKind::Other,
    }
}

/// Replaces the `durable_tool_events` fact rows for one session, with no
/// rollup side effects. Used by the normal write path (via
/// [`store_tool_events`], which additionally rebuilds the rollups from the
/// same observations) and — deliberately without rollup maintenance — by the
/// legacy schema-v1/v2 fact backfill, which runs *before* the rollup tables
/// exist; the later v3->v4 migration step populates rollups for every
/// session from whatever `durable_tool_events` holds at that point, however
/// it got there, so backfilling rollups here would double-insert.
fn store_tool_event_facts(
    transaction: &Transaction<'_>,
    key: &str,
    observations: &[ToolObservation],
) -> Result<()> {
    transaction.execute(
        "DELETE FROM durable_tool_events WHERE session_key = ?1",
        [key],
    )?;
    let mut insert = transaction.prepare(
        "INSERT INTO durable_tool_events(session_key, timestamp_ms, model, kind, outcome, turn_id, target, duration_ms, output_bytes)
         VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for item in observations {
        insert.execute(params![
            key,
            item.timestamp.timestamp_millis(),
            item.model,
            item.kind.as_str(),
            tool_outcome_str(item.outcome),
            item.turn_id,
            item.target,
            item.duration_ms.map(|value| value as i64),
            to_i64(item.output_bytes)?,
        ])?;
    }
    Ok(())
}

/// Replaceable per-session materialization of tool observations, carrying
/// exactly the fields window-scoped `ToolMetrics` reconstruction needs, plus
/// the #107 hour-bucket rollups derived from the same observations. Unlike
/// token events these are not append-only: the snapshot-hash gate at the
/// call sites already limits rewrites to sessions that actually changed.
fn store_tool_events(
    transaction: &Transaction<'_>,
    key: &str,
    observations: &[ToolObservation],
) -> Result<()> {
    store_tool_event_facts(transaction, key, observations)?;
    transaction.execute(
        "DELETE FROM rollup_tool_metrics WHERE session_key = ?1",
        [key],
    )?;
    transaction.execute(
        "DELETE FROM rollup_mutation_chains WHERE session_key = ?1",
        [key],
    )?;
    // Rebuilt wholesale from the same observations just written above (this
    // path is a replace-all, not an append), so the rollups can never
    // observe a durable_tool_events row they did not also account for.
    let mut tool_metrics: HashMap<(i64, String), ToolMetrics> = HashMap::new();
    let mut chain_counts: HashMap<(i64, String, String, String), u64> = HashMap::new();
    for item in observations {
        let bucket = hour_bucket(item.timestamp.timestamp_millis());
        let model = key_sentinel(item.model.as_deref()).to_owned();
        crate::telemetry::accumulate_observation(
            tool_metrics.entry((bucket, model.clone())).or_default(),
            item,
        );
        if item.kind == ToolKind::Mutation {
            let turn = key_sentinel(item.turn_id.as_deref()).to_owned();
            let target = key_sentinel(item.target.as_deref()).to_owned();
            *chain_counts
                .entry((bucket, model, turn, target))
                .or_insert(0) += 1;
        }
    }
    if !tool_metrics.is_empty() {
        let mut insert_metrics = transaction.prepare(
            "INSERT INTO rollup_tool_metrics(
               session_key, hour_bucket, model, calls, reads, searches, mutations, commands, other,
               successes, failures, unknown, duration_ms, output_bytes)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        )?;
        for ((bucket, model), metrics) in &tool_metrics {
            insert_metrics.execute(params![
                key,
                bucket,
                model,
                to_i64(metrics.calls)?,
                to_i64(metrics.reads)?,
                to_i64(metrics.searches)?,
                to_i64(metrics.mutations)?,
                to_i64(metrics.commands)?,
                to_i64(metrics.other)?,
                to_i64(metrics.successes)?,
                to_i64(metrics.failures)?,
                to_i64(metrics.unknown)?,
                to_i64(metrics.duration_ms)?,
                to_i64(metrics.output_bytes)?,
            ])?;
        }
    }
    if !chain_counts.is_empty() {
        let mut insert_chains = transaction.prepare(
            "INSERT INTO rollup_mutation_chains(session_key, hour_bucket, model, turn_id, target, mutation_count)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for ((bucket, model, turn, target), count) in &chain_counts {
            insert_chains.execute(params![key, bucket, model, turn, target, to_i64(*count)?])?;
        }
    }
    Ok(())
}

/// Replaceable per-session materialization of optimization findings, carrying
/// the fields window-scoped counts and `OptimizationSummary` need.
fn store_finding_events(
    transaction: &Transaction<'_>,
    key: &str,
    findings: &[OptimizationFinding],
) -> Result<()> {
    transaction.execute(
        "DELETE FROM durable_finding_events WHERE session_key = ?1",
        [key],
    )?;
    let mut insert = transaction.prepare(
        "INSERT INTO durable_finding_events(session_key, timestamp_ms, rule_id, severity, avoidable_calls)
         VALUES(?1, ?2, ?3, ?4, ?5)",
    )?;
    for finding in findings {
        insert.execute(params![
            key,
            finding.timestamp.map(|value| value.timestamp_millis()),
            finding.rule_id,
            finding.severity,
            to_i64(finding.avoidable_calls)?,
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
    session.harness == claude_code_provider_id()
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
        CategoryMetric, OptimizationFinding, RateLimitSnapshotPoint, TokenTotals, ToolMetrics,
        ToolObservation, TurnInfo,
    };
    use crate::provider::codex_provider_id;
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
            cache_creation_input_tokens: input / 16,
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
            storage_id: crate::model::storage_id_for_session(&codex_provider_id(), id),
            harness: codex_provider_id(),
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

    fn tool(
        at: &str,
        kind: crate::model::ToolKind,
        outcome: ToolOutcome,
        model: Option<&str>,
        turn: Option<&str>,
        target: Option<&str>,
    ) -> ToolObservation {
        ToolObservation {
            call_id: format!("call-{at}"),
            turn_id: turn.map(Into::into),
            harness: codex_provider_id(),
            model: model.map(Into::into),
            timestamp: timestamp(at),
            kind,
            name: "tool".into(),
            providers: Vec::new(),
            effective_tools: Vec::new(),
            target: target.map(Into::into),
            resource_id: None,
            outcome,
            duration_ms: Some(40),
            output_bytes: 128,
        }
    }

    /// Adversarial rollup fixture: multiple models and tiers, a model-less
    /// event, mutation-target repeats that straddle window boundaries (so
    /// retry/one-shot counts differ per window), and dated plus undated
    /// findings.
    fn rich_session(id: &str) -> Session {
        use crate::model::ToolKind;
        let mut fixture = session(id, 100);
        let event =
            |at: &str, model: Option<&str>, tier: Option<&str>, input: u64| TokenHistoryPoint {
                timestamp: timestamp(at),
                model: model.map(Into::into),
                service_tier: tier.map(Into::into),
                request_input_tokens: Some(input),
                total_tokens: 0,
                delta: totals(input),
            };
        fixture.tokens_history = vec![
            event("2026-01-01T00:00:01Z", Some("gpt-a"), None, 100),
            event("2026-01-01T06:00:00Z", Some("gpt-a"), Some("fast"), 40),
            // Sub-millisecond precision at an exact window bound: both paths
            // must classify it identically (comparisons are ms-floored).
            event("2026-01-01T12:00:00.000441Z", Some("gpt-b"), None, 70),
            event("2026-01-01T18:00:00Z", None, None, 30),
            event("2026-01-02T00:00:00Z", Some("gpt-b"), Some("fast"), 55),
            event("2026-01-02T09:30:00Z", Some("gpt-a"), None, 20),
        ];
        fixture.tool_observations = vec![
            tool(
                "2026-01-01T01:00:00Z",
                ToolKind::Read,
                ToolOutcome::Success,
                Some("gpt-a"),
                Some("t1"),
                None,
            ),
            tool(
                "2026-01-01T05:00:00Z",
                ToolKind::Mutation,
                ToolOutcome::Success,
                Some("gpt-a"),
                Some("t1"),
                Some("hash-x"),
            ),
            // Same (turn, target) mutated again the next day: a retry when the
            // window spans both days, two one-shots when windows split them.
            tool(
                "2026-01-02T05:00:00Z",
                ToolKind::Mutation,
                ToolOutcome::Failure,
                Some("gpt-b"),
                Some("t1"),
                Some("hash-x"),
            ),
            tool(
                "2026-01-02T06:00:00Z",
                ToolKind::Command,
                ToolOutcome::Unknown,
                None,
                None,
                None,
            ),
            tool(
                "2026-01-02T07:00:00Z",
                ToolKind::Search,
                ToolOutcome::Pending,
                Some("gpt-b"),
                Some("t2"),
                None,
            ),
        ];
        fixture.optimization_findings = vec![
            OptimizationFinding {
                rule_id: "rule-a".into(),
                severity: "warning".into(),
                avoidable_calls: 3,
                timestamp: Some(timestamp("2026-01-01T13:00:00Z")),
                ..OptimizationFinding::default()
            },
            OptimizationFinding {
                rule_id: "rule-b".into(),
                severity: "info".into(),
                avoidable_calls: 1,
                timestamp: Some(timestamp("2026-01-02T08:00:00Z")),
                ..OptimizationFinding::default()
            },
            OptimizationFinding {
                rule_id: "rule-c".into(),
                severity: "warning".into(),
                avoidable_calls: 0,
                timestamp: None,
                ..OptimizationFinding::default()
            },
        ];
        fixture.last_event_at = timestamp("2026-01-02T09:30:00Z");
        fixture
    }

    #[test]
    fn ledger_range_totals_match_in_memory_rollups() {
        let (_directory, store) = store();
        let sessions = [rich_session("golden-a"), rich_session("golden-b")];
        let generation = store.begin_scan().unwrap().max(1);
        let mut keys = Vec::new();
        for (index, fixture) in sessions.iter().enumerate() {
            let stored = store
                .observe(
                    Path::new(&format!("golden-{index}.jsonl")),
                    fixture,
                    generation,
                )
                .unwrap();
            keys.push(stored.key);
        }

        let bound = |value: &str| Some(timestamp(value));
        let windows: Vec<RangeWindow> = vec![
            (None, None),
            // Exactly the first day; the upper bound hits an event timestamp.
            (bound("2026-01-01T00:00:00Z"), bound("2026-01-02T00:00:00Z")),
            // Second day only: splits the repeated mutation target.
            (bound("2026-01-02T00:00:01Z"), None),
            // Open start.
            (None, bound("2026-01-01T11:59:59Z")),
            // Empty window.
            (bound("2027-01-01T00:00:00Z"), bound("2027-06-01T00:00:00Z")),
            // Single instant equal to an event timestamp.
            (bound("2026-01-01T12:00:00Z"), bound("2026-01-01T12:00:00Z")),
        ];

        let from_ledger = store.range_totals_multi(&keys, &windows).unwrap();
        for (window_index, window) in windows.iter().enumerate() {
            for (fixture, key) in sessions.iter().zip(&keys) {
                let expected = fixture.range_totals_multi(std::slice::from_ref(window));
                let expected = &expected[0];
                match from_ledger[window_index].get(key) {
                    Some(actual) => {
                        assert_eq!(
                            &actual.tokens, &expected.tokens,
                            "tokens window {window_index} {key}"
                        );
                        assert_eq!(
                            &actual.buckets, &expected.buckets,
                            "buckets window {window_index} {key}"
                        );
                        assert_eq!(
                            &actual.tool_metrics, &expected.tool_metrics,
                            "tool_metrics window {window_index} {key}"
                        );
                        assert_eq!(
                            &actual.tool_metrics_by_model, &expected.tool_metrics_by_model,
                            "tool_metrics_by_model window {window_index} {key}"
                        );
                        assert_eq!(
                            actual.optimization_findings_count,
                            expected.optimization_findings_count,
                            "findings count window {window_index} {key}"
                        );
                        assert_eq!(
                            &actual.optimization_summary, &expected.optimization_summary,
                            "findings summary window {window_index} {key}"
                        );
                    }
                    None => {
                        assert!(
                            !crate::commands::range_has_data(expected),
                            "ledger omitted a window with data: window {window_index} {key}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn ledger_range_totals_include_cache_creation_tokens_across_rollup_and_edge_reads() {
        // Regression guard for the AGENTS.md ledger-rollup-dimension
        // invariant (#112): `rollup_token_totals` must carry
        // `cache_creation_input_tokens`, or a window that spans a whole-hour
        // rollup bucket plus sub-hour edges silently drops the
        // cache-creation dimension for the rollup-served bucket while still
        // reporting it correctly for the edge-served buckets — a partial,
        // silent under-count rather than an obviously wrong total.
        let (_directory, store) = store();
        let mut fixture = session("cache-creation-golden", 0);
        let event = |at: &str, cache_creation: u64| TokenHistoryPoint {
            timestamp: timestamp(at),
            model: Some("gpt-test".into()),
            service_tier: None,
            request_input_tokens: Some(100),
            total_tokens: 0,
            delta: TokenTotals {
                input_tokens: 100,
                cached_input_tokens: 0,
                cache_creation_input_tokens: cache_creation,
                output_tokens: 20,
                reasoning_output_tokens: 0,
                total_tokens: 120,
            },
        };
        // Bucket 0 (00:00-01:00) and bucket 2 (02:00-03:00) are edge-served
        // by the window below; bucket 1 (01:00-02:00) is fully inside it and
        // is rollup-served. Non-zero cache-creation tokens in at least two
        // distinct hour buckets, including the rollup-served one, are
        // exactly what a fact-table-only fix cannot get right.
        fixture.tokens_history = vec![
            event("2026-01-01T00:30:00Z", 40),
            event("2026-01-01T01:15:00Z", 25),
            event("2026-01-01T02:45:00Z", 10),
        ];
        let mut total = TokenTotals::default();
        for point in &fixture.tokens_history {
            total += &point.delta;
        }
        fixture.tokens_total = total;
        fixture.last_event_at = timestamp("2026-01-01T02:45:00Z");

        let generation = store.begin_scan().unwrap().max(1);
        let key = store
            .observe(
                Path::new("cache-creation-golden.jsonl"),
                &fixture,
                generation,
            )
            .unwrap()
            .key;

        // Both bounds land inside a bucket (not on an hour boundary), so
        // this single call exercises a whole-bucket rollup read (bucket 1)
        // together with two partial-edge event reads (the tails of buckets
        // 0 and 2) in one query — the exact combination the AGENTS.md
        // invariant calls out.
        let windows: Vec<RangeWindow> = vec![(
            Some(timestamp("2026-01-01T00:15:00Z")),
            Some(timestamp("2026-01-01T02:50:00Z")),
        )];
        let from_ledger = store
            .range_totals_multi(std::slice::from_ref(&key), &windows)
            .unwrap();
        let expected = fixture.range_totals_multi(&windows);

        assert_eq!(
            expected[0].tokens.cache_creation_input_tokens, 75,
            "fixture sanity check: 40 + 25 + 10"
        );
        let actual = from_ledger[0]
            .get(&key)
            .expect("window has data for this session");
        assert_eq!(
            actual.tokens.cache_creation_input_tokens,
            expected[0].tokens.cache_creation_input_tokens,
            "ledger cache-creation tokens must match the in-memory oracle across rollup and edge reads"
        );
        assert_eq!(&actual.tokens, &expected[0].tokens);
    }

    #[test]
    fn appended_history_keeps_ledger_and_oracle_equal() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap().max(1);
        let mut fixture = rich_session("append");
        let key = store
            .observe(Path::new("append.jsonl"), &fixture, generation)
            .unwrap()
            .key;
        // Append: more events, another straddling mutation, one more finding.
        fixture.tokens_history.push(TokenHistoryPoint {
            timestamp: timestamp("2026-01-03T04:00:00Z"),
            model: Some("gpt-a".into()),
            service_tier: Some("fast".into()),
            request_input_tokens: Some(15),
            total_tokens: 0,
            delta: totals(15),
        });
        fixture.tool_observations.push(tool(
            "2026-01-03T05:00:00Z",
            crate::model::ToolKind::Mutation,
            ToolOutcome::Success,
            Some("gpt-a"),
            Some("t1"),
            Some("hash-x"),
        ));
        fixture.optimization_findings.push(OptimizationFinding {
            rule_id: "rule-a".into(),
            severity: "warning".into(),
            avoidable_calls: 2,
            timestamp: Some(timestamp("2026-01-03T06:00:00Z")),
            ..OptimizationFinding::default()
        });
        fixture.last_event_at = timestamp("2026-01-03T06:00:00Z");
        store
            .observe(Path::new("append.jsonl"), &fixture, generation)
            .unwrap();

        let bound = |value: &str| Some(timestamp(value));
        let windows: Vec<RangeWindow> = vec![
            (None, None),
            (bound("2026-01-03T00:00:00Z"), None),
            (bound("2026-01-01T00:00:00Z"), bound("2026-01-02T12:00:00Z")),
        ];
        let from_ledger = store
            .range_totals_multi(std::slice::from_ref(&key), &windows)
            .unwrap();
        let expected = fixture.range_totals_multi(&windows);
        for (window_index, expected_range) in expected.iter().enumerate() {
            let actual = from_ledger[window_index]
                .get(&key)
                .expect("window has data after append");
            assert_eq!(
                &actual.tokens, &expected_range.tokens,
                "window {window_index}"
            );
            assert_eq!(
                &actual.buckets, &expected_range.buckets,
                "window {window_index}"
            );
            assert_eq!(
                &actual.tool_metrics, &expected_range.tool_metrics,
                "window {window_index}"
            );
            assert_eq!(
                actual.optimization_findings_count, expected_range.optimization_findings_count,
                "window {window_index}"
            );
        }
    }

    #[test]
    fn metadata_overlay_marks_diverged_history_dirty_and_observe_clears_it() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap().max(1);
        let mut fixture = rich_session("overlay");
        let key = store
            .observe(Path::new("overlay.jsonl"), &fixture, generation)
            .unwrap()
            .key;
        // Same history, new display name: overlay stays clean.
        fixture.thread_name = Some("Renamed".into());
        store.update_snapshot(&fixture).unwrap();
        assert!(store.dirty_session_keys().unwrap().is_empty());

        // History advanced without an observe (the failed-persist scenario):
        // the overlay must durably mark the ledger facts untrustworthy.
        fixture.tokens_history.push(TokenHistoryPoint {
            timestamp: timestamp("2026-01-04T00:00:00Z"),
            model: Some("gpt-a".into()),
            service_tier: None,
            request_input_tokens: Some(5),
            total_tokens: 0,
            delta: totals(5),
        });
        store.update_snapshot(&fixture).unwrap();
        assert_eq!(store.dirty_session_keys().unwrap(), vec![key.clone()]);

        // A successful observe realigns facts and clears the marking.
        store
            .observe(Path::new("overlay.jsonl"), &fixture, generation)
            .unwrap();
        assert!(store.dirty_session_keys().unwrap().is_empty());
        let windows: Vec<RangeWindow> = vec![(None, None)];
        let from_ledger = store
            .range_totals_multi(std::slice::from_ref(&key), &windows)
            .unwrap();
        let expected = &fixture.range_totals_multi(&windows)[0];
        assert_eq!(&from_ledger[0][&key].tokens, &expected.tokens);
    }

    #[test]
    fn v3_migration_backfills_facts_from_snapshots() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        {
            let store = HistoryStore::open(&path).unwrap();
            let generation = store.begin_scan().unwrap().max(1);
            store
                .observe(
                    Path::new("backfill.jsonl"),
                    &rich_session("backfill"),
                    generation,
                )
                .unwrap();
        }
        // Rewind to schema v2: drop the fact and rollup tables, the ledger
        // flag, and the cache-creation column, and the version marker —
        // exactly what a store written by a release before #107 and #42
        // looks like (a genuine v2 database predates all of them).
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "DROP TABLE durable_tool_events;
                     DROP TABLE durable_finding_events;
                     DROP TABLE rollup_token_totals;
                     DROP TABLE rollup_tool_metrics;
                     DROP TABLE rollup_mutation_chains;
                     ALTER TABLE durable_sessions DROP COLUMN ledger_dirty;
                     ALTER TABLE durable_token_events DROP COLUMN cache_creation_input_tokens;
                     INSERT INTO history_meta(key, value) VALUES('schema_version', '2')
                       ON CONFLICT(key) DO UPDATE SET value = '2';
                     PRAGMA user_version = 2;",
                )
                .unwrap();
        }
        let store = HistoryStore::open(&path).unwrap();
        let fixture = rich_session("backfill");
        let key = crate::model::storage_id_for_session(&codex_provider_id(), "backfill");
        let windows: Vec<RangeWindow> = vec![(None, None)];
        let from_ledger = store
            .range_totals_multi(std::slice::from_ref(&key), &windows)
            .unwrap();
        let expected = &fixture.range_totals_multi(&windows)[0];
        let actual = from_ledger[0].get(&key).expect("backfilled facts present");
        assert_eq!(&actual.tool_metrics, &expected.tool_metrics);
        assert_eq!(
            actual.optimization_findings_count,
            expected.optimization_findings_count
        );
        assert_eq!(&actual.optimization_summary, &expected.optimization_summary);
    }

    /// Builds a session whose only activity is the same `(turn, target)`
    /// mutation chain firing twice, at `first_at` and `second_at` — the
    /// minimal fixture for proving the rollup read path never double-counts
    /// or under-counts a chain that straddles a bucket boundary.
    fn mutation_pair_session(id: &str, first_at: &str, second_at: &str) -> Session {
        use crate::model::ToolKind;
        let mut fixture = session(id, 10);
        fixture.tool_observations = vec![
            tool(
                first_at,
                ToolKind::Mutation,
                ToolOutcome::Success,
                Some("gpt-a"),
                Some("t1"),
                Some("hash-x"),
            ),
            tool(
                second_at,
                ToolKind::Mutation,
                ToolOutcome::Success,
                Some("gpt-a"),
                Some("t1"),
                Some("hash-x"),
            ),
        ];
        fixture.last_event_at = timestamp(second_at);
        fixture
    }

    /// Asserts the ledger matches the in-memory oracle for every window,
    /// including windows with no data in range (the ledger must omit them).
    fn assert_ledger_matches_oracle_tool_metrics(
        store: &HistoryStore,
        key: &str,
        fixture: &Session,
        windows: &[RangeWindow],
    ) {
        let from_ledger = store
            .range_totals_multi(std::slice::from_ref(&key.to_owned()), windows)
            .unwrap();
        let expected = fixture.range_totals_multi(windows);
        for (window_index, expected_range) in expected.iter().enumerate() {
            let actual = from_ledger[window_index].get(key);
            if crate::commands::range_has_data(expected_range) {
                let actual = actual
                    .unwrap_or_else(|| panic!("ledger omitted data for window {window_index}"));
                assert_eq!(
                    &actual.tool_metrics, &expected_range.tool_metrics,
                    "tool_metrics window {window_index}"
                );
            } else {
                assert!(
                    actual.is_none(),
                    "ledger had extra data for window {window_index}"
                );
            }
        }
    }

    #[test]
    fn mutation_chain_straddling_an_hour_bucket_matches_oracle() {
        let (_directory, store) = store();
        // The chain's two mutations land in adjacent hour buckets (00 and
        // 01), thirty seconds either side of the boundary.
        let fixture = mutation_pair_session(
            "hour-straddle",
            "2026-01-01T00:59:30Z",
            "2026-01-01T01:00:30Z",
        );
        let generation = store.begin_scan().unwrap().max(1);
        let key = store
            .observe(Path::new("hour-straddle.jsonl"), &fixture, generation)
            .unwrap()
            .key;

        let bound = |value: &str| Some(timestamp(value));
        let windows: Vec<RangeWindow> = vec![
            // Whole range: one chain, two events -> a retry, not two one-shots.
            (None, None),
            // Both hour buckets, hour-aligned: same expectation via rollups.
            (bound("2026-01-01T00:00:00Z"), bound("2026-01-01T02:00:00Z")),
            // Only the first bucket: a single one-shot mutation.
            (
                bound("2026-01-01T00:00:00Z"),
                bound("2026-01-01T00:59:59.999Z"),
            ),
            // Only the second bucket: a single one-shot mutation.
            (
                bound("2026-01-01T01:00:00Z"),
                bound("2026-01-01T01:59:59.999Z"),
            ),
            // Sub-hour window straddling the exact boundary (no full bucket
            // at all — served entirely from an exact event read).
            (bound("2026-01-01T00:59:00Z"), bound("2026-01-01T01:01:00Z")),
        ];
        assert_ledger_matches_oracle_tool_metrics(&store, &key, &fixture, &windows);
    }

    #[test]
    fn mutation_chain_straddling_a_day_boundary_matches_oracle() {
        let (_directory, store) = store();
        // The chain's two mutations land on either side of midnight.
        let fixture = mutation_pair_session(
            "day-straddle",
            "2026-01-01T23:59:30Z",
            "2026-01-02T00:00:30Z",
        );
        let generation = store.begin_scan().unwrap().max(1);
        let key = store
            .observe(Path::new("day-straddle.jsonl"), &fixture, generation)
            .unwrap()
            .key;

        let bound = |value: &str| Some(timestamp(value));
        let windows: Vec<RangeWindow> = vec![
            (None, None),
            // Both days, day-aligned (also hour-aligned): one retried chain.
            (bound("2026-01-01T00:00:00Z"), bound("2026-01-03T00:00:00Z")),
            // Day one only: a single one-shot mutation.
            (
                bound("2026-01-01T00:00:00Z"),
                bound("2026-01-01T23:59:59.999Z"),
            ),
            // Day two only: a single one-shot mutation.
            (bound("2026-01-02T00:00:00Z"), None),
        ];
        assert_ledger_matches_oracle_tool_metrics(&store, &key, &fixture, &windows);
    }

    #[test]
    fn v4_migration_rebuilds_rollups_from_existing_facts_without_reparsing() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let fixture = rich_session("rollup-rebuild");
        let key;
        {
            let store = HistoryStore::open(&path).unwrap();
            let generation = store.begin_scan().unwrap().max(1);
            key = store
                .observe(Path::new("rollup-rebuild.jsonl"), &fixture, generation)
                .unwrap()
                .key;
        }
        // Rewind to schema v3: drop only the rollup tables, keeping the
        // normalized fact tables the migration must rebuild them from —
        // exactly what an existing v0.8.4 store looks like.
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "DROP TABLE rollup_token_totals;
                     DROP TABLE rollup_tool_metrics;
                     DROP TABLE rollup_mutation_chains;
                     INSERT INTO history_meta(key, value) VALUES('schema_version', '3')
                       ON CONFLICT(key) DO UPDATE SET value = '3';
                     PRAGMA user_version = 3;",
                )
                .unwrap();
        }
        let store = HistoryStore::open(&path).unwrap();
        let connection = store.connection().unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        let rollup_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM rollup_token_totals", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(rollup_rows > 0, "migration should have populated rollups");
        drop(connection);

        let bound = |value: &str| Some(timestamp(value));
        let windows: Vec<RangeWindow> = vec![
            (None, None),
            (bound("2026-01-01T00:00:00Z"), bound("2026-01-02T00:00:00Z")),
            (bound("2026-01-02T00:00:01Z"), None),
        ];
        let from_ledger = store
            .range_totals_multi(std::slice::from_ref(&key), &windows)
            .unwrap();
        let expected = fixture.range_totals_multi(&windows);
        for (window_index, expected_range) in expected.iter().enumerate() {
            let actual = from_ledger[window_index].get(&key);
            if crate::commands::range_has_data(expected_range) {
                let actual = actual.expect("rebuilt rollups should cover this window");
                assert_eq!(
                    &actual.tokens, &expected_range.tokens,
                    "window {window_index}"
                );
                assert_eq!(
                    &actual.buckets, &expected_range.buckets,
                    "window {window_index}"
                );
                assert_eq!(
                    &actual.tool_metrics, &expected_range.tool_metrics,
                    "window {window_index}"
                );
            } else {
                assert!(actual.is_none(), "window {window_index}");
            }
        }
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
                 -- Present in every real v1 store; the v3 fact backfill reads
                 -- current snapshots through these tables.
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
                 CREATE TABLE session_snapshots (
                   session_key TEXT NOT NULL,
                   version INTEGER NOT NULL,
                   format_version INTEGER NOT NULL,
                   snapshot_hash TEXT NOT NULL,
                   captured_at_ms INTEGER NOT NULL,
                   session_json BLOB NOT NULL,
                   PRIMARY KEY(session_key, version)
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
        let cache_creation_column_count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('durable_token_events') WHERE name = 'cache_creation_input_tokens'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cache_creation_column_count, 1);
    }

    #[test]
    fn cache_creation_tokens_round_trip_through_durable_store() {
        // New rows written to a normal (not migrated-from-legacy) store must
        // actually round-trip the cache-creation dimension end to end, not
        // just carry the column.
        let (_directory, store) = store();
        let fixture = rich_session("cache-creation-roundtrip");
        let expected_cache_creation: u64 = fixture
            .tokens_history
            .iter()
            .map(|event| event.delta.cache_creation_input_tokens)
            .sum();
        assert!(
            expected_cache_creation > 0,
            "fixture must exercise the cache-creation dimension"
        );
        let generation = store.begin_scan().unwrap().max(1);
        let observed = store
            .observe(
                Path::new("cache-creation-roundtrip.jsonl"),
                &fixture,
                generation,
            )
            .unwrap();
        let windows: Vec<RangeWindow> = vec![(None, None)];
        let totals = store
            .range_totals_multi(std::slice::from_ref(&observed.key), &windows)
            .unwrap();
        assert_eq!(
            totals[0][&observed.key].tokens.cache_creation_input_tokens,
            expected_cache_creation
        );
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
        session.tokens_total += &delta;
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
        original.harness = claude_code_provider_id();
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
        first.harness = claude_code_provider_id();
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
