//! Retention metadata and bounded durable projections. Source files are never
//! removed by this module. Purged identities retain only exclusion tombstones.
use super::*;
use crate::model::{SessionLifecycle, SessionSummary};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub(super) fn install_schema(transaction: &Transaction<'_>) -> Result<()> {
    let exists: bool = transaction.query_row(
        "SELECT COUNT(*) > 0 FROM pragma_table_info('durable_sessions') WHERE name = 'lifecycle'",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        transaction.execute_batch("ALTER TABLE durable_sessions ADD COLUMN lifecycle TEXT NOT NULL DEFAULT 'present' CHECK(lifecycle IN ('present','retained','superseded')); ")?;
    }
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS session_summaries (
           session_key TEXT PRIMARY KEY REFERENCES durable_sessions(session_key),
           summary_json BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS purged_sessions (
           session_key TEXT NOT NULL,
           identity_key TEXT NOT NULL,
           first_event_fingerprint TEXT NOT NULL,
           purged_at_ms INTEGER NOT NULL,
           PRIMARY KEY(session_key,first_event_fingerprint)
         );
         CREATE INDEX IF NOT EXISTS purged_sessions_identity_idx
           ON purged_sessions(identity_key, first_event_fingerprint);
         INSERT OR IGNORE INTO history_meta(key,value) VALUES('coverage_complete','1');
         UPDATE durable_sessions SET lifecycle = 'retained'
           WHERE lifecycle = 'present' AND NOT EXISTS(
             SELECT 1 FROM source_locations l WHERE l.session_key = durable_sessions.session_key AND l.present = 1);",
    )?;
    // The backfill stays inside SQLite, including bucket and finding reduction.
    // Never deserialize full transcripts in Rust during a multi-GB migration.
    transaction.execute_batch(SUMMARY_BACKFILL_SQL)?;
    Ok(())
}

const SUMMARY_BACKFILL_SQL: &str = r#"
INSERT OR IGNORE INTO session_summaries(session_key,summary_json)
SELECT d.session_key, CAST(json_set(
  json_remove(CAST(s.session_json AS TEXT), '$.turns', '$.tokens_history', '$.tokens_by_model',
    '$.tool_observations', '$.optimization_findings', '$.rate_limits_history'),
  '$.storage_id', d.session_key,
  '$.first_user_message', substr(json_extract(s.session_json,'$.first_user_message'),1,1024),
  '$.thread_name', substr(json_extract(s.session_json,'$.thread_name'),1,512),
  '$.buckets', json(CASE WHEN coalesce(json_array_length(s.session_json,'$.tokens_history'),0) = 0
    THEN coalesce((SELECT json_group_array(json_object('model',m.key,'service_tier',NULL,'tokens',json(m.value)))
      FROM json_each(s.session_json,'$.tokens_by_model') m ORDER BY m.key),'[]')
    ELSE coalesce((SELECT json_group_array(json_object('model',b.model,'service_tier',b.tier,
      'tokens',json_object('input_tokens',b.input,'cached_input_tokens',b.cached,
        'cache_creation_input_tokens',b.creation,'output_tokens',b.output,
        'reasoning_output_tokens',b.reasoning,'total_tokens',b.total)))
      FROM (SELECT json_extract(e.value,'$.model') model, json_extract(e.value,'$.service_tier') tier,
        sum(coalesce(json_extract(e.value,'$.delta.input_tokens'),0)) input,
        sum(coalesce(json_extract(e.value,'$.delta.cached_input_tokens'),0)) cached,
        sum(coalesce(json_extract(e.value,'$.delta.cache_creation_input_tokens'),0)) creation,
        sum(coalesce(json_extract(e.value,'$.delta.output_tokens'),0)) output,
        sum(coalesce(json_extract(e.value,'$.delta.reasoning_output_tokens'),0)) reasoning,
        sum(coalesce(json_extract(e.value,'$.delta.total_tokens'),0)) total
      FROM json_each(s.session_json,'$.tokens_history') e WHERE json_extract(e.value,'$.model') IS NOT NULL
      GROUP BY model,tier ORDER BY model,tier) b),'[]') END),
  '$.optimization_findings_count', coalesce(json_array_length(s.session_json,'$.optimization_findings'),0),
  '$.optimization_summary', json_object(
    'findings',coalesce(json_array_length(s.session_json,'$.optimization_findings'),0),
    'warnings',(SELECT count(*) FROM json_each(s.session_json,'$.optimization_findings') f
      WHERE json_extract(f.value,'$.severity') = 'warning'),
    'likely_avoidable_calls',coalesce((SELECT sum(json_extract(f.value,'$.avoidable_calls'))
      FROM json_each(s.session_json,'$.optimization_findings') f),0),
    'by_rule',json(coalesce((SELECT json_group_object(rule,n) FROM (
      SELECT json_extract(f.value,'$.rule_id') rule,count(*) n
      FROM json_each(s.session_json,'$.optimization_findings') f GROUP BY rule)),'{}')))
) AS BLOB)
FROM durable_sessions d JOIN session_snapshots s
  ON s.session_key=d.session_key AND s.version=d.current_snapshot_version;
"#;

pub(super) fn store_summary(
    transaction: &Transaction<'_>,
    key: &str,
    session: &Session,
) -> Result<()> {
    let mut summary = SessionSummary::of(session);
    truncate_preview(&mut summary.first_user_message, 1024);
    truncate_preview(&mut summary.thread_name, 512);
    transaction.execute(
        "INSERT INTO session_summaries(session_key,summary_json) VALUES(?1,?2)
         ON CONFLICT(session_key) DO UPDATE SET summary_json=excluded.summary_json",
        params![key, serde_json::to_vec(&summary)?],
    )?;
    Ok(())
}

fn truncate_preview(value: &mut Option<String>, limit: usize) {
    if let Some(text) = value {
        if let Some((index, _)) = text.char_indices().nth(limit) {
            text.truncate(index);
        }
    }
}

pub(super) fn decode_lifecycle(value: &str) -> Result<SessionLifecycle> {
    match value {
        "present" => Ok(SessionLifecycle::Present),
        "retained" => Ok(SessionLifecycle::Retained),
        "superseded" => Ok(SessionLifecycle::Superseded),
        _ => bail!("invalid durable session lifecycle"),
    }
}

pub(super) fn refresh_missing(connection: &Connection) -> Result<()> {
    connection.execute(
        "UPDATE durable_sessions SET lifecycle='retained' WHERE lifecycle='present'
         AND NOT EXISTS(SELECT 1 FROM source_locations l WHERE l.session_key=durable_sessions.session_key AND l.present=1)", [],
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Keep all by default. A duration selects purge candidates, never automatic deletion.
    pub retained_days: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionStatus {
    pub policy: RetentionPolicy,
    pub present_sessions: u64,
    pub retained_sessions: u64,
    pub superseded_sessions: u64,
    pub purged_sessions: u64,
    pub coverage_complete: bool,
    pub recovered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PurgePreview {
    pub cutoff_utc_day: String,
    pub sessions: u64,
    pub identity_groups: u64,
    pub snapshot_bytes: u64,
    pub tokens: TokenTotals,
    /// Optimistic revision of the exact candidate set, not a credential.
    pub revision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PurgeResult {
    pub removed_keys: Vec<String>,
    pub purged_at: DateTime<Utc>,
}

#[derive(Debug)]
pub struct PurgedSource;
impl std::fmt::Display for PurgedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("source history was explicitly purged and is excluded from reimport")
    }
}
impl std::error::Error for PurgedSource {}

pub(super) fn is_excluded(connection: &Connection, session: &Session) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM purged_sessions WHERE (identity_key=?1 OR session_key=?1) AND first_event_fingerprint=?2)",
        params![provider_identity(session)?,first_event_fingerprint(session)], |r|r.get(0),
    )?)
}

#[derive(Serialize, Deserialize)]
struct ExclusionRecord {
    version: u32,
    purged_at_ms: i64,
    identities: ExclusionIdentities,
}
type ExclusionIdentities = Vec<(String, String, String)>;

fn exclusion_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".exclusions.jsonl");
    PathBuf::from(name)
}

/// Write the already-confirmed erasure intent before committing removal. A
/// failed SQL commit leaves existing history readable, but a later corrupt
/// recovery conservatively excludes the confirmed identities. The independent
/// record contains no source path, prompt, title, or usage.
pub(super) fn write_exclusions(
    path: &Path,
    identities: &[(String, String, String)],
    at: i64,
) -> Result<()> {
    if identities.is_empty() {
        return Ok(());
    }
    read_exclusions(path, |_| Ok(()))?;
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(exclusion_path(path))?;
    let length = file.metadata()?.len();
    if length > 256 * 1024 * 1024 {
        bail!("purge exclusion journal exceeds its safe size limit");
    }
    // A crash before the prior record was complete cannot have committed its
    // SQL purge. Remove only that incomplete tail before appending a new intent.
    let mut complete = length;
    let mut tail = vec![0; 32 * 1024];
    while complete > 0 {
        let begin = complete.saturating_sub(tail.len() as u64);
        let chunk = &mut tail[..usize::try_from(complete - begin)?];
        file.seek(SeekFrom::Start(begin))?;
        file.read_exact(chunk)?;
        if let Some(index) = chunk.iter().rposition(|byte| *byte == b'\n') {
            complete = begin + index as u64 + 1;
            break;
        }
        complete = begin;
        if length - complete > 32 * 1024 * 1024 {
            bail!("incomplete purge exclusion tail exceeds its safe size limit");
        }
    }
    file.set_len(complete)?;
    file.seek(SeekFrom::End(0))?;
    let record = ExclusionRecord {
        version: 1,
        purged_at_ms: at,
        identities: identities.to_vec(),
    };
    let raw = serde_json::to_vec(&record)?;
    if raw.len() >= 32 * 1024 * 1024 {
        bail!("purge exclusion batch exceeds its safe size limit");
    }
    if complete + raw.len() as u64 + 1 > 256 * 1024 * 1024 {
        bail!("purge exclusion journal would exceed its safe size limit");
    }
    file.write_all(&raw)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn read_exclusions(
    path: &Path,
    mut apply: impl FnMut(ExclusionRecord) -> Result<()>,
) -> Result<()> {
    use std::io::BufRead;
    let file = match std::fs::File::open(exclusion_path(path)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if file.metadata()?.len() > 256 * 1024 * 1024 {
        bail!("purge exclusion journal exceeds its safe size limit");
    }
    let mut reader = std::io::BufReader::new(file);
    let mut raw = Vec::new();
    let mut identities = 0usize;
    loop {
        raw.clear();
        // Limit the allocation before reading an untrusted/corrupt record.
        let length =
            std::io::Read::take(&mut reader, 32 * 1024 * 1024 + 1).read_until(b'\n', &mut raw)?;
        if length == 0 {
            break;
        }
        if raw.len() > 32 * 1024 * 1024 {
            bail!("purge exclusion record exceeds its safe size limit");
        }
        if raw.last() != Some(&b'\n') {
            break;
        }
        let record: ExclusionRecord = serde_json::from_slice(&raw).context(
            "purge exclusions could not be verified; recovery cannot safely reimport sources",
        )?;
        if record.version != 1 {
            bail!("unsupported purge exclusion version; update Odometer before recovery");
        }
        identities += record.identities.len();
        if identities > 500_000 {
            bail!("purge exclusion identity limit exceeded");
        }
        apply(record)?;
    }
    Ok(())
}

pub(super) fn restore_exclusions(connection: &mut Connection, path: &Path) -> Result<()> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    read_exclusions(path, |record| {
        for (key, identity, fingerprint) in record.identities {
            // If SQL rolled back, the existing row is still retained. On an
            // empty recovery replacement every confirmed identity is excluded.
            transaction.execute("INSERT OR IGNORE INTO purged_sessions(session_key,identity_key,first_event_fingerprint,purged_at_ms)
              SELECT ?1,?2,?3,?4 WHERE NOT EXISTS(SELECT 1 FROM durable_sessions WHERE session_key=?1)",params![key,identity,fingerprint,record.purged_at_ms])?;
        }
        Ok(())
    })?;
    transaction.commit()?;
    Ok(())
}

/// Bounded minimal-identity cache, kept even when the main ledger cannot open.
/// Invalid or unexpectedly removed journals cannot be treated as empty.
#[derive(Default)]
pub struct ExclusionCache {
    path: Option<PathBuf>,
    stamp: Option<(u64, std::time::SystemTime)>,
    initialized: bool,
    invalid: bool,
    identities: std::collections::HashSet<(String, String)>,
}

impl ExclusionCache {
    pub fn unavailable() -> Self {
        Self {
            invalid: true,
            ..Self::default()
        }
    }
    pub fn at(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            ..Self::default()
        }
    }
    pub fn bind(&mut self, path: PathBuf) {
        if self.path.as_ref() != Some(&path) {
            *self = Self::at(path);
        }
    }
    pub fn is_excluded(&mut self, session: &Session) -> Result<bool> {
        let Some(path) = &self.path else {
            if self.invalid {
                bail!("independent purge exclusions are unavailable");
            }
            return Ok(false);
        };
        let stamp = match std::fs::metadata(exclusion_path(path)) {
            Ok(metadata) => Some((metadata.len(), metadata.modified()?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if !self.initialized || stamp != self.stamp {
            if self.initialized && self.stamp.is_some() && stamp.is_none() {
                self.invalid = true;
            } else {
                let mut loaded = std::collections::HashSet::new();
                let result = read_exclusions(path, |record| {
                    for (key, identity, fingerprint) in record.identities {
                        loaded.insert((key, fingerprint.clone()));
                        loaded.insert((identity, fingerprint));
                    }
                    Ok(())
                });
                self.invalid = result.is_err();
                if result.is_ok() {
                    let additional = loaded
                        .iter()
                        .filter(|identity| !self.identities.contains(*identity))
                        .count();
                    if self.identities.len() + additional > 1_000_000 {
                        self.invalid = true;
                    } else {
                        self.identities.extend(loaded);
                    }
                }
            }
            self.initialized = true;
            self.stamp = stamp;
        }
        if self.invalid {
            bail!("independent purge exclusions could not be verified; source history cannot be republished");
        }
        Ok(self.identities.contains(&(
            provider_identity(session)?,
            first_event_fingerprint(session),
        )))
    }
}

// A fingerprint group is indivisible: leaving one present/young sibling behind
// while excluding the fingerprint would erase or later resurrect that sibling.
const PURGE_CANDIDATES_SQL: &str = "
SELECT d.session_key,d.identity_key,d.first_event_fingerprint,d.current_snapshot_hash,d.lifecycle,
       length(s.session_json),p.summary_json
FROM durable_sessions d JOIN session_summaries p ON p.session_key=d.session_key
JOIN session_snapshots s ON s.session_key=d.session_key AND s.version=d.current_snapshot_version
WHERE d.lifecycle <> 'present' AND d.ledger_dirty=0
 AND substr(json_extract(p.summary_json,'$.last_event_at'),1,10) < ?1
 AND NOT EXISTS(
   SELECT 1 FROM durable_sessions sibling LEFT JOIN session_summaries sp ON sp.session_key=sibling.session_key
   WHERE sibling.identity_key=d.identity_key AND sibling.first_event_fingerprint=d.first_event_fingerprint
    AND (sibling.lifecycle='present' OR sibling.ledger_dirty<>0 OR sp.summary_json IS NULL
      OR substr(json_extract(sp.summary_json,'$.last_event_at'),1,10) >= ?1))
ORDER BY d.session_key";

fn purge_candidates(
    connection: &Connection,
    cutoff: &str,
) -> Result<(PurgePreview, ExclusionIdentities)> {
    let mut statement = connection.prepare(PURGE_CANDIDATES_SQL)?;
    let mut rows = statement.query([cutoff])?;
    let mut candidates = Vec::new();
    let mut groups = std::collections::BTreeSet::new();
    let mut revision = cutoff.to_owned();
    let mut tokens = TokenTotals::default();
    let mut bytes = 0;
    while let Some(row) = rows.next()? {
        if candidates.len() >= 100_000 {
            bail!("purge candidate limit exceeded; select a shorter retention duration");
        }
        let key: String = row.get(0)?;
        let identity: String = row.get(1)?;
        let fingerprint: String = row.get(2)?;
        let hash: String = row.get(3)?;
        let state: String = row.get(4)?;
        bytes += u64::try_from(row.get::<_, i64>(5)?)?;
        let raw: Vec<u8> = row.get(6)?;
        let summary: SessionSummary = serde_json::from_slice(&raw)?;
        tokens += &summary.tokens_total;
        revision.push_str(&format!("\u{1f}{key}\u{1f}{hash}\u{1f}{state}"));
        groups.insert((identity.clone(), fingerprint.clone()));
        candidates.push((key, identity, fingerprint));
    }
    Ok((
        PurgePreview {
            cutoff_utc_day: cutoff.to_owned(),
            sessions: candidates.len() as u64,
            identity_groups: groups.len() as u64,
            snapshot_bytes: bytes,
            tokens,
            revision: stable_hash(&revision),
        },
        candidates,
    ))
}

impl HistoryStore {
    pub fn exclusion_path_identity(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn session_lifecycles(
        &self,
    ) -> Result<HashMap<String, (SessionLifecycle, SourceAvailability)>> {
        let connection = self.open_reader()?;
        self.check_session_count(&connection)?;
        let mut statement=connection.prepare("SELECT d.session_key,d.lifecycle,EXISTS(SELECT 1 FROM source_locations l WHERE l.session_key=d.session_key AND l.present=1) FROM durable_sessions d ORDER BY d.session_key")?;
        let mut rows = statement.query([])?;
        let mut result = HashMap::new();
        while let Some(row) = rows.next()? {
            self.check_query()?;
            if let Some(control) = &self.query_control {
                control.consume_row()?;
            }
            result.insert(
                row.get(0)?,
                (
                    decode_lifecycle(&row.get::<_, String>(1)?)?,
                    if row.get::<_, bool>(2)? {
                        SourceAvailability::Present
                    } else {
                        SourceAvailability::Missing
                    },
                ),
            );
        }
        Ok(result)
    }

    pub fn has_session_key(&self, key: &str) -> Result<bool> {
        Ok(self.open_reader()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM durable_sessions WHERE session_key=?1)",
            [key],
            |r| r.get(0),
        )?)
    }

    pub fn is_session_excluded(&self, session: &Session) -> Result<bool> {
        let connection = self.open_reader()?;
        is_excluded(&connection, session)
    }

    pub fn is_purged_key(&self, key: &str) -> Result<bool> {
        Ok(self.open_reader()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM purged_sessions WHERE session_key=?1) AND NOT EXISTS(SELECT 1 FROM durable_sessions WHERE session_key=?1)",
            [key],
            |r| r.get(0),
        )?)
    }

    pub fn preview_purge(&self, now: DateTime<Utc>) -> Result<PurgePreview> {
        let status = self.retention_status()?;
        let days = status
            .policy
            .retained_days
            .ok_or_else(|| anyhow!("choose a retention duration before reviewing purge"))?;
        let cutoff = (now.date_naive() - chrono::Duration::days(i64::from(days))).to_string();
        let connection = self.open_reader()?;
        Ok(purge_candidates(&connection, &cutoff)?.0)
    }

    /// Confirmed removal of local derived history only. Tombstones and every
    /// child-table deletion commit together, so a restart never reimports half a purge.
    pub fn purge_retained(
        &self,
        preview: &PurgePreview,
        now: DateTime<Utc>,
    ) -> Result<PurgeResult> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (current, candidates) = purge_candidates(&transaction, &preview.cutoff_utc_day)?;
        if &current != preview {
            bail!("history changed after the purge preview; review a fresh preview");
        }
        write_exclusions(&self.path, &candidates, now.timestamp_millis())?;
        for (key, identity, fingerprint) in &candidates {
            transaction.execute("INSERT INTO purged_sessions(session_key,identity_key,first_event_fingerprint,purged_at_ms) VALUES(?1,?2,?3,?4)",params![key,identity,fingerprint,now.timestamp_millis()])?;
            // New session-owned derived features must extend this deletion list
            // in the same transaction; project-wide aliases remain independent.
            for table in [
                "project_session_overrides",
                "source_locations",
                "source_artifacts",
                "session_summaries",
                "session_snapshots",
                "durable_token_events",
                "durable_tool_events",
                "durable_finding_events",
                "durable_tool_dimension_events",
                "rollup_token_totals",
                "rollup_tool_metrics",
                "rollup_mutation_chains",
                "rollup_tool_dimensions",
            ] {
                transaction.execute(&format!("DELETE FROM {table} WHERE session_key=?1"), [key])?;
            }
            transaction.execute(
                "DELETE FROM history_meta WHERE key=?1",
                [format!("{ROLLUPS_PENDING_PREFIX}{key}")],
            )?;
            transaction.execute("DELETE FROM durable_sessions WHERE session_key=?1", [key])?;
        }
        transaction.commit()?;
        Ok(PurgeResult {
            removed_keys: candidates.into_iter().map(|(key, _, _)| key).collect(),
            purged_at: now,
        })
    }

    /// Recovery of readable sources cannot claim that missing historical sources were recovered.
    pub fn has_complete_coverage(&self) -> Result<bool> {
        let connection = self.open_reader()?;
        Ok(connection.query_row(
            "SELECT value='1' FROM history_meta WHERE key='coverage_complete'",
            [],
            |r| r.get(0),
        )?)
    }

    pub fn retention_status(&self) -> Result<RetentionStatus> {
        let connection = self.open_reader()?;
        let policy: Option<String> = connection
            .query_row(
                "SELECT value FROM history_meta WHERE key='retention_policy'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        let count = |state: &str| -> Result<u64> {
            Ok(u64::try_from(connection.query_row(
                "SELECT count(*) FROM durable_sessions WHERE lifecycle=?1",
                [state],
                |r| r.get::<_, i64>(0),
            )?)?)
        };
        let recovered: Option<i64> = connection
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM history_meta WHERE key='recovered_at_ms'",
                [],
                |r| r.get(0),
            )
            .optional()?;
        Ok(RetentionStatus {
            policy: policy
                .map(|p| serde_json::from_str(&p))
                .transpose()?
                .unwrap_or_default(),
            present_sessions: count("present")?,
            retained_sessions: count("retained")?,
            superseded_sessions: count("superseded")?,
            purged_sessions: u64::try_from(connection.query_row(
                "SELECT count(*) FROM purged_sessions",
                [],
                |r| r.get::<_, i64>(0),
            )?)?,
            coverage_complete: connection.query_row(
                "SELECT value='1' FROM history_meta WHERE key='coverage_complete'",
                [],
                |r| r.get(0),
            )?,
            recovered_at: recovered.and_then(DateTime::from_timestamp_millis),
        })
    }

    pub fn set_retention_policy(&self, policy: &RetentionPolicy) -> Result<()> {
        if policy
            .retained_days
            .is_some_and(|days| !(1..=36500).contains(&days))
        {
            bail!("retention duration must be between 1 and 36500 days");
        }
        self.connection()?.execute("INSERT INTO history_meta(key,value) VALUES('retention_policy',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[serde_json::to_string(policy)?])?;
        Ok(())
    }

    /// Enumerate retained sessions without reading any full Session snapshot.
    pub fn session_summaries(&self) -> Result<Vec<SessionSummary>> {
        let connection = self.open_reader()?;
        self.check_session_count(&connection)?;
        let mut statement = connection.prepare(
            "SELECT d.session_key,p.summary_json,d.lifecycle,d.thread_name_overlay_set,d.thread_name_overlay,
              EXISTS(SELECT 1 FROM source_locations l WHERE l.session_key=d.session_key AND l.present=1),
              (SELECT path FROM source_locations l WHERE l.session_key=d.session_key ORDER BY present DESC,last_seen_at_ms DESC,path LIMIT 1)
             FROM durable_sessions d LEFT JOIN session_summaries p ON p.session_key=d.session_key
             ORDER BY d.last_seen_at_ms DESC,d.session_key",
        )?;
        let mut rows = statement.query([])?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            self.check_query()?;
            if let Some(control) = &self.query_control {
                control.consume_row()?;
            }
            let key: String = row.get(0)?;
            let raw: Option<Vec<u8>> = row.get(1)?;
            let raw = raw.ok_or_else(|| anyhow!("missing durable summary for {key}"))?;
            if let Some(control) = &self.query_control {
                control.consume_snapshot_bytes(raw.len())?;
            }
            let mut summary: SessionSummary = serde_json::from_slice(&raw)
                .with_context(|| format!("corrupt durable summary for {key}"))?;
            summary.storage_id = key;
            summary.lifecycle = decode_lifecycle(&row.get::<_, String>(2)?)?;
            if row.get::<_, bool>(3)? {
                summary.thread_name = row.get(4)?;
                truncate_preview(&mut summary.thread_name, 512);
            }
            summary.source_availability = if row.get::<_, bool>(5)? {
                SourceAvailability::Present
            } else {
                SourceAvailability::Missing
            };
            if let Some(path) = row.get::<_, Option<String>>(6)? {
                summary.file_path = path;
            }
            result.push(summary);
        }
        Ok(result)
    }
}
