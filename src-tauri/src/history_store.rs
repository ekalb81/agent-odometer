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
use std::time::{Duration, Instant};

const SCHEMA_VERSION: i64 = 6;
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

/// A user-controlled project-identity overlay row (#41): an optional local
/// display-label alias, and/or a merge redirect folding this project's
/// sessions under another project. Both are reversible by deleting or
/// clearing this row; neither ever touches the auto-computed
/// `durable_sessions.project_*` columns or a source transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectOverrideRow {
    pub project_key: String,
    pub display_label: Option<String>,
    pub canonical_project_key: Option<String>,
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
        Self::open_default_with_progress(|_| {})
    }

    /// Like [`Self::open_default`], but reports migration progress through
    /// `on_progress` (#116). Opening the archive (including a chained
    /// migration over an existing install) can take seconds on a large
    /// corpus; the caller uses this to drive UI feedback and performance
    /// instrumentation while the archive is not yet ready, rather than
    /// blocking silently.
    pub fn open_default_with_progress(on_progress: impl FnMut(MigrationStepEvent)) -> Result<Self> {
        let path = Self::default_path()?;
        Self::open_with_progress(&path, on_progress)
    }

    /// Opens or creates the archive and applies durable, forward-only schema
    /// migrations. Nothing here is keyed to `CARGO_PKG_VERSION`.
    pub fn open(path: &Path) -> Result<Self> {
        Self::open_with_progress(path, |_| {})
    }

    /// Like [`Self::open`], but reports migration progress through
    /// `on_progress` (#116). See [`Self::open_default_with_progress`].
    pub fn open_with_progress(
        path: &Path,
        mut on_progress: impl FnMut(MigrationStepEvent),
    ) -> Result<Self> {
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
        migrate(&mut connection, &mut on_progress)?;
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
        apply_project_identity(&transaction, &key, &mut archived_session)?;
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
        apply_project_identity(&transaction, &key, &mut archived)?;
        let raw_snapshot = serde_json::to_vec(&archived)
            .context("could not encode metadata-only session snapshot")?;
        let snapshot_hash = stable_hash_bytes(&raw_snapshot);
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

    /// Every project-identity override row (local aliases and/or merges,
    /// #41). Both are reversible: deleting or clearing a row never touches
    /// the auto-computed `durable_sessions.project_*` columns or a source
    /// transcript.
    pub fn list_project_overrides(&self) -> Result<Vec<ProjectOverrideRow>> {
        let connection = self.connection()?;
        load_project_overrides(&connection).map(|map| map.into_values().collect())
    }

    /// Sets (`Some`) or clears (`None`) a local display-label alias for
    /// `project_key`. Never rewrites the auto-computed `project_label`.
    pub fn set_project_alias(&self, project_key: &str, display_label: Option<&str>) -> Result<()> {
        let connection = self.connection()?;
        let now = now_ms();
        connection.execute(
            "INSERT INTO project_overrides(project_key, display_label, canonical_project_key, updated_at_ms)
             VALUES(?1, ?2, NULL, ?3)
             ON CONFLICT(project_key) DO UPDATE SET display_label = ?2, updated_at_ms = ?3",
            params![project_key, display_label, now],
        )?;
        prune_empty_project_override(&connection, project_key)?;
        Ok(())
    }

    /// Merges `source_key` to display under `canonical_key`: every session
    /// auto-computed under `source_key` reports as `canonical_key` once
    /// resolved via [`resolve_canonical_project_key`]. Rejects merging a key
    /// into itself and rejects creating a cycle.
    pub fn merge_project(&self, source_key: &str, canonical_key: &str) -> Result<()> {
        if source_key == canonical_key {
            bail!("cannot merge a project into itself");
        }
        let connection = self.connection()?;
        let overrides = load_project_overrides(&connection)?;
        // A merge of source -> canonical creates a cycle exactly when
        // canonical can already (transitively, through existing overrides)
        // reach source: the resulting chain would be
        // `source -> canonical -> ... -> source`.
        if project_key_reaches(&overrides, canonical_key, source_key) {
            bail!("merging {source_key} into {canonical_key} would create a cycle");
        }
        let now = now_ms();
        connection.execute(
            "INSERT INTO project_overrides(project_key, display_label, canonical_project_key, updated_at_ms)
             VALUES(?1, NULL, ?2, ?3)
             ON CONFLICT(project_key) DO UPDATE SET canonical_project_key = ?2, updated_at_ms = ?3",
            params![source_key, canonical_key, now],
        )?;
        Ok(())
    }

    /// Removes a merge redirect for `project_key` (an existing alias is
    /// preserved). The reversal for `merge_project`.
    pub fn unmerge_project(&self, project_key: &str) -> Result<()> {
        let connection = self.connection()?;
        let now = now_ms();
        connection.execute(
            "INSERT INTO project_overrides(project_key, display_label, canonical_project_key, updated_at_ms)
             VALUES(?1, NULL, NULL, ?2)
             ON CONFLICT(project_key) DO UPDATE SET canonical_project_key = NULL, updated_at_ms = ?2",
            params![project_key, now],
        )?;
        prune_empty_project_override(&connection, project_key)?;
        Ok(())
    }

    /// Manually reassigns one session to `project_key` (or, when `None`, to
    /// a freshly minted standalone project key), overriding whatever it
    /// would otherwise be auto-grouped or merged into. This is the "split"
    /// primitive: it pulls exactly one session out on its own. Reversible
    /// via [`Self::clear_session_project_override`]. Never rewrites the
    /// auto-computed `durable_sessions.project_*` columns or a source
    /// transcript.
    pub fn reassign_session_project(
        &self,
        session_key: &str,
        project_key: Option<&str>,
    ) -> Result<String> {
        let connection = self.connection()?;
        let exists: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM durable_sessions WHERE session_key = ?1",
                [session_key],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            bail!("cannot reassign unknown durable session {session_key}");
        }
        let now = now_ms();
        let effective_key = match project_key.map(str::trim).filter(|key| !key.is_empty()) {
            Some(key) => key.to_string(),
            None => format!(
                "manual:{}",
                stable_hash(&format!("{session_key}\u{1f}{now}"))
            ),
        };
        connection.execute(
            "INSERT INTO project_session_overrides(session_key, project_key, updated_at_ms)
             VALUES(?1, ?2, ?3)
             ON CONFLICT(session_key) DO UPDATE SET project_key = excluded.project_key, updated_at_ms = excluded.updated_at_ms",
            params![session_key, effective_key, now],
        )?;
        Ok(effective_key)
    }

    /// Reverts a manual session reassignment back to auto-computed grouping.
    pub fn clear_session_project_override(&self, session_key: &str) -> Result<()> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM project_session_overrides WHERE session_key = ?1",
            [session_key],
        )?;
        Ok(())
    }

    /// Every session-level manual project reassignment (#41 "split"), keyed
    /// by durable session key.
    pub fn list_session_project_overrides(&self) -> Result<HashMap<String, String>> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT session_key, project_key FROM project_session_overrides")?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows.into_iter().collect())
    }

    /// Re-resolves project identity for every durable session from its
    /// current snapshot, clearing any stale value from before a working
    /// directory changed on disk in a way the cheap unchanged-directory skip
    /// in `apply_project_identity` could not observe (for example, a
    /// directory that became a Git repository after its sessions were first
    /// recorded). Mirrors `resolve_working_directories`' explicit
    /// `refresh()` — this app never re-probes the filesystem automatically
    /// on every observation, only on request. Returns the number of durable
    /// sessions re-resolved.
    pub fn refresh_project_identities(&self) -> Result<usize> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let count = count(&transaction, "SELECT COUNT(*) FROM durable_sessions")?;
        backfill_project_identities(&transaction, None)?;
        transaction.commit()?;
        Ok(count)
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

/// One step of the migration chain reported through `open_with_progress`'s
/// callback (#116), so a caller can drive a "preparing history" UI and
/// per-step performance instrumentation the same way `scanner::scan_all`
/// drives `scan-progress` — without `HistoryStore` knowing whether a
/// listener, a performance recorder, both, or neither is attached.
///
/// A step reports itself twice: once with `elapsed_ms: None` right before it
/// starts running (so a UI can show "step N of M" immediately), and once with
/// `elapsed_ms: Some(..)` right after its transaction commits. A step that
/// streams per-row work (currently only the v5->v6 project-identity
/// backfill) additionally reports `items_done`/`items_total` between those
/// two; every other step commits as one SQL statement and cannot report a
/// fraction without a second full pass, so those fields stay `None`.
#[derive(Debug, Clone)]
pub struct MigrationStepEvent {
    pub step: &'static str,
    pub step_index: u32,
    pub step_total: u32,
    pub from_version: i64,
    pub to_version: i64,
    pub elapsed_ms: Option<u64>,
    pub items_done: Option<usize>,
    pub items_total: Option<usize>,
}

impl MigrationStepEvent {
    fn started(step: &'static str, step_index: u32, step_total: u32, from: i64, to: i64) -> Self {
        Self {
            step,
            step_index,
            step_total,
            from_version: from,
            to_version: to,
            elapsed_ms: None,
            items_done: None,
            items_total: None,
        }
    }

    fn finished(
        step: &'static str,
        step_index: u32,
        step_total: u32,
        from: i64,
        to: i64,
        elapsed: Duration,
    ) -> Self {
        Self {
            step,
            step_index,
            step_total,
            from_version: from,
            to_version: to,
            elapsed_ms: Some(elapsed.as_millis() as u64),
            items_done: None,
            items_total: None,
        }
    }

    fn item_progress(
        step: &'static str,
        step_index: u32,
        step_total: u32,
        from: i64,
        to: i64,
        done: usize,
        total: usize,
    ) -> Self {
        Self {
            step,
            step_index,
            step_total,
            from_version: from,
            to_version: to,
            elapsed_ms: None,
            items_done: Some(done),
            items_total: Some(total),
        }
    }
}

/// The number of steps `migrate()` will actually run starting from
/// `from_version`, purely so a progress callback can report "step N of M"
/// before the first step starts. Mirrors `migrate()`'s own `if version == N`
/// / `if (A..=B).contains(&version)` gates in the same order; kept beside it
/// and covered by `migration_step_count_matches_steps_migrate_actually_runs`
/// so the two cannot silently drift apart.
fn migration_step_count(from_version: i64) -> u32 {
    if from_version == 0 {
        return 1;
    }
    let mut version = from_version;
    let mut steps = 0u32;
    if version == 1 {
        steps += 1;
        version = 2;
    }
    if (1..=2).contains(&version) {
        steps += 1;
        version = 3;
    }
    if version == 3 {
        steps += 1;
        version = 4;
    }
    if version == 4 {
        steps += 1;
        version = 5;
    }
    if version == 5 {
        steps += 1;
    }
    steps
}

fn migrate(
    connection: &mut Connection,
    on_progress: &mut dyn FnMut(MigrationStepEvent),
) -> Result<()> {
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > SCHEMA_VERSION {
        bail!("history store schema {version} is newer than this application supports");
    }
    let step_total = migration_step_count(version);
    let mut step_index = 0u32;
    if version == 0 {
        step_index += 1;
        on_progress(MigrationStepEvent::started(
            "fresh_install",
            step_index,
            step_total,
            0,
            SCHEMA_VERSION,
        ));
        let started = Instant::now();
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
               ledger_dirty INTEGER NOT NULL DEFAULT 0,
               project_key TEXT,
               project_label TEXT,
               project_provenance TEXT,
               project_source_directory TEXT
             );
             CREATE INDEX durable_sessions_identity_idx ON durable_sessions(identity_key);
             CREATE INDEX durable_sessions_project_idx ON durable_sessions(project_key);
             CREATE TABLE project_overrides (
               project_key TEXT PRIMARY KEY,
               display_label TEXT,
               canonical_project_key TEXT,
               updated_at_ms INTEGER NOT NULL
             );
             CREATE TABLE project_session_overrides (
               session_key TEXT PRIMARY KEY REFERENCES durable_sessions(session_key),
               project_key TEXT NOT NULL,
               updated_at_ms INTEGER NOT NULL
             );
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
        on_progress(MigrationStepEvent::finished(
            "fresh_install",
            step_index,
            step_total,
            0,
            SCHEMA_VERSION,
            started.elapsed(),
        ));
    }
    if version == 1 {
        step_index += 1;
        on_progress(MigrationStepEvent::started(
            "v1_to_v2_request_input_tokens",
            step_index,
            step_total,
            1,
            2,
        ));
        let started = Instant::now();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(
            "ALTER TABLE durable_token_events ADD COLUMN request_input_tokens INTEGER;
             INSERT INTO history_meta(key, value) VALUES('schema_version', '2')
               ON CONFLICT(key) DO UPDATE SET value = excluded.value;
             PRAGMA user_version = 2;",
        )?;
        transaction.commit()?;
        on_progress(MigrationStepEvent::finished(
            "v1_to_v2_request_input_tokens",
            step_index,
            step_total,
            1,
            2,
            started.elapsed(),
        ));
    }
    if (1..=2).contains(&version) {
        step_index += 1;
        on_progress(MigrationStepEvent::started(
            "v1v2_to_v3_tool_finding_facts",
            step_index,
            step_total,
            version,
            3,
        ));
        let started = Instant::now();
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
        on_progress(MigrationStepEvent::finished(
            "v1v2_to_v3_tool_finding_facts",
            step_index,
            step_total,
            version,
            3,
            started.elapsed(),
        ));
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
        step_index += 1;
        on_progress(MigrationStepEvent::started(
            "v3_to_v4_rollup_backfill",
            step_index,
            step_total,
            3,
            4,
        ));
        let started = Instant::now();
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
        on_progress(MigrationStepEvent::finished(
            "v3_to_v4_rollup_backfill",
            step_index,
            step_total,
            3,
            4,
            started.elapsed(),
        ));
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
        step_index += 1;
        on_progress(MigrationStepEvent::started(
            "v4_to_v5_cache_creation_backfill",
            step_index,
            step_total,
            4,
            5,
        ));
        let started = Instant::now();
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
        on_progress(MigrationStepEvent::finished(
            "v4_to_v5_cache_creation_backfill",
            step_index,
            step_total,
            4,
            5,
            started.elapsed(),
        ));
    }
    let version_before_project_identity: i64 =
        connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version_before_project_identity == 5 {
        // Project identity (#41) is a *session*-level dimension: it lives on
        // `durable_sessions`, never on a per-event fact/rollup table, so it
        // deliberately does not touch `durable_token_events` or the
        // `rollup_*` tables and is exempt from the two-table rollup-dimension
        // invariant that governs those (see `AGENTS.md`). `project_overrides`
        // and `project_session_overrides` are a separate user-controlled
        // overlay (aliases/merges/splits) resolved at read time; they never
        // rewrite the auto-computed columns added here or any source
        // transcript, and are fully reversible by deleting the override row.
        step_index += 1;
        on_progress(MigrationStepEvent::started(
            "v5_to_v6_project_identity_backfill",
            step_index,
            step_total,
            5,
            6,
        ));
        let started = Instant::now();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !table_has_column(&transaction, "durable_sessions", "project_key")? {
            transaction.execute_batch(
                "ALTER TABLE durable_sessions ADD COLUMN project_key TEXT;
                 ALTER TABLE durable_sessions ADD COLUMN project_label TEXT;
                 ALTER TABLE durable_sessions ADD COLUMN project_provenance TEXT;
                 ALTER TABLE durable_sessions ADD COLUMN project_source_directory TEXT;
                 CREATE INDEX durable_sessions_project_idx ON durable_sessions(project_key);",
            )?;
        }
        transaction.execute_batch(
            "CREATE TABLE IF NOT EXISTS project_overrides (
               project_key TEXT PRIMARY KEY,
               display_label TEXT,
               canonical_project_key TEXT,
               updated_at_ms INTEGER NOT NULL
             );
             CREATE TABLE IF NOT EXISTS project_session_overrides (
               session_key TEXT PRIMARY KEY REFERENCES durable_sessions(session_key),
               project_key TEXT NOT NULL,
               updated_at_ms INTEGER NOT NULL
             );",
        )?;
        // Backfill every existing session's project identity. Unlike the
        // token/tool dimension backfills above, the source data
        // (`working_directory`) exists only inside each session's snapshot
        // JSON, not in a normalized per-event fact table — there is no SQL
        // `GROUP BY` that could compute it. This streams one session
        // snapshot at a time (mirroring the v1->v2 tool/finding-facts
        // backfill earlier in this function), so at most one snapshot blob
        // is resident at a time; it is bounded by session count, not event
        // count, so it stays cheap even on a multi-GB ledger.
        backfill_project_identities(
            &transaction,
            Some(&mut |done, total| {
                // Throttled like `scanner::scan_all`'s progress callback: the
                // endpoints plus every 100th session is smooth enough for a
                // UI without turning a multi-thousand-session backfill into
                // that many IPC events.
                if done == 1 || done == total || done % 100 == 0 {
                    on_progress(MigrationStepEvent::item_progress(
                        "v5_to_v6_project_identity_backfill",
                        step_index,
                        step_total,
                        5,
                        6,
                        done,
                        total,
                    ));
                }
            }),
        )?;
        transaction.execute_batch(
            "INSERT INTO history_meta(key, value) VALUES('schema_version', '6')
               ON CONFLICT(key) DO UPDATE SET value = excluded.value;
             PRAGMA user_version = 6;",
        )?;
        transaction.commit()?;
        on_progress(MigrationStepEvent::finished(
            "v5_to_v6_project_identity_backfill",
            step_index,
            step_total,
            5,
            6,
            started.elapsed(),
        ));
    }
    Ok(())
}

/// First pass of `backfill_project_identities`: gathers only session keys,
/// never a snapshot blob, so peak memory during this pass is bounded by
/// session count and key length, not by how large any snapshot is. Named so
/// `backfill_project_identity_selects_keys_before_any_snapshot_blob` can
/// assert its shape directly rather than needing a true peak-RSS
/// measurement, which is impractical in a unit test.
const BACKFILL_PROJECT_IDENTITY_KEYS_SQL: &str = "SELECT session_key FROM durable_sessions";

/// Per-session snapshot fetch used by the same backfill's loop body below:
/// scoped to exactly one session per query (`WHERE d.session_key = ?1`), so
/// at most one snapshot blob is resident at a time — never every session's
/// `session_json` collected into memory at once.
const BACKFILL_PROJECT_IDENTITY_SNAPSHOT_SQL: &str = "SELECT s.session_json
                 FROM durable_sessions d JOIN session_snapshots s
                   ON s.session_key = d.session_key
                  AND s.version = d.current_snapshot_version
                 WHERE d.session_key = ?1";

/// Resolves and stores project identity for every durable session whose
/// current snapshot deserializes cleanly. A snapshot that fails to
/// deserialize is skipped with a warning, exactly like the v1->v2 fact
/// backfill: it heals the next time that session is observed.
///
/// Two things this deliberately avoids, both measured problems on a real
/// corpus (`AGENTS.md` notes full sessions running ~200 MB in aggregate; a
/// real ledger here holds thousands of sessions):
///
/// - It never materializes every session's snapshot blob at once. Only the
///   (short) session keys are collected up front; each iteration then fetches
///   *one* snapshot with a targeted single-row query. That query's cursor is
///   fully closed by the time the following `UPDATE durable_sessions` runs —
///   necessary because SQLite does not support writing to a table through an
///   open `SELECT` cursor spanning that same table on one connection, which
///   is exactly what running the `UPDATE` under the original all-rows cursor
///   would have done. Peak memory is bounded by one snapshot, not the whole
///   database.
/// - It never probes the filesystem/Git once per session. Repository and
///   workspace-marker discovery (`resolve_directory`) depends only on the
///   working directory, so it is resolved at most once per *distinct*
///   directory string and reused for every session sharing one — the same
///   ~30x reduction `commands::resolve_working_directories` already relies
///   on (124 distinct directories against 4,083 sessions on a real corpus).
///   A session with no working directory never reaches the cache at all.
fn backfill_project_identities(
    transaction: &Transaction<'_>,
    mut on_item: Option<&mut dyn FnMut(usize, usize)>,
) -> Result<()> {
    let home = dirs::home_dir();

    let keys: Vec<String> = {
        let mut select = transaction.prepare(BACKFILL_PROJECT_IDENTITY_KEYS_SQL)?;
        let rows = select.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    let total = keys.len();

    let mut directory_cache: HashMap<String, crate::project_identity::DirectoryResolution> =
        HashMap::new();

    for (index, key) in keys.into_iter().enumerate() {
        if let Some(callback) = on_item.as_deref_mut() {
            callback(index + 1, total);
        }
        let raw: Option<Vec<u8>> = transaction
            .query_row(
                BACKFILL_PROJECT_IDENTITY_SNAPSHOT_SQL,
                [key.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            // No current snapshot recorded for this session; nothing to
            // backfill (it heals on its next observe).
            continue;
        };
        let session: Session = match serde_json::from_slice(&raw) {
            Ok(session) => session,
            Err(error) => {
                tracing::warn!(
                    "could not backfill project identity for {key}: snapshot did not deserialize: {error}"
                );
                continue;
            }
        };
        let identity = resolve_with_directory_cache(
            session.working_directory.as_deref(),
            &session.harness,
            &session.file_path,
            home.as_deref(),
            &mut directory_cache,
            crate::project_identity::resolve_directory,
        );
        transaction.execute(
            "UPDATE durable_sessions
             SET project_key = ?2, project_label = ?3, project_provenance = ?4, project_source_directory = ?5
             WHERE session_key = ?1",
            params![
                key,
                identity.as_ref().map(|i| i.project_key.as_str()),
                identity.as_ref().map(|i| i.label.as_str()),
                identity.as_ref().map(|i| i.provenance.as_str()),
                session.working_directory,
            ],
        )?;
    }
    Ok(())
}

/// Resolves one session's project identity against `cache`, calling
/// `resolve_directory` (the filesystem/Git-probing tier) only on a cache
/// miss for its exact working-directory string. Split out from
/// `backfill_project_identities` so the "at most one probe per distinct
/// directory" guarantee is directly testable with a counting stand-in for
/// `resolve_directory` instead of real repositories on disk — see
/// `resolving_many_sessions_probes_each_distinct_directory_at_most_once`.
fn resolve_with_directory_cache<F>(
    working_directory: Option<&str>,
    harness: &crate::provider::ProviderId,
    file_path: &str,
    home: Option<&Path>,
    cache: &mut HashMap<String, crate::project_identity::DirectoryResolution>,
    mut resolve_directory: F,
) -> Option<crate::project_identity::ProjectIdentity>
where
    F: FnMut(&str, Option<&Path>) -> crate::project_identity::DirectoryResolution,
{
    let directory = working_directory?;
    let resolution = cache
        .entry(directory.to_owned())
        .or_insert_with(|| resolve_directory(directory, home));
    Some(match &resolution.identity {
        Some(identity) => identity.clone(),
        None => crate::project_identity::resolve_fallback_identity(
            directory,
            &resolution.resolved,
            harness,
            file_path,
            home,
        ),
    })
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

/// Resolves (or reuses) project identity onto `session` in place, and
/// persists it onto `durable_sessions` for `key`, which must already exist.
///
/// The previous durable value is reused whenever `working_directory` is
/// unchanged, so a filesystem/git probe does not run on every token-event
/// append for an already-classified session — only on first observation and
/// after a genuine working-directory change (a resumed session recorded
/// under a different `cwd`, for example).
fn apply_project_identity(
    transaction: &Transaction<'_>,
    key: &str,
    session: &mut Session,
) -> Result<()> {
    let current_directory = session.working_directory.clone();
    let (prev_key, prev_label, prev_provenance, prev_directory): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = transaction.query_row(
        "SELECT project_key, project_label, project_provenance, project_source_directory
         FROM durable_sessions WHERE session_key = ?1",
        [key],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if prev_key.is_some() && prev_directory == current_directory {
        session.project_key = prev_key;
        session.project_label = prev_label;
        session.project_provenance = prev_provenance
            .as_deref()
            .and_then(crate::project_identity::ProjectProvenance::parse);
        return Ok(());
    }
    let identity = crate::project_identity::resolve_project_identity(
        current_directory.as_deref(),
        &session.harness,
        &session.file_path,
        dirs::home_dir().as_deref(),
    );
    session.project_key = identity.as_ref().map(|i| i.project_key.clone());
    session.project_label = identity.as_ref().map(|i| i.label.clone());
    session.project_provenance = identity.as_ref().map(|i| i.provenance);
    transaction.execute(
        "UPDATE durable_sessions
         SET project_key = ?2, project_label = ?3, project_provenance = ?4, project_source_directory = ?5
         WHERE session_key = ?1",
        params![
            key,
            session.project_key,
            session.project_label,
            session.project_provenance.map(|p| p.as_str()),
            current_directory,
        ],
    )?;
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

fn load_project_overrides(connection: &Connection) -> Result<HashMap<String, ProjectOverrideRow>> {
    let mut statement = connection.prepare(
        "SELECT project_key, display_label, canonical_project_key FROM project_overrides",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(ProjectOverrideRow {
                project_key: row.get(0)?,
                display_label: row.get(1)?,
                canonical_project_key: row.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows
        .into_iter()
        .map(|row| (row.project_key.clone(), row))
        .collect())
}

/// Deletes an override row once it carries neither an alias nor a merge
/// redirect, so a cleared alias or unmerge does not leave empty clutter.
fn prune_empty_project_override(connection: &Connection, project_key: &str) -> Result<()> {
    connection.execute(
        "DELETE FROM project_overrides
         WHERE project_key = ?1 AND display_label IS NULL AND canonical_project_key IS NULL",
        [project_key],
    )?;
    Ok(())
}

/// Chases `canonical_project_key` redirects to the final, non-redirected
/// project key. Cycle-safe: a chain that revisits a key it has already
/// visited (which `merge_project`'s own validation prevents in the normal
/// path, but a hand-edited database might not) stops at the repeated key
/// rather than looping forever.
pub fn resolve_canonical_project_key(
    overrides: &HashMap<String, ProjectOverrideRow>,
    start: &str,
) -> String {
    let mut current = start.to_string();
    let mut seen = std::collections::HashSet::new();
    while seen.insert(current.clone()) {
        match overrides
            .get(&current)
            .and_then(|row| row.canonical_project_key.clone())
        {
            Some(next) if next != current => current = next,
            _ => break,
        }
    }
    current
}

/// Whether following `canonical_project_key` redirects from `start` ever
/// reaches `target`. Used by `merge_project` to reject a merge that would
/// otherwise create a cycle.
fn project_key_reaches(
    overrides: &HashMap<String, ProjectOverrideRow>,
    start: &str,
    target: &str,
) -> bool {
    let mut current = start.to_string();
    let mut seen = std::collections::HashSet::new();
    while seen.insert(current.clone()) {
        if current == target {
            return true;
        }
        match overrides
            .get(&current)
            .and_then(|row| row.canonical_project_key.clone())
        {
            Some(next) if next != current => current = next,
            _ => return false,
        }
    }
    false
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
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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
            project_key: None,
            project_label: None,
            project_provenance: None,
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

    fn project_test_git(repo: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn project_test_repository() -> tempfile::TempDir {
        let directory = tempdir().unwrap();
        project_test_git(directory.path(), &["init", "--quiet"]);
        project_test_git(directory.path(), &["config", "user.name", "Synthetic Test"]);
        project_test_git(
            directory.path(),
            &["config", "user.email", "synthetic@example.invalid"],
        );
        project_test_git(directory.path(), &["config", "commit.gpgsign", "false"]);
        directory
    }

    #[test]
    fn observe_persists_project_identity_and_reuses_it_when_the_directory_is_unchanged() {
        let repo = project_test_repository();
        let (_directory, store) = store();
        let mut fixture = session("project-thread", 10);
        fixture.working_directory = Some(repo.path().to_str().unwrap().to_string());
        let generation = store.begin_scan().unwrap();
        let first = store
            .observe(Path::new("project.jsonl"), &fixture, generation)
            .unwrap();
        assert_eq!(
            first.session.project_provenance,
            Some(crate::project_identity::ProjectProvenance::RepositoryRoot)
        );
        assert!(first.session.project_key.is_some());

        // The repository is removed before the second observe: if identity
        // were recomputed from disk it would fall back to a path identity
        // (a different provenance). Getting `RepositoryRoot` back proves the
        // durable value was reused rather than re-probed.
        drop(repo);
        append_event(&mut fixture, 5, "2026-01-01T00:00:02Z");
        let second = store
            .observe(Path::new("project.jsonl"), &fixture, generation)
            .unwrap();
        assert_eq!(second.session.project_key, first.session.project_key);
        assert_eq!(
            second.session.project_provenance,
            Some(crate::project_identity::ProjectProvenance::RepositoryRoot)
        );
    }

    #[test]
    fn observe_recomputes_project_identity_after_a_working_directory_change() {
        let repo_a = project_test_repository();
        let repo_b = project_test_repository();
        let (_directory, store) = store();
        let mut fixture = session("project-move", 10);
        fixture.working_directory = Some(repo_a.path().to_str().unwrap().to_string());
        let generation = store.begin_scan().unwrap();
        let first = store
            .observe(Path::new("project-move.jsonl"), &fixture, generation)
            .unwrap();

        fixture.working_directory = Some(repo_b.path().to_str().unwrap().to_string());
        append_event(&mut fixture, 5, "2026-01-01T00:00:02Z");
        let second = store
            .observe(Path::new("project-move.jsonl"), &fixture, generation)
            .unwrap();
        assert_ne!(second.session.project_key, first.session.project_key);
    }

    #[test]
    fn sessions_without_a_working_directory_have_no_project_identity() {
        let (_directory, store) = store();
        let fixture = session("no-cwd", 10);
        assert!(fixture.working_directory.is_none());
        let generation = store.begin_scan().unwrap();
        let observed = store
            .observe(Path::new("no-cwd.jsonl"), &fixture, generation)
            .unwrap();
        assert!(observed.session.project_key.is_none());
        assert!(observed.session.project_label.is_none());
        assert!(observed.session.project_provenance.is_none());
    }

    #[test]
    fn v5_migration_backfills_project_identity_from_existing_snapshots() {
        let repo = project_test_repository();
        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let fixture = {
            let mut fixture = session("project-backfill", 10);
            fixture.working_directory = Some(repo.path().to_str().unwrap().to_string());
            fixture
        };
        let key;
        {
            let store = HistoryStore::open(&path).unwrap();
            let generation = store.begin_scan().unwrap().max(1);
            key = store
                .observe(Path::new("project-backfill.jsonl"), &fixture, generation)
                .unwrap()
                .key;
        }
        // Rewind to schema v5: an existing pre-#41 store has no project
        // identity backfilled yet and no override tables. Clearing the
        // column *values* (rather than dropping and recreating the table,
        // which would fight the other tables' foreign keys into it) models
        // the same pre-migration state the real v5->v6 step must repair.
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "UPDATE durable_sessions SET
                       project_key = NULL, project_label = NULL,
                       project_provenance = NULL, project_source_directory = NULL;
                     DROP TABLE IF EXISTS project_overrides;
                     DROP TABLE IF EXISTS project_session_overrides;
                     INSERT INTO history_meta(key, value) VALUES('schema_version', '5')
                       ON CONFLICT(key) DO UPDATE SET value = '5';
                     PRAGMA user_version = 5;",
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
        let (project_key, project_label, provenance): (Option<String>, Option<String>, Option<String>) = connection
            .query_row(
                "SELECT project_key, project_label, project_provenance FROM durable_sessions WHERE session_key = ?1",
                [key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert!(project_key.is_some());
        assert!(project_label.is_some());
        assert_eq!(provenance.as_deref(), Some("repository_root"));
        drop(connection);

        let loaded = store.load_one(&key).unwrap();
        assert_eq!(loaded.session.project_key, project_key);
    }

    #[test]
    #[ignore = "performance probe; run with --release --ignored --nocapture"]
    fn performance_v5_migration_backfill_4000_sessions_120_directories() {
        // Deliberately non-existent working directories: `resolve_directory`
        // fails `canonicalize` immediately and falls through to the
        // `fallback_path_identity` tier without a real `gix::discover` walk.
        // This isolates and measures the cost this PR actually adds inside
        // the migration's write transaction — per-session SQL point
        // queries/updates plus the directory-cache bookkeeping — separate
        // from Git discovery cost, which is capped at one probe per distinct
        // directory and is the same cost `resolve_working_directories`
        // already pays interactively today (measured elsewhere at 124
        // distinct directories for 4,083 sessions on a real corpus). Total
        // real-world backfill time is this measurement plus that many
        // `gix::discover` calls, not multiplied by session count.
        const SESSIONS: usize = 4_000;
        const DIRECTORIES: usize = 120;

        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        {
            let store = HistoryStore::open(&path).unwrap();
            let generation = store.begin_scan().unwrap().max(1);
            for index in 0..SESSIONS {
                let mut fixture = session(&format!("perf-{index}"), 10);
                fixture.working_directory = Some(format!(
                    "/synthetic/does-not-exist/project-{}",
                    index % DIRECTORIES
                ));
                store
                    .observe(
                        Path::new(&format!("perf-{index}.jsonl")),
                        &fixture,
                        generation,
                    )
                    .unwrap();
            }
        }
        {
            let connection = Connection::open(&path).unwrap();
            connection
                .execute_batch(
                    "UPDATE durable_sessions SET
                       project_key = NULL, project_label = NULL,
                       project_provenance = NULL, project_source_directory = NULL;
                     DROP TABLE IF EXISTS project_overrides;
                     DROP TABLE IF EXISTS project_session_overrides;
                     INSERT INTO history_meta(key, value) VALUES('schema_version', '5')
                       ON CONFLICT(key) DO UPDATE SET value = '5';
                     PRAGMA user_version = 5;",
                )
                .unwrap();
        }

        let started = std::time::Instant::now();
        let store = HistoryStore::open(&path).unwrap();
        let elapsed = started.elapsed();
        eprintln!(
            "v5->v6 migration backfill: {SESSIONS} sessions across {DIRECTORIES} distinct \
             (non-existent, no Git discovery) directories in {elapsed:?}"
        );

        let count: i64 = store
            .connection()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM durable_sessions WHERE project_key IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, SESSIONS as i64);
    }

    #[test]
    fn set_project_alias_overrides_the_label_and_is_reversible() {
        let (_directory, store) = store();
        store
            .set_project_alias("repo:abc", Some("Renamed"))
            .unwrap();
        let overrides = store.list_project_overrides().unwrap();
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].display_label.as_deref(), Some("Renamed"));
        assert_eq!(overrides[0].canonical_project_key, None);

        // Clearing the alias (None) removes the now-empty override row.
        store.set_project_alias("repo:abc", None).unwrap();
        assert!(store.list_project_overrides().unwrap().is_empty());
    }

    #[test]
    fn merge_project_redirects_and_rejects_cycles() {
        let (_directory, store) = store();
        store.merge_project("repo:a", "repo:b").unwrap();
        let overrides: HashMap<_, _> = store
            .list_project_overrides()
            .unwrap()
            .into_iter()
            .map(|row| (row.project_key.clone(), row))
            .collect();
        assert_eq!(
            resolve_canonical_project_key(&overrides, "repo:a"),
            "repo:b"
        );

        // Merging b back into a would create a cycle and must be rejected.
        assert!(store.merge_project("repo:b", "repo:a").is_err());
        // Merging a key into itself is always rejected.
        assert!(store.merge_project("repo:c", "repo:c").is_err());

        // Unmerge reverses the redirect (the "split" undo for a merge).
        store.unmerge_project("repo:a").unwrap();
        assert!(store.list_project_overrides().unwrap().is_empty());
    }

    #[test]
    fn merge_preserves_an_existing_alias_and_alias_preserves_an_existing_merge() {
        let (_directory, store) = store();
        store.set_project_alias("repo:a", Some("Alpha")).unwrap();
        store.merge_project("repo:a", "repo:b").unwrap();
        let overrides = store.list_project_overrides().unwrap();
        let row = overrides
            .iter()
            .find(|row| row.project_key == "repo:a")
            .unwrap();
        assert_eq!(row.display_label.as_deref(), Some("Alpha"));
        assert_eq!(row.canonical_project_key.as_deref(), Some("repo:b"));

        store.set_project_alias("repo:a", Some("Alpha 2")).unwrap();
        let overrides = store.list_project_overrides().unwrap();
        let row = overrides
            .iter()
            .find(|row| row.project_key == "repo:a")
            .unwrap();
        assert_eq!(row.display_label.as_deref(), Some("Alpha 2"));
        assert_eq!(row.canonical_project_key.as_deref(), Some("repo:b"));
    }

    #[test]
    fn reassign_session_project_splits_a_session_and_is_reversible() {
        let (_directory, store) = store();
        let generation = store.begin_scan().unwrap();
        let observed = store
            .observe(Path::new("split.jsonl"), &session("split", 10), generation)
            .unwrap();

        let manual_key = store.reassign_session_project(&observed.key, None).unwrap();
        assert!(manual_key.starts_with("manual:"));
        let overrides = store.list_session_project_overrides().unwrap();
        assert_eq!(overrides.get(&observed.key), Some(&manual_key));

        store
            .reassign_session_project(&observed.key, Some("repo:other"))
            .unwrap();
        let overrides = store.list_session_project_overrides().unwrap();
        assert_eq!(
            overrides.get(&observed.key),
            Some(&"repo:other".to_string())
        );

        store.clear_session_project_override(&observed.key).unwrap();
        assert!(store.list_session_project_overrides().unwrap().is_empty());
    }

    #[test]
    fn reassign_session_project_rejects_an_unknown_session() {
        let (_directory, store) = store();
        assert!(store
            .reassign_session_project("does-not-exist", Some("repo:x"))
            .is_err());
    }

    /// The backfill migration's whole reason for grouping by directory
    /// first (rather than resolving every session independently) is to
    /// avoid thousands of Git/filesystem probes on a real corpus. This
    /// proves the cache actually delivers that: a counting stand-in for the
    /// expensive `resolve_directory` tier, called through the same
    /// `resolve_with_directory_cache` the real backfill uses, must be
    /// invoked exactly once per distinct working directory no matter how
    /// many sessions share it — and never at all for a session with none.
    #[test]
    fn resolving_many_sessions_probes_each_distinct_directory_at_most_once() {
        use std::cell::RefCell;

        let probe_calls: RefCell<HashMap<String, u32>> = RefCell::new(HashMap::new());
        let resolve = |directory: &str, _home: Option<&Path>| {
            *probe_calls
                .borrow_mut()
                .entry(directory.to_string())
                .or_insert(0) += 1;
            crate::project_identity::DirectoryResolution {
                resolved: PathBuf::from(directory),
                identity: Some(crate::project_identity::ProjectIdentity {
                    project_key: format!("repo:{directory}"),
                    label: directory.to_string(),
                    provenance: crate::project_identity::ProjectProvenance::RepositoryRoot,
                }),
            }
        };

        // Ten sessions, only two distinct working directories, plus one
        // session with none at all.
        let sessions: [(Option<&str>, &str); 10] = [
            (Some("/repo/alpha"), "a1.jsonl"),
            (Some("/repo/alpha"), "a2.jsonl"),
            (Some("/repo/beta"), "b1.jsonl"),
            (Some("/repo/alpha"), "a3.jsonl"),
            (Some("/repo/beta"), "b2.jsonl"),
            (Some("/repo/alpha"), "a4.jsonl"),
            (None, "no-cwd.jsonl"),
            (Some("/repo/beta"), "b3.jsonl"),
            (Some("/repo/alpha"), "a5.jsonl"),
            (Some("/repo/beta"), "b4.jsonl"),
        ];

        let mut cache: HashMap<String, crate::project_identity::DirectoryResolution> =
            HashMap::new();
        for (working_directory, file_path) in sessions {
            let identity = resolve_with_directory_cache(
                working_directory,
                &codex_provider_id(),
                file_path,
                None,
                &mut cache,
                resolve,
            );
            assert_eq!(identity.is_some(), working_directory.is_some());
        }

        let counts = probe_calls.borrow();
        assert_eq!(counts.get("/repo/alpha"), Some(&1));
        assert_eq!(counts.get("/repo/beta"), Some(&1));
        assert_eq!(
            counts.len(),
            2,
            "a session with no working directory must never reach the resolver"
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

    // -----------------------------------------------------------------
    // Structural invariants (test/ledger-structural-invariants).
    //
    // These tests are deliberately derived from the Rust types and the
    // schema itself rather than from a hand-maintained fixture list, so a
    // newly added token/tool dimension, a colliding schema version, or an
    // unbounded migration backfill fails loudly instead of quietly passing
    // CI the way the defects described in AGENTS.md's rollup-dimension
    // invariant did.
    // -----------------------------------------------------------------

    /// Every column name of `table`, read directly from SQLite rather than
    /// from any Rust-side listing, so this can never itself drift from the
    /// live schema.
    fn table_columns(connection: &Connection, table: &str) -> BTreeSet<String> {
        let mut statement = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let names = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap();
        names
            .collect::<std::result::Result<BTreeSet<_>, _>>()
            .unwrap()
    }

    #[test]
    fn token_totals_fields_have_matching_rollup_columns() {
        // Invariant 1 (highest value): every `TokenTotals` field must have a
        // same-named column on `rollup_token_totals`, or `range_totals_multi`
        // silently returns 0 for that dimension on every whole-hour bucket
        // while still reporting it correctly at the sub-hour edges — exactly
        // the `cache_creation_input_tokens` near-miss AGENTS.md documents.
        let (_directory, store) = store();
        let connection = store.connection().unwrap();

        let fields: BTreeSet<String> = serde_json::to_value(TokenTotals::default())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();

        let key_columns: BTreeSet<&str> = ["session_key", "hour_bucket", "model", "service_tier"]
            .into_iter()
            .collect();
        let rollup_dimensions: BTreeSet<String> = table_columns(&connection, "rollup_token_totals")
            .into_iter()
            .filter(|column| !key_columns.contains(column.as_str()))
            .collect();

        assert_eq!(
            fields, rollup_dimensions,
            "TokenTotals fields and rollup_token_totals's non-key columns diverged; every \
             token dimension needs a matching rollup column (AGENTS.md's ledger-rollup-\
             dimension invariant) — extend the fact table, the rollup table, the migration \
             backfill, and both the rollup write and read paths together"
        );
    }

    #[test]
    fn tool_metrics_fields_match_rollup_columns_or_documented_allowlist() {
        // Invariant 1: same idea as `token_totals_fields_have_matching_rollup_columns`,
        // but `ToolMetrics` has three fields — `mutation_targets`,
        // `one_shot_mutations`, `retry_count` — that are *deliberately* not
        // plain rollup columns. They are defined over distinct
        // `(turn_id, target)` chains (see `telemetry::mutation_chain_fields`
        // and `compute_range_totals` above); summing per-bucket values for
        // these three would double-count or miscount a chain that straddles
        // a rollup bucket boundary, so they are reconstructed at read time
        // from `rollup_mutation_chains` instead. The allowlist below exists
        // so a 15th field forces a deliberate choice — a rollup column or an
        // explicit, justified addition here — instead of silently
        // defaulting to an under-count.
        let (_directory, store) = store();
        let connection = store.connection().unwrap();

        let fields: BTreeSet<String> = serde_json::to_value(ToolMetrics::default())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();

        let key_columns: BTreeSet<&str> = ["session_key", "hour_bucket", "model"]
            .into_iter()
            .collect();
        let rollup_columns: BTreeSet<String> = table_columns(&connection, "rollup_tool_metrics")
            .into_iter()
            .filter(|column| !key_columns.contains(column.as_str()))
            .collect();

        // Allowlist: fields reconstructed from `rollup_mutation_chains`
        // rather than stored as plain rollup columns, and why each cannot be
        // a plain summed column.
        let derived_from_mutation_chains: BTreeSet<&str> = [
            // Distinct-chain count: summing per-bucket counts would count a
            // chain once per bucket it touches instead of once overall.
            "mutation_targets",
            // Depends on a chain's *total* count across every bucket it
            // touches (a chain is "one-shot" only if it never repeats
            // anywhere) — not decidable from any single bucket's partial
            // count.
            "one_shot_mutations",
            // retries = total chain length minus one; the same
            // straddling problem as `mutation_targets` applies to the
            // per-chain lengths this is computed from.
            "retry_count",
        ]
        .into_iter()
        .collect();

        let expected_plain_columns: BTreeSet<String> = fields
            .iter()
            .filter(|field| !derived_from_mutation_chains.contains(field.as_str()))
            .cloned()
            .collect();

        assert_eq!(
            expected_plain_columns, rollup_columns,
            "ToolMetrics fields (minus the documented mutation-chain-derived allowlist) must \
             match rollup_tool_metrics's non-key columns exactly; a newly added field needs \
             either a rollup column or a deliberate, justified addition to the allowlist in \
             this test"
        );

        // Guard the allowlist itself: every allowlisted name must still be a
        // real ToolMetrics field (catches a stale/renamed entry), and must
        // NOT also have a rollup column (catches "fixing" this test by both
        // allowlisting a field and adding a column for it, which would
        // silently resurrect the double-count/miscount bug the allowlist
        // exists to prevent).
        for name in &derived_from_mutation_chains {
            assert!(
                fields.contains(*name),
                "allowlisted field {name} is not a ToolMetrics field"
            );
            assert!(
                !rollup_columns.contains(*name),
                "allowlisted field {name} unexpectedly also has a rollup_tool_metrics column; \
                 pick one derivation, not both"
            );
        }
    }

    /// Normalized DDL fingerprint of every table and index in the schema.
    /// Two migrations that both claim `SCHEMA_VERSION` but define different
    /// table shapes look identical under `PRAGMA user_version` alone; this
    /// only diverges in `sqlite_master`'s actual SQL text, which is exactly
    /// what this fingerprints.
    fn schema_fingerprint(connection: &Connection) -> String {
        let mut statement = connection
            .prepare(
                "SELECT type, name, sql FROM sqlite_master
                 WHERE sql IS NOT NULL AND type IN ('table', 'index')
                 ORDER BY type, name",
            )
            .unwrap();
        let mut lines: Vec<String> = statement
            .query_map([], |row| {
                let kind: String = row.get(0)?;
                let name: String = row.get(1)?;
                let sql: String = row.get(2)?;
                // Whitespace-normalize so incidental formatting (added blank
                // lines, re-indentation) never trips the fingerprint — only
                // an actual change to a table/index's shape should.
                let normalized_sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(format!("{kind}:{name}:{normalized_sql}"))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        lines.sort();
        lines.join("\n")
    }

    /// Committed schema fingerprint for `SCHEMA_VERSION`. Changing this
    /// value means the schema's actual shape changed — a new table, a
    /// new/renamed/retyped column, or a new index — and that change must go
    /// through a reviewed migration step (a new `if version == N` block in
    /// `migrate()`) with `SCHEMA_VERSION` bumped, never a silent edit to an
    /// existing `CREATE TABLE`/`CREATE INDEX`. Regenerate by printing
    /// `schema_fingerprint(&store.connection().unwrap())` from a fresh
    /// `HistoryStore::open` and pasting the result below.
    const EXPECTED_SCHEMA_FINGERPRINT: &str = "index:durable_finding_events_session_idx:CREATE INDEX durable_finding_events_session_idx ON durable_finding_events(session_key)\nindex:durable_sessions_identity_idx:CREATE INDEX durable_sessions_identity_idx ON durable_sessions(identity_key)\nindex:durable_sessions_project_idx:CREATE INDEX durable_sessions_project_idx ON durable_sessions(project_key)\nindex:durable_token_events_session_timestamp_idx:CREATE INDEX durable_token_events_session_timestamp_idx ON durable_token_events(session_key, timestamp_ms)\nindex:durable_tool_events_session_timestamp_idx:CREATE INDEX durable_tool_events_session_timestamp_idx ON durable_tool_events(session_key, timestamp_ms)\nindex:rollup_mutation_chains_key_idx:CREATE UNIQUE INDEX rollup_mutation_chains_key_idx ON rollup_mutation_chains(session_key, hour_bucket, model, turn_id, target)\nindex:rollup_token_totals_key_idx:CREATE UNIQUE INDEX rollup_token_totals_key_idx ON rollup_token_totals(session_key, hour_bucket, model, service_tier)\nindex:rollup_tool_metrics_key_idx:CREATE UNIQUE INDEX rollup_tool_metrics_key_idx ON rollup_tool_metrics(session_key, hour_bucket, model)\nindex:source_locations_session_idx:CREATE INDEX source_locations_session_idx ON source_locations(session_key, present)\ntable:durable_finding_events:CREATE TABLE durable_finding_events ( session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), timestamp_ms INTEGER, rule_id TEXT NOT NULL, severity TEXT NOT NULL, avoidable_calls INTEGER NOT NULL )\ntable:durable_sessions:CREATE TABLE durable_sessions ( session_key TEXT PRIMARY KEY, identity_key TEXT NOT NULL, first_event_fingerprint TEXT NOT NULL, fingerprint_is_final INTEGER NOT NULL, collision INTEGER NOT NULL DEFAULT 0, current_snapshot_version INTEGER NOT NULL DEFAULT 0, current_snapshot_hash TEXT, created_at_ms INTEGER NOT NULL, last_seen_at_ms INTEGER NOT NULL, ledger_dirty INTEGER NOT NULL DEFAULT 0, project_key TEXT, project_label TEXT, project_provenance TEXT, project_source_directory TEXT )\ntable:durable_token_events:CREATE TABLE durable_token_events ( session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), event_key TEXT NOT NULL, event_index INTEGER NOT NULL, timestamp_ms INTEGER NOT NULL, model TEXT, service_tier TEXT, request_input_tokens INTEGER, cumulative_total_tokens INTEGER NOT NULL, input_tokens INTEGER NOT NULL, cached_input_tokens INTEGER NOT NULL, cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL, reasoning_output_tokens INTEGER NOT NULL, total_tokens INTEGER NOT NULL, PRIMARY KEY(session_key, event_key) )\ntable:durable_tool_events:CREATE TABLE durable_tool_events ( session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), timestamp_ms INTEGER NOT NULL, model TEXT, kind TEXT NOT NULL, outcome TEXT NOT NULL, turn_id TEXT, target TEXT, duration_ms INTEGER, output_bytes INTEGER NOT NULL )\ntable:history_meta:CREATE TABLE history_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)\ntable:project_overrides:CREATE TABLE project_overrides ( project_key TEXT PRIMARY KEY, display_label TEXT, canonical_project_key TEXT, updated_at_ms INTEGER NOT NULL )\ntable:project_session_overrides:CREATE TABLE project_session_overrides ( session_key TEXT PRIMARY KEY REFERENCES durable_sessions(session_key), project_key TEXT NOT NULL, updated_at_ms INTEGER NOT NULL )\ntable:rollup_mutation_chains:CREATE TABLE rollup_mutation_chains ( session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), hour_bucket INTEGER NOT NULL, model TEXT NOT NULL DEFAULT '', turn_id TEXT NOT NULL DEFAULT '', target TEXT NOT NULL DEFAULT '', mutation_count INTEGER NOT NULL DEFAULT 0 )\ntable:rollup_token_totals:CREATE TABLE rollup_token_totals ( session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), hour_bucket INTEGER NOT NULL, model TEXT NOT NULL DEFAULT '', service_tier TEXT NOT NULL DEFAULT '', input_tokens INTEGER NOT NULL DEFAULT 0, cached_input_tokens INTEGER NOT NULL DEFAULT 0, cache_creation_input_tokens INTEGER NOT NULL DEFAULT 0, output_tokens INTEGER NOT NULL DEFAULT 0, reasoning_output_tokens INTEGER NOT NULL DEFAULT 0, total_tokens INTEGER NOT NULL DEFAULT 0 )\ntable:rollup_tool_metrics:CREATE TABLE rollup_tool_metrics ( session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), hour_bucket INTEGER NOT NULL, model TEXT NOT NULL DEFAULT '', calls INTEGER NOT NULL DEFAULT 0, reads INTEGER NOT NULL DEFAULT 0, searches INTEGER NOT NULL DEFAULT 0, mutations INTEGER NOT NULL DEFAULT 0, commands INTEGER NOT NULL DEFAULT 0, other INTEGER NOT NULL DEFAULT 0, successes INTEGER NOT NULL DEFAULT 0, failures INTEGER NOT NULL DEFAULT 0, unknown INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER NOT NULL DEFAULT 0, output_bytes INTEGER NOT NULL DEFAULT 0 )\ntable:session_snapshots:CREATE TABLE session_snapshots ( session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), version INTEGER NOT NULL, format_version INTEGER NOT NULL, snapshot_hash TEXT NOT NULL, captured_at_ms INTEGER NOT NULL, session_json BLOB NOT NULL, PRIMARY KEY(session_key, version) )\ntable:source_artifacts:CREATE TABLE source_artifacts ( artifact_key TEXT PRIMARY KEY, identity_key TEXT NOT NULL, first_event_fingerprint TEXT NOT NULL, session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), created_at_ms INTEGER NOT NULL, last_seen_at_ms INTEGER NOT NULL )\ntable:source_locations:CREATE TABLE source_locations ( path TEXT PRIMARY KEY, artifact_key TEXT NOT NULL REFERENCES source_artifacts(artifact_key), session_key TEXT NOT NULL REFERENCES durable_sessions(session_key), present INTEGER NOT NULL, first_seen_at_ms INTEGER NOT NULL, last_seen_at_ms INTEGER NOT NULL, seen_generation INTEGER NOT NULL DEFAULT 0 )";

    #[test]
    fn schema_fingerprint_matches_committed_expected_value() {
        // Invariant 2: this is what actually catches two branches that both
        // bump to the same `SCHEMA_VERSION` with different table shapes —
        // `PRAGMA user_version` alone cannot tell them apart, but their DDL
        // text differs, and this fingerprints exactly that.
        let (_directory, store) = store();
        let connection = store.connection().unwrap();
        let fingerprint = schema_fingerprint(&connection);
        assert_eq!(
            fingerprint, EXPECTED_SCHEMA_FINGERPRINT,
            "the durable-ledger schema's actual shape changed without updating \
             EXPECTED_SCHEMA_FINGERPRINT. If this is a deliberate, reviewed schema change, it \
             must go through a new migration step with SCHEMA_VERSION bumped; then regenerate \
             this constant from schema_fingerprint(&store.connection().unwrap())"
        );
    }

    /// Order-insensitive structural shape of every table and index: each
    /// table becomes its columns' (name, declared type, NOT NULL, default,
    /// primary-key rank) tuples plus its foreign-key references, sorted by
    /// column name; each index becomes its normalized `CREATE INDEX` text.
    ///
    /// Deliberately *not* `schema_fingerprint`, which is order-sensitive:
    /// the fresh-database path (`migrate()`'s `version == 0` branch, whose
    /// `CREATE TABLE` lists every column inline) and the fully-migrated
    /// path (whose legacy columns arrive via `ALTER TABLE ... ADD COLUMN`
    /// and land at the end) genuinely and harmlessly declare
    /// `durable_token_events`'s columns in a different order — this
    /// codebase always addresses columns by name, never positionally, so
    /// that difference is not a real shape divergence. What *would* be a
    /// real divergence — a missing, extra, retyped, or renamed column, a
    /// changed constraint, or a missing index — still fails this check.
    fn schema_shape(connection: &Connection) -> Vec<String> {
        let mut tables: Vec<String> = {
            let mut statement = connection
                .prepare(
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND sql IS NOT NULL",
                )
                .unwrap();
            statement
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        tables.sort();

        let mut lines = Vec::new();
        for table in &tables {
            let mut column_statement = connection
                .prepare(&format!("PRAGMA table_info('{table}')"))
                .unwrap();
            let mut columns: Vec<String> = column_statement
                .query_map([], |row| {
                    let name: String = row.get(1)?;
                    let declared_type: String = row.get(2)?;
                    let not_null: i64 = row.get(3)?;
                    let default_value: Option<String> = row.get(4)?;
                    let primary_key_rank: i64 = row.get(5)?;
                    Ok(format!(
                        "{name}:{declared_type}:{not_null}:{}:{primary_key_rank}",
                        default_value.unwrap_or_default()
                    ))
                })
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            columns.sort();

            let mut fk_statement = connection
                .prepare(&format!("PRAGMA foreign_key_list('{table}')"))
                .unwrap();
            let mut foreign_keys: Vec<String> = fk_statement
                .query_map([], |row| {
                    let referenced_table: String = row.get(2)?;
                    let from_column: String = row.get(3)?;
                    let to_column: String = row.get(4)?;
                    Ok(format!("{from_column}->{referenced_table}.{to_column}"))
                })
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap();
            foreign_keys.sort();

            lines.push(format!(
                "table:{table}:columns=[{}]:fks=[{}]",
                columns.join(","),
                foreign_keys.join(",")
            ));
        }

        let mut index_statement = connection
            .prepare(
                "SELECT name, sql FROM sqlite_master
                 WHERE type = 'index' AND sql IS NOT NULL",
            )
            .unwrap();
        let indexes: Vec<String> = index_statement
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let sql: String = row.get(1)?;
                let normalized_sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
                Ok(format!("index:{name}:{normalized_sql}"))
            })
            .unwrap()
            .collect::<std::result::Result<Vec<_>, _>>()
            .unwrap();
        lines.extend(indexes);
        lines.sort();
        lines
    }

    #[test]
    fn fresh_and_fully_migrated_databases_produce_identical_schema_shapes() {
        // Invariant 2: the fresh-database path (`migrate()`'s `version == 0`
        // branch) and the fully-migrated-from-oldest path are built
        // separately in this file and have drifted before; this pins them to
        // agree on every table's columns, constraints, foreign keys, and
        // indexes (see `schema_shape` for why this is order-insensitive
        // rather than reusing the strict `schema_fingerprint`).
        let (_fresh_directory, fresh_store) = store();
        let fresh_shape = schema_shape(&fresh_store.connection().unwrap());

        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        {
            let connection = Connection::open(&path).unwrap();
            connection.execute_batch(V1_MINIMAL_SCHEMA_SQL).unwrap();
        }
        let migrated_store = HistoryStore::open(&path).unwrap();
        let migrated_shape = schema_shape(&migrated_store.connection().unwrap());

        assert_eq!(
            fresh_shape, migrated_shape,
            "the fresh-database schema and the fully-migrated-from-v1 schema must define the \
             same tables, columns, constraints, foreign keys, and indexes; a drift here means \
             the two paths built separately in this file have diverged"
        );
    }

    /// Minimal genuine schema-v1 database, matching a real pre-#107/#42/#41
    /// store. Used by `fresh_and_fully_migrated_databases_produce_identical_schema_shapes`.
    ///
    /// Matches the *original* `version == 0` shape from this file's first
    /// commit (`9e74026`, "Add durable session history and time-aware
    /// pricing") byte-for-byte in every column, foreign key, and index that
    /// commit already had — including the `durable_sessions_identity_idx`
    /// and `durable_token_events_session_timestamp_idx` indexes and the
    /// `REFERENCES durable_sessions(session_key)` foreign keys on
    /// `durable_token_events` and `session_snapshots` — so this genuinely
    /// models a real legacy database rather than the narrower, hand-typed v1
    /// stub used elsewhere in this file for column-presence-only checks
    /// (`migrates_schema_v1_request_evidence_forward`), which was never
    /// meant to be a byte-for-byte proxy for a real v1 schema and would
    /// otherwise make `fresh_and_fully_migrated_databases_produce_identical_schema_shapes`
    /// fail for a reason that isn't a real migration bug.
    const V1_MINIMAL_SCHEMA_SQL: &str = "CREATE TABLE history_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO history_meta(key, value) VALUES('schema_version', '1');
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
           cumulative_total_tokens INTEGER NOT NULL,
           input_tokens INTEGER NOT NULL,
           cached_input_tokens INTEGER NOT NULL,
           output_tokens INTEGER NOT NULL,
           reasoning_output_tokens INTEGER NOT NULL,
           total_tokens INTEGER NOT NULL,
           PRIMARY KEY(session_key, event_key)
         );
         CREATE INDEX durable_token_events_session_timestamp_idx ON durable_token_events(session_key, timestamp_ms);
         PRAGMA user_version = 1;";

    /// Rewinds a freshly-created (head-schema) database down to exactly
    /// `target_version` by undoing each migration step above it in reverse —
    /// the literal inverse of the real `migrate()` SQL, so the resulting
    /// stub tracks the actual legacy shapes instead of a hand-maintained
    /// parallel fixture that could silently drift from them.
    fn rewind_database_to_version(path: &Path, target_version: i64) {
        let connection = Connection::open(path).unwrap();
        let mut version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            version, SCHEMA_VERSION,
            "rewind_database_to_version expects a head-schema database to start from"
        );
        while version > target_version {
            let sql = match version {
                6 => {
                    "DROP INDEX IF EXISTS durable_sessions_project_idx;
                     ALTER TABLE durable_sessions DROP COLUMN project_key;
                     ALTER TABLE durable_sessions DROP COLUMN project_label;
                     ALTER TABLE durable_sessions DROP COLUMN project_provenance;
                     ALTER TABLE durable_sessions DROP COLUMN project_source_directory;
                     DROP TABLE project_overrides;
                     DROP TABLE project_session_overrides;"
                }
                5 => {
                    "ALTER TABLE durable_token_events DROP COLUMN cache_creation_input_tokens;
                     ALTER TABLE rollup_token_totals DROP COLUMN cache_creation_input_tokens;"
                }
                4 => {
                    "DROP TABLE rollup_token_totals;
                     DROP TABLE rollup_tool_metrics;
                     DROP TABLE rollup_mutation_chains;"
                }
                3 => {
                    "DROP TABLE durable_tool_events;
                     DROP TABLE durable_finding_events;
                     ALTER TABLE durable_sessions DROP COLUMN ledger_dirty;"
                }
                2 => "ALTER TABLE durable_token_events DROP COLUMN request_input_tokens;",
                other => unreachable!("no rewind step defined for version {other}"),
            };
            connection.execute_batch(sql).unwrap();
            version -= 1;
            connection
                .execute_batch(&format!(
                    "INSERT INTO history_meta(key, value) VALUES('schema_version', '{version}')
                       ON CONFLICT(key) DO UPDATE SET value = '{version}';
                     PRAGMA user_version = {version};"
                ))
                .unwrap();
        }
    }

    #[test]
    fn migration_from_every_prior_version_reaches_schema_version_with_no_gap_or_duplicate() {
        // Invariant 2: proves migrate()'s version-by-version `if` chain has
        // no gap (every version from 1 to SCHEMA_VERSION - 1 makes forward
        // progress all the way to head) and no duplicated step (history_meta
        // ends with exactly one schema_version row, matching PRAGMA
        // user_version).
        for target_version in 1..SCHEMA_VERSION {
            let directory = tempdir().unwrap();
            let path = directory.path().join("history.sqlite3");
            {
                let _ = HistoryStore::open(&path).unwrap();
            }
            rewind_database_to_version(&path, target_version);

            let store = HistoryStore::open(&path).unwrap();
            let connection = store.connection().unwrap();
            let version: i64 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                version, SCHEMA_VERSION,
                "migrating from v{target_version} did not reach SCHEMA_VERSION (a gap in the \
                 migration chain)"
            );
            let schema_version_rows: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM history_meta WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                schema_version_rows, 1,
                "migrating from v{target_version} left {schema_version_rows} 'schema_version' \
                 rows in history_meta (expected exactly 1 — a duplicated migration step)"
            );
            let recorded_version: String = connection
                .query_row(
                    "SELECT value FROM history_meta WHERE key = 'schema_version'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(recorded_version, SCHEMA_VERSION.to_string());
        }
    }

    #[test]
    fn migration_step_count_matches_steps_migrate_actually_runs() {
        // `migration_step_count` mirrors `migrate()`'s own `if` gates purely
        // to size a progress callback's `step_total` before the first step
        // starts (#116, see its doc comment); this proves the two agree by
        // counting real "step started" events against it for every version
        // migrate() actually chains from, rather than trusting the mirrored
        // logic never drifts silently.
        let fresh_directory = tempdir().unwrap();
        let fresh_path = fresh_directory.path().join("history.sqlite3");
        let mut fresh_starts = 0u32;
        let mut fresh_totals = HashSet::new();
        let _ = HistoryStore::open_with_progress(&fresh_path, |event| {
            if event.elapsed_ms.is_none() && event.items_done.is_none() {
                fresh_starts += 1;
                fresh_totals.insert(event.step_total);
            }
        })
        .unwrap();
        assert_eq!(fresh_starts, migration_step_count(0));
        assert_eq!(fresh_totals.into_iter().collect::<Vec<_>>(), vec![1]);

        for target_version in 1..SCHEMA_VERSION {
            let directory = tempdir().unwrap();
            let path = directory.path().join("history.sqlite3");
            {
                let _ = HistoryStore::open(&path).unwrap();
            }
            rewind_database_to_version(&path, target_version);

            let mut started_steps = 0u32;
            let mut reported_totals = HashSet::new();
            let _ = HistoryStore::open_with_progress(&path, |event| {
                if event.elapsed_ms.is_none() && event.items_done.is_none() {
                    started_steps += 1;
                    reported_totals.insert(event.step_total);
                }
            })
            .unwrap();

            assert_eq!(
                started_steps,
                migration_step_count(target_version),
                "migrating from v{target_version}"
            );
            assert_eq!(
                reported_totals.into_iter().collect::<Vec<_>>(),
                vec![migration_step_count(target_version)],
                "step_total must be reported consistently for v{target_version}"
            );
        }
    }

    #[test]
    fn interrupting_migration_between_steps_resumes_cleanly_at_the_right_version() {
        // Each migration step commits its own transaction before the next
        // one starts (AGENTS.md), so a process kill between steps must never
        // re-run a completed step or leave `PRAGMA user_version` disagreeing
        // with `history_meta`. This proves it without an actual process
        // kill: rewind a fully-migrated database to v3, then unwind out of
        // `open_with_progress` immediately after its first step's
        // transaction has committed (simulating the app dying at that exact
        // instant — nothing about SQLite's durability depends on the Rust
        // call stack unwinding cleanly afterward), and confirm a fresh open
        // (as a relaunched app would perform) resumes at v4 and reaches
        // `SCHEMA_VERSION` with the same data an uninterrupted migration
        // would have produced.
        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        const SESSIONS: usize = 5;
        {
            let store = HistoryStore::open(&path).unwrap();
            let generation = store.begin_scan().unwrap().max(1);
            for index in 0..SESSIONS {
                let fixture = session(&format!("interrupt-{index}"), 100 + index as u64);
                store
                    .observe(
                        Path::new(&format!("interrupt-{index}.jsonl")),
                        &fixture,
                        generation,
                    )
                    .unwrap();
            }
        }
        rewind_database_to_version(&path, 3);

        let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            HistoryStore::open_with_progress(&path, |event| {
                if event.step == "v3_to_v4_rollup_backfill" && event.elapsed_ms.is_some() {
                    panic!("simulated interruption immediately after v3->v4 commits");
                }
            })
        }));
        assert!(
            interrupted.is_err(),
            "expected the simulated interruption to unwind"
        );

        // The committed v3->v4 step must have survived the interruption on
        // its own — nothing runs after the panic to persist it.
        {
            let connection = Connection::open(&path).unwrap();
            let version: i64 = connection
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                version, 4,
                "the committed v3->v4 step should have persisted despite the interruption"
            );
        }

        let mut resumed_steps = Vec::new();
        let store = HistoryStore::open_with_progress(&path, |event| {
            if event.elapsed_ms.is_some() {
                resumed_steps.push(event.step);
            }
        })
        .unwrap();
        assert_eq!(
            resumed_steps,
            vec![
                "v4_to_v5_cache_creation_backfill",
                "v5_to_v6_project_identity_backfill",
            ],
            "resuming must run exactly the remaining steps, never re-running v3->v4"
        );

        let connection = store.connection().unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let schema_version_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM history_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            schema_version_rows, 1,
            "resuming must not duplicate the schema_version row"
        );
        // A re-run of the v3->v4 backfill would violate
        // rollup_token_totals_key_idx on its second INSERT (no ON CONFLICT
        // clause) and this open would already have failed above; this
        // additionally proves the row counts are exactly what one pass over
        // the fixture data produces, not silently doubled some other way.
        let token_event_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM durable_token_events", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(token_event_rows, SESSIONS as i64);
        let rollup_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM rollup_token_totals", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rollup_rows, SESSIONS as i64);
        drop(connection);

        let loaded = store.load_sessions().unwrap();
        assert_eq!(loaded.len(), SESSIONS);
    }

    #[test]
    #[ignore = "performance probe; run with --release --ignored --nocapture"]
    fn performance_v3_to_v6_chained_migration_large_ledger() {
        // Issue #116: the chained v3->v6 migration runs as one blocking pass
        // before any window exists, and its ~10-20s estimate was inferred
        // from #107's isolated per-step benchmarks, never measured end to
        // end on one corpus. This builds one large enough for the chain's
        // real, combined cost to show up: a handful of "hot" sessions
        // carrying a million-plus token/tool events each (driving the
        // v3->v4 rollup GROUP BY and the v4->v5 cache-creation
        // UPDATE...FROM, both full-table aggregate passes) plus thousands of
        // "cold" sessions across many distinct, deliberately non-existent
        // working directories (driving the v5->v6 project-identity
        // backfill, isolated from real Git-discovery cost exactly like the
        // #41 precedent probe above it in this file).
        const HOT_SESSIONS: usize = 20;
        const TOKEN_EVENTS: usize = 1_000_000;
        const TOOL_EVENTS: usize = 100_000;
        const COLD_SESSIONS: usize = 4_000;
        const DIRECTORIES: usize = 120;

        let directory = tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        let hot_keys: Vec<String> = {
            let store = HistoryStore::open(&path).unwrap();
            let generation = store.begin_scan().unwrap().max(1);
            let mut keys = Vec::with_capacity(HOT_SESSIONS);
            for index in 0..HOT_SESSIONS {
                let fixture = session(&format!("hot-{index}"), 100);
                let stored = store
                    .observe(
                        Path::new(&format!("hot-{index}.jsonl")),
                        &fixture,
                        generation,
                    )
                    .unwrap();
                keys.push(stored.key);
            }
            for index in 0..COLD_SESSIONS {
                let mut fixture = session(&format!("cold-{index}"), 10);
                fixture.working_directory = Some(format!(
                    "/synthetic/does-not-exist/project-{}",
                    index % DIRECTORIES
                ));
                store
                    .observe(
                        Path::new(&format!("cold-{index}.jsonl")),
                        &fixture,
                        generation,
                    )
                    .unwrap();
            }
            keys
        };
        {
            // Bulk fixture setup, not the migration itself: a plain
            // prepared-statement loop (rather than SQL-side generation) is
            // fine here even at this row count, unlike inside the real
            // backfill, which AGENTS.md requires to stay SQL-side.
            let mut connection = Connection::open(&path).unwrap();
            let transaction = connection.transaction().unwrap();
            {
                let mut insert = transaction
                    .prepare(
                        "INSERT INTO durable_token_events(
                           session_key, event_key, event_index, timestamp_ms, model, service_tier,
                           request_input_tokens, cumulative_total_tokens, input_tokens,
                           cached_input_tokens, output_tokens, reasoning_output_tokens,
                           total_tokens, cache_creation_input_tokens)
                         VALUES (?1, ?2, ?3, ?4, 'gpt-test', NULL, 100, 1000, 100, 10, 50, 5, 150, 2)",
                    )
                    .unwrap();
                for n in 0..TOKEN_EVENTS {
                    insert
                        .execute(params![
                            hot_keys[n % HOT_SESSIONS],
                            format!("bulk-{n}"),
                            n as i64,
                            1_735_689_600_000i64 + n as i64 * 60_000,
                        ])
                        .unwrap();
                }
            }
            {
                let mut insert = transaction
                    .prepare(
                        "INSERT INTO durable_tool_events(
                           session_key, timestamp_ms, model, kind, outcome, turn_id, target,
                           duration_ms, output_bytes)
                         VALUES (?1, ?2, 'gpt-test', ?3, ?4, ?5, ?6, 120, 256)",
                    )
                    .unwrap();
                const KINDS: [&str; 4] = ["read", "search", "mutation", "command"];
                const OUTCOMES: [&str; 3] = ["success", "failure", "unknown"];
                for n in 0..TOOL_EVENTS {
                    insert
                        .execute(params![
                            hot_keys[n % HOT_SESSIONS],
                            1_735_689_600_000i64 + n as i64 * 60_000,
                            KINDS[n % KINDS.len()],
                            OUTCOMES[n % OUTCOMES.len()],
                            format!("turn-{}", n % 500),
                            format!("target-{}", n % 50),
                        ])
                        .unwrap();
                }
            }
            transaction.commit().unwrap();
        }
        rewind_database_to_version(&path, 3);

        let mut step_timings: Vec<(String, u64)> = Vec::new();
        let started = Instant::now();
        let store = HistoryStore::open_with_progress(&path, |event| {
            if let Some(elapsed_ms) = event.elapsed_ms {
                step_timings.push((event.step.to_string(), elapsed_ms));
            }
        })
        .unwrap();
        let total = started.elapsed();

        eprintln!(
            "v3->v6 chained migration over {TOKEN_EVENTS} token events / {TOOL_EVENTS} tool \
             events across {HOT_SESSIONS} sessions, plus {COLD_SESSIONS} sessions across \
             {DIRECTORIES} distinct (non-existent, no Git discovery) directories: {total:?} total"
        );
        for (step, elapsed_ms) in &step_timings {
            eprintln!("  {step}: {elapsed_ms}ms");
        }

        let connection = store.connection().unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let rollup_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM rollup_token_totals", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(
            rollup_rows > 0,
            "v3->v4 backfill should have populated rollups"
        );
        let projects: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM durable_sessions WHERE project_key IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(projects, COLD_SESSIONS as i64);
    }

    /// Builds a `Session` whose every `TokenTotals` field and every
    /// `ToolMetrics` counter is non-zero and pairwise distinct within its
    /// struct, spread across three consecutive hour buckets on
    /// 2026-01-01 (00:xx, 01:xx, 02:xx).
    fn every_dimension_session(id: &str) -> Session {
        use crate::model::ToolKind;
        let mut fixture = session(id, 1);

        // Token dimension: one event per bucket. Field values (grouped by
        // dimension across all three events) are pairwise distinct: input=93,
        // cached=103, cache_creation=115, output=127, reasoning=137,
        // total=221.
        let token_event = |at: &str,
                           input: u64,
                           cached: u64,
                           cache_creation: u64,
                           output: u64,
                           reasoning: u64,
                           total: u64| TokenHistoryPoint {
            timestamp: timestamp(at),
            model: Some("gpt-dimension".into()),
            service_tier: None,
            request_input_tokens: Some(input),
            total_tokens: total,
            delta: TokenTotals {
                input_tokens: input,
                cached_input_tokens: cached,
                cache_creation_input_tokens: cache_creation,
                output_tokens: output,
                reasoning_output_tokens: reasoning,
                total_tokens: total,
            },
        };
        fixture.tokens_history = vec![
            token_event("2026-01-01T00:30:00Z", 11, 13, 17, 19, 23, 101),
            token_event("2026-01-01T01:15:00Z", 29, 31, 37, 41, 43, 47),
            token_event("2026-01-01T02:45:00Z", 53, 59, 61, 67, 71, 73),
        ];
        let mut total = TokenTotals::default();
        for point in &fixture.tokens_history {
            total += &point.delta;
        }
        fixture.tokens_total = total;

        // Tool dimension: 30 observations across the same three buckets.
        // Kind counts: reads=4, searches=6, mutations=5, commands=7, other=8
        // (calls=30). The 5 mutations are two (turn, target) chains: a
        // one-shot chain (turn-a/target-a, bucket 1) and a four-call retry
        // chain (turn-b/target-b) that straddles an edge bucket, the
        // rollup-served bucket (twice), and the other edge bucket — giving
        // mutation_targets=2, one_shot_mutations=1, retry_count=3. Outcome
        // counts: successes=9, failures=10, unknown=11. duration_ms and
        // output_bytes are concentrated on one call so their totals (555,
        // 9999) stay distinct from every other counter.
        struct Call {
            at: String,
            kind: ToolKind,
            turn: Option<&'static str>,
            target: Option<&'static str>,
        }
        let mut calls = vec![
            Call {
                at: "2026-01-01T01:05:00Z".into(),
                kind: ToolKind::Mutation,
                turn: Some("turn-a"),
                target: Some("target-a"),
            },
            Call {
                at: "2026-01-01T00:20:00Z".into(),
                kind: ToolKind::Mutation,
                turn: Some("turn-b"),
                target: Some("target-b"),
            },
            Call {
                at: "2026-01-01T01:10:00Z".into(),
                kind: ToolKind::Mutation,
                turn: Some("turn-b"),
                target: Some("target-b"),
            },
            Call {
                at: "2026-01-01T01:40:00Z".into(),
                kind: ToolKind::Mutation,
                turn: Some("turn-b"),
                target: Some("target-b"),
            },
            Call {
                at: "2026-01-01T02:10:00Z".into(),
                kind: ToolKind::Mutation,
                turn: Some("turn-b"),
                target: Some("target-b"),
            },
        ];
        let hours = ["00", "01", "02"];
        let mut minute = [16usize, 16usize, 16usize];
        let plain_kinds = [
            ToolKind::Read,
            ToolKind::Search,
            ToolKind::Command,
            ToolKind::Other,
        ];
        let plain_counts = [4usize, 6, 7, 8];
        let mut plain_generated = 0usize;
        for (kind, count) in plain_kinds.into_iter().zip(plain_counts) {
            for _ in 0..count {
                let bucket = plain_generated % 3;
                let at = format!("2026-01-01T{}:{:02}:00Z", hours[bucket], minute[bucket]);
                minute[bucket] += 1;
                plain_generated += 1;
                calls.push(Call {
                    at,
                    kind,
                    turn: None,
                    target: None,
                });
            }
        }
        assert_eq!(
            calls.len(),
            30,
            "fixture sanity check: reads+searches+mutations+commands+other"
        );

        let outcomes: Vec<ToolOutcome> = std::iter::repeat_n(ToolOutcome::Success, 9)
            .chain(std::iter::repeat_n(ToolOutcome::Failure, 10))
            .chain(std::iter::repeat_n(ToolOutcome::Unknown, 11))
            .collect();
        assert_eq!(outcomes.len(), 30);

        fixture.tool_observations = calls
            .into_iter()
            .zip(outcomes)
            .enumerate()
            .map(|(index, (call, outcome))| ToolObservation {
                call_id: format!("dimension-call-{index}"),
                turn_id: call.turn.map(Into::into),
                harness: codex_provider_id(),
                model: Some("gpt-dimension".into()),
                timestamp: timestamp(&call.at),
                kind: call.kind,
                name: "tool".into(),
                providers: Vec::new(),
                effective_tools: Vec::new(),
                target: call.target.map(Into::into),
                resource_id: None,
                outcome,
                duration_ms: Some(if index == 0 { 555 } else { 0 }),
                output_bytes: if index == 0 { 9999 } else { 0 },
            })
            .collect();

        fixture.last_event_at = timestamp("2026-01-01T02:45:00Z");
        fixture
    }

    /// Every field of the serialized value must be a non-negative integer
    /// that is both non-zero and pairwise distinct from every other field —
    /// reflection-based (`serde_json`), not a hand-maintained field list, so
    /// a newly added dimension is included automatically without anyone
    /// remembering to update this test.
    fn assert_all_fields_nonzero_and_distinct(value: &serde_json::Value, label: &str) {
        let object = value
            .as_object()
            .unwrap_or_else(|| panic!("{label} did not serialize to a JSON object"));
        let mut seen = HashSet::new();
        for (field, field_value) in object {
            let n = field_value
                .as_u64()
                .unwrap_or_else(|| panic!("{label}.{field} is not a plain non-negative integer"));
            assert_ne!(
                n, 0,
                "{label}.{field} is zero in the golden fixture; a field that silently defaults \
                 to zero must fail this guard rather than pass vacuously"
            );
            assert!(
                seen.insert(n),
                "{label}.{field} = {n} collides with another field's value in this fixture; \
                 dimensions must be pairwise distinct or a field swap would go undetected"
            );
        }
    }

    #[test]
    fn ledger_range_totals_match_oracle_for_every_nonzero_distinct_dimension() {
        // Invariant 3: strengthens the golden ledger-vs-oracle comparison so
        // a newly added dimension is exercised without anyone extending a
        // fixture by hand. `every_dimension_session` populates every
        // TokenTotals field and every ToolMetrics counter with a distinct,
        // non-zero value, and the window below straddles buckets so the
        // rollup-served path and the sub-hour edge path are both exercised
        // in the same call.
        let (_directory, store) = store();
        let fixture = every_dimension_session("every-dimension");
        let generation = store.begin_scan().unwrap().max(1);
        let key = store
            .observe(Path::new("every-dimension.jsonl"), &fixture, generation)
            .unwrap()
            .key;

        let windows: Vec<RangeWindow> = vec![(
            Some(timestamp("2026-01-01T00:15:00Z")),
            Some(timestamp("2026-01-01T02:50:00Z")),
        )];
        let expected = &fixture.range_totals_multi(&windows)[0];

        assert_all_fields_nonzero_and_distinct(
            &serde_json::to_value(&expected.tokens).unwrap(),
            "tokens",
        );
        assert_all_fields_nonzero_and_distinct(
            &serde_json::to_value(&expected.tool_metrics).unwrap(),
            "tool_metrics",
        );

        let from_ledger = store
            .range_totals_multi(std::slice::from_ref(&key), &windows)
            .unwrap();
        let actual = from_ledger[0]
            .get(&key)
            .expect("window has data for this session");
        assert_eq!(&actual.tokens, &expected.tokens, "tokens");
        assert_eq!(&actual.tool_metrics, &expected.tool_metrics, "tool_metrics");
    }

    #[test]
    fn backfill_project_identity_selects_keys_before_any_snapshot_blob() {
        // Invariant 4: structural proxy for the migration backfill's memory
        // shape. A true peak-RSS measurement is impractical in a unit test,
        // so this instead asserts the query shape `backfill_project_identities`
        // actually issues: its first pass (`BACKFILL_PROJECT_IDENTITY_KEYS_SQL`)
        // selects only session keys, never a snapshot blob, and its
        // per-session fetch (`BACKFILL_PROJECT_IDENTITY_SNAPSHOT_SQL`) is
        // scoped to exactly one session at a time. A ledger can hold
        // thousands of sessions with multi-megabyte snapshots each
        // (AGENTS.md); a query that returned every `session_json` at once
        // would exhaust memory well before that, the same class of defect
        // the v1->v2 fact backfill earlier in this file was already written
        // to avoid.
        assert!(
            !BACKFILL_PROJECT_IDENTITY_KEYS_SQL
                .to_lowercase()
                .contains("session_json"),
            "the first-pass query must select only session keys, not snapshot blobs: {}",
            BACKFILL_PROJECT_IDENTITY_KEYS_SQL
        );
        assert!(
            BACKFILL_PROJECT_IDENTITY_SNAPSHOT_SQL.contains("WHERE d.session_key = ?1"),
            "the snapshot fetch must be scoped to exactly one session per query: {}",
            BACKFILL_PROJECT_IDENTITY_SNAPSHOT_SQL
        );
        assert!(
            !BACKFILL_PROJECT_IDENTITY_SNAPSHOT_SQL
                .to_lowercase()
                .contains(" in ("),
            "the snapshot fetch must not batch multiple sessions' blobs into one query: {}",
            BACKFILL_PROJECT_IDENTITY_SNAPSHOT_SQL
        );
    }
}
