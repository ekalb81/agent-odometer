//! Explicit, private, small review cases. No accounting, model execution or uploads.
use super::*;
use crate::{
    store::AppState,
    transcript::{self, TranscriptCursor, TranscriptRequest},
};
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    LazyLock,
};

const MAX_CASES: i64 = 64;
const MAX_CASE_BYTES: usize = 16 * 1024;
const JOURNAL_LIMIT: i64 = 4096;
static NEXT_PREVIEW: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CuratedBlock {
    pub role: String,
    pub text: String,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CuratedContent {
    pub name: String,
    pub rubric: String,
    pub expected_outcome: String,
    pub source_provider: String,
    pub source_fingerprint_at_capture: String,
    pub source_records: Vec<String>,
    pub captured_at: String,
    pub blocks: Vec<CuratedBlock>,
    pub redactions: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratedCase {
    pub id: i64,
    pub version: i64,
    pub session_key: String,
    pub fingerprint: String,
    pub content_hash: String,
    pub content: CuratedContent,
}
#[derive(Debug, Serialize)]
pub struct CuratedChange {
    pub revision: i64,
    pub case_id: i64,
    pub change: String,
    pub content_hash: String,
}
#[derive(Debug, Serialize)]
pub struct CuratedDataset {
    pub export_digest: String,
    pub format_version: u32,
    pub revision: i64,
    pub earliest_change_revision: Option<i64>,
    pub cases: Vec<CuratedCase>,
    pub changes: Vec<CuratedChange>,
    pub recovery_backup_unrestored: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CuratedRequest {
    pub identity: AnnotationIdentity,
    pub outcome_revision: i64,
    pub dataset_revision: i64,
    pub case_id: Option<i64>,
    pub record_ids: Vec<String>,
    pub name: String,
    pub rubric: String,
    pub redact_phrases: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct CuratedPreview {
    pub token: String,
    pub content: CuratedContent,
    pub replacing_case: Option<i64>,
}
pub(super) struct Pending {
    token: String,
    created: Instant,
    identity: AnnotationIdentity,
    outcome_revision: i64,
    dataset_revision: i64,
    case_id: Option<i64>,
    content: CuratedContent,
}
#[derive(Debug, Serialize)]
pub struct CuratedCandidate {
    pub record_id: String,
    pub role: String,
    pub excerpt: String,
}
#[derive(Debug, Serialize)]
pub struct CuratedCandidates {
    pub records: Vec<CuratedCandidate>,
    pub next_cursor: Option<TranscriptCursor>,
    pub availability: String,
}

// Shared #256 policy, checked against the same synthetic fixtures in Rust and
// TypeScript. Only this backend projection can supply persisted dataset content.
// ponytail: heuristic redaction cannot identify every secret; exact preview review
// remains mandatory. Extend shared fixtures before adding a demonstrated rule.
pub(super) fn redact(text: &str, phrases: &[String]) -> (String, usize) {
    static RULES: LazyLock<Vec<(regex::Regex, &'static str)>> = LazyLock::new(|| {
        [
        (r"-----BEGIN [^-\r\n]*PRIVATE KEY-----[\s\S]*?(?:-----END [^-\r\n]*PRIVATE KEY-----|$)", "private key"),
        (r"\b(?:sk-(?:proj-)?[A-Za-z0-9_-]{8,}|(?:gh[pousr]_|github_pat_)[A-Za-z0-9_]{8,}|AKIA[A-Z0-9]{16})\b", "credential"),
        (r#"(?i)\bBearer\s+[^\s\"'<>,;]+"#, "authorization"),
        (r#"(?i)[\"']?(?:api[-_]?key|password|passwd|secret|client_secret|access_token|refresh_token|token)[\"']?\s*[:=]\s*(?:\"(?:\\.|[^\"\\])*\"|'(?:\\.|[^'\\])*'|[^\s,;}]+)"#, "credential field"),
        (r#"(?i)(?:https?|file)://[^\s<>\"']+"#, "URL"),
        (r#"(?:[A-Za-z]:[\\/]|\\\\)[^\r\n<>\"'|]+"#, "local path"),
        (r#"(?i)data:[^\s\"'<>]+"#, "attachment"),
        (r#"(?i)[\"'](?:data|base64|b64_json)[\"']\s*:\s*\"(?:\\.|[^\"\\])*\""#, "attachment field"),
        (r"[A-Za-z0-9+/_=-]{160,}", "encoded content"),
    ].into_iter().map(|(p,l)| (regex::Regex::new(p).unwrap(),l)).collect()
    });
    static UNIX_PATH: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?m)(^|[\s(\"'=])/[^\s<>\"',;)]+"#).unwrap());
    let mut value = text.to_owned();
    let mut count = 0;
    for (index, (pattern, label)) in RULES.iter().enumerate() {
        // Unix paths follow Windows paths, before encoded attachment rules.
        if index == 6 {
            value = UNIX_PATH
                .replace_all(&value, |c: &regex::Captures| {
                    count += 1;
                    format!("{}[local path redacted]", &c[1])
                })
                .into_owned();
        }
        value = pattern
            .replace_all(&value, |_: &regex::Captures| {
                count += 1;
                format!("[{label} redacted]")
            })
            .into_owned();
    }
    for phrase in phrases.iter().filter(|p| !p.is_empty()) {
        count += value.matches(phrase).count();
        value = value.replace(phrase, "[review phrase redacted]");
    }
    (value, count)
}
fn bounded(mut text: String, maximum: usize) -> (String, bool) {
    let truncated = text.len() > maximum;
    if truncated {
        let mut end = maximum;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    (text, truncated)
}
fn project(
    record: &crate::transcript::TranscriptRecord,
    phrases: &[String],
) -> Option<(CuratedBlock, usize)> {
    let view = record.presentation.as_ref()?;
    let role = view.role.as_deref()?;
    if !["user", "assistant"].contains(&role) {
        return None;
    }
    let text = view
        .blocks
        .iter()
        .filter(|b| b.kind == "text")
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.trim().is_empty() {
        return None;
    }
    let (text, count) = redact(&text, phrases);
    let (text, truncated) = bounded(text, 3000);
    Some((
        CuratedBlock {
            role: role.into(),
            text,
            truncated,
        },
        count,
    ))
}
pub fn candidates(
    state: &AppState,
    session_key: String,
    cursor: Option<TranscriptCursor>,
) -> CuratedCandidates {
    let page = transcript::read_for_session(
        state,
        TranscriptRequest {
            session_id: session_key,
            cursor,
            max_records: Some(25),
            max_bytes: Some(256 * 1024),
            ..Default::default()
        },
    );
    let records = page
        .records
        .iter()
        .filter_map(|r| {
            project(r, &[]).map(|(b, _)| CuratedCandidate {
                record_id: r.id.clone(),
                role: b.role,
                excerpt: bounded(b.text, 160).0,
            })
        })
        .collect();
    CuratedCandidates {
        records,
        next_cursor: page.next_cursor,
        availability: format!("{:?}", page.availability).to_lowercase(),
    }
}
fn revision(conn: &Connection) -> Result<i64> {
    Ok(conn
        .query_row(
            "SELECT CAST(value AS INTEGER) FROM history_meta WHERE key='dataset_revision'",
            [],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}
fn validate_rating(
    conn: &Connection,
    identity: &AnnotationIdentity,
    expected_revision: i64,
) -> Result<String> {
    let row: Option<(String,i64,Option<String>)>=conn.query_row("SELECT d.first_event_fingerprint,a.revision,a.outcome_label FROM durable_sessions d JOIN session_annotations a ON a.session_key=d.session_key AND a.anchor='' WHERE d.session_key=?1",[&identity.session_key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    let (fingerprint, rev, label) =
        row.ok_or_else(|| anyhow!("Use a session with an explicit Accepted or Rejected outcome"))?;
    if !identity.anchor.is_empty()
        || fingerprint != identity.fingerprint
        || rev != expected_revision
    {
        bail!("Outcome identity or revision changed; reload before curating");
    }
    let label = label
        .filter(|v| ["accepted", "rejected"].contains(&v.as_str()))
        .ok_or_else(|| {
            anyhow!("Only explicitly Accepted or Rejected outcomes can become examples")
        })?;
    Ok(label)
}
fn verify_records(
    state: &AppState,
    identity: &AnnotationIdentity,
    ids: &[String],
) -> Result<Vec<crate::transcript::TranscriptRecord>> {
    let mut records = Vec::new();
    for id in ids {
        let mut page = transcript::read_for_session(
            state,
            TranscriptRequest {
                session_id: identity.session_key.clone(),
                record_id: Some(id.clone()),
                max_records: Some(1),
                max_bytes: Some(256 * 1024),
                ..Default::default()
            },
        );
        if page.records.len() != 1
            || page.records[0].id != *id
            || page.records[0].raw_json.is_none()
            || page.records[0].issue.is_some()
        {
            bail!("Selected source record is unavailable or changed; rebuild the preview");
        }
        records.push(page.records.remove(0));
    }
    Ok(records)
}
fn validate_case_owner(conn: &Connection, id: i64, identity: &AnnotationIdentity) -> Result<()> {
    let owner: Option<(String, String)> = conn
        .query_row(
            "SELECT session_key,first_event_fingerprint FROM curated_cases WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if owner.as_ref() != Some(&(identity.session_key.clone(), identity.fingerprint.clone())) {
        bail!("Example was removed or belongs to another source; reload it");
    }
    Ok(())
}
impl HistoryStore {
    /// Native export only: no SQLite lock spans the user-controlled picker.
    pub fn export_current_dataset(
        &self,
        revision: i64,
        digest: &str,
        publish: impl FnOnce(&str) -> Result<()>,
    ) -> Result<()> {
        let dataset = self.curated_dataset()?;
        if dataset.revision != revision || dataset.export_digest != digest {
            bail!("Dataset changed; reload before exporting");
        }
        let content = serde_json::to_string_pretty(&dataset)? + "\n";
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: i64 = tx
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM history_meta WHERE key='dataset_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if current != revision {
            bail!("Dataset changed; reload before exporting");
        }
        publish(&content)?;
        tx.commit()?;
        Ok(())
    }
    pub fn curated_dataset(&self) -> Result<CuratedDataset> {
        let mut connection = self.connection()?;
        let conn = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let mut rows=conn.prepare("SELECT id,version,session_key,first_event_fingerprint,content_hash,content_json FROM curated_cases ORDER BY id LIMIT 65")?;
        let cases = rows
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get::<_, String>(5)?,
                ))
            })?
            .map(|r| {
                let (id, version, session_key, fingerprint, content_hash, body) = r?;
                Ok(CuratedCase {
                    id,
                    version,
                    session_key,
                    fingerprint,
                    content_hash,
                    content: serde_json::from_str(&body)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if cases.len() > MAX_CASES as usize {
            bail!("Dataset exceeds its supported case limit");
        }
        let mut journal=conn.prepare("SELECT revision,case_id,change,content_hash FROM curated_changes ORDER BY revision LIMIT 4096")?;
        let changes = journal
            .query_map([], |r| {
                Ok(CuratedChange {
                    revision: r.get(0)?,
                    case_id: r.get(1)?,
                    change: r.get(2)?,
                    content_hash: r.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let revision = revision(&conn)?;
        drop(journal);
        drop(rows);
        conn.commit()?;
        drop(connection);
        let mut dataset = CuratedDataset {
            export_digest: String::new(),
            format_version: 1,
            revision,
            earliest_change_revision: changes.first().map(|c| c.revision),
            cases,
            changes,
            recovery_backup_unrestored: self.organization_recovery_pending()?,
        };
        dataset.export_digest = stable_hash(&serde_json::to_string(&dataset)?);
        Ok(dataset)
    }
    pub fn prepare_curated(
        &self,
        state: &AppState,
        request: CuratedRequest,
    ) -> Result<CuratedPreview> {
        organization::validate_identity(&request.identity)?;
        if request.record_ids.is_empty()
            || request.record_ids.len() > 4
            || request.record_ids.iter().any(|id| id.len() > 2048)
            || request.name.trim().is_empty()
            || request.name.len() > 128
            || request.rubric.len() > 1024
            || request.redact_phrases.len() > 100
            || request
                .redact_phrases
                .iter()
                .any(|p| p.len() > 1024 || p.chars().count() > 256)
        {
            bail!("Select one to four conversation records, a name up to 128 bytes, a rubric up to 1024 bytes, and at most 100 redaction phrases of 256 characters");
        }
        let unique: std::collections::HashSet<_> = request.record_ids.iter().collect();
        if unique.len() != request.record_ids.len() {
            bail!("Select each source record once");
        }
        let label = {
            let mut connection = self.connection()?;
            let conn = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
            if revision(&conn)? != request.dataset_revision {
                bail!("Dataset changed; reload before preparing");
            }
            let label = validate_rating(&conn, &request.identity, request.outcome_revision)?;
            if let Some(id) = request.case_id {
                validate_case_owner(&conn, id, &request.identity)?;
            }
            conn.commit()?;
            label
        };
        let records = verify_records(state, &request.identity, &request.record_ids)?;
        let mut blocks = Vec::new();
        let mut redactions = 0;
        for record in &records {
            let (block,count)=project(record,&request.redact_phrases).ok_or_else(||anyhow!("Select conversation text; tools, reasoning, unknown records and attachments are omitted"))?;
            blocks.push(block);
            redactions += count;
        }
        let (name, count) = redact(request.name.trim(), &request.redact_phrases);
        redactions += count;
        let (rubric, count) = redact(request.rubric.trim(), &request.redact_phrases);
        redactions += count;
        let source_provider=self.connection()?.query_row("SELECT json_extract(CAST(summary_json AS TEXT),'$.harness') FROM session_summaries WHERE session_key=?1",[&request.identity.session_key],|r|r.get::<_,String>(0)).optional()?.unwrap_or_else(||"recorded_source".into());
        let content = CuratedContent {
            name,
            rubric,
            expected_outcome: label,
            source_provider,
            source_fingerprint_at_capture: request.identity.fingerprint.clone(),
            source_records: request.record_ids,
            captured_at: chrono::Utc::now().to_rfc3339(),
            blocks,
            redactions,
        };
        if serde_json::to_vec(&content)?.len() > MAX_CASE_BYTES {
            bail!("Example exceeds 16 KiB; select fewer records");
        }
        let token = NEXT_PREVIEW.fetch_add(1, Ordering::Relaxed).to_string();
        let mut pending = self.curated_previews.lock().unwrap();
        pending.retain(|p| p.created.elapsed() < Duration::from_secs(300));
        if pending.len() >= 8 {
            pending.remove(0);
        }
        pending.push(Pending {
            token: token.clone(),
            created: Instant::now(),
            identity: request.identity,
            outcome_revision: request.outcome_revision,
            dataset_revision: request.dataset_revision,
            case_id: request.case_id,
            content: content.clone(),
        });
        Ok(CuratedPreview {
            token,
            content,
            replacing_case: request.case_id,
        })
    }
    pub fn commit_curated(
        &self,
        state: &AppState,
        token: &str,
        reviewed: bool,
    ) -> Result<CuratedDataset> {
        if !reviewed {
            bail!("Review the exact minimized preview before adding it");
        }
        let pending = {
            let mut list = self.curated_previews.lock().unwrap();
            let index = list
                .iter()
                .position(|p| p.token == token && p.created.elapsed() < Duration::from_secs(300))
                .ok_or_else(|| anyhow!("Preview expired or was consumed; build it again"))?;
            list.remove(index)
        };
        verify_records(state, &pending.identity, &pending.content.source_records)?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if revision(&tx)? != pending.dataset_revision {
            bail!("Dataset changed after preview; reload it");
        }
        if validate_rating(&tx, &pending.identity, pending.outcome_revision)?
            != pending.content.expected_outcome
        {
            bail!("Human outcome changed after preview");
        }
        let body = serde_json::to_string(&pending.content)?;
        let hash = stable_hash(&body);
        let version = pending
            .dataset_revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("Dataset revision exhausted"))?;
        if let Some(id) = pending.case_id {
            validate_case_owner(&tx, id, &pending.identity)?;
            tx.execute(
                "UPDATE curated_cases SET version=?2,content_json=?3,content_hash=?4 WHERE id=?1",
                params![id, version, body, hash],
            )?;
        } else {
            let count: i64 =
                tx.query_row("SELECT COUNT(*) FROM curated_cases", [], |r| r.get(0))?;
            if count >= MAX_CASES {
                bail!("This small dataset is limited to64 examples; remove one first");
            }
            tx.execute("INSERT INTO curated_cases(version,session_key,first_event_fingerprint,content_json,content_hash) VALUES(?1,?2,?3,?4,?5)",params![version,pending.identity.session_key,pending.identity.fingerprint,body,hash])?;
        }
        tx.commit()?;
        drop(conn);
        self.curated_dataset()
    }
    pub fn remove_curated(&self, id: i64, expected_revision: i64) -> Result<CuratedDataset> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if revision(&tx)? != expected_revision {
            bail!("Dataset changed; reload before removing");
        }
        if tx.execute("DELETE FROM curated_cases WHERE id=?1", [id])? != 1 {
            bail!("Example already removed");
        }
        tx.commit()?;
        drop(conn);
        self.curated_dataset()
    }
}
pub(super) fn install_schema(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE curated_cases(id INTEGER PRIMARY KEY AUTOINCREMENT,version INTEGER NOT NULL,session_key TEXT NOT NULL,first_event_fingerprint TEXT NOT NULL,content_json TEXT NOT NULL CHECK(length(CAST(content_json AS BLOB))<=16384),content_hash TEXT NOT NULL,FOREIGN KEY(session_key,first_event_fingerprint) REFERENCES durable_sessions(session_key,first_event_fingerprint) ON UPDATE CASCADE ON DELETE CASCADE);
        CREATE INDEX curated_cases_session_idx ON curated_cases(session_key);
        CREATE TABLE curated_changes(revision INTEGER PRIMARY KEY,case_id INTEGER NOT NULL,change TEXT NOT NULL CHECK(change IN ('added','edited','removed')),content_hash TEXT NOT NULL);")?;
    for (operation, kind, row) in [
        ("INSERT", "added", "NEW"),
        ("UPDATE OF content_json", "edited", "NEW"),
        ("DELETE", "removed", "OLD"),
    ] {
        tx.execute_batch(&format!("CREATE TRIGGER curated_{} AFTER {operation} ON curated_cases BEGIN
            INSERT INTO history_meta(key,value) VALUES('dataset_revision','1') ON CONFLICT(key) DO UPDATE SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT);
            INSERT INTO history_meta(key,value) VALUES('organization_revision','1') ON CONFLICT(key) DO UPDATE SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT);
            INSERT INTO curated_changes SELECT CAST(value AS INTEGER),{row}.id,'{kind}',{row}.content_hash FROM history_meta WHERE key='dataset_revision';
            DELETE FROM curated_changes WHERE revision <= (SELECT CAST(value AS INTEGER)-{JOURNAL_LIMIT} FROM history_meta WHERE key='dataset_revision'); END;",kind))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_shared_export_redaction_policy_before_minimized_projection() {
        let fixtures: serde_json::Value = serde_json::from_str(include_str!(
            "../../src/lib/fixtures/transcriptRedaction.json"
        ))
        .unwrap();
        for vector in fixtures["vectors"].as_array().unwrap() {
            let phrases: Vec<String> = serde_json::from_value(vector["phrases"].clone()).unwrap();
            let (text, count) = redact(vector["input"].as_str().unwrap(), &phrases);
            assert_eq!(text, vector["text"].as_str().unwrap(), "{}", vector["name"]);
            assert_eq!(
                count,
                vector["count"].as_u64().unwrap() as usize,
                "{}",
                vector["name"]
            );
        }
        let (text, cut) = bounded("😀".repeat(1000), 3000);
        assert!(cut);
        assert_eq!(text.len(), 3000);
        let record = crate::transcript::TranscriptRecord {
            id: "synthetic".into(),
            byte_offset: 0,
            byte_length: 10,
            raw_json: Some("DO_NOT_COPY_RAW".into()),
            kind: None,
            message_id: None,
            issue: None,
            context_evidence: None,
            presentation: Some(crate::transcript_view::TranscriptPresentation {
                role: Some("assistant".into()),
                timestamp: None,
                blocks: vec![crate::transcript_view::TranscriptBlock {
                    kind: "reasoning",
                    text: "DO_NOT_COPY_REASONING".into(),
                    call_id: None,
                    name: None,
                    edit: None,
                }],
            }),
        };
        assert!(project(&record, &[]).is_none());
    }
    #[test]
    fn fourteen_migration_preserves_labels_and_measurements_and_versions_deletion() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.sqlite");
        let source = root.path().join("source.jsonl");
        std::fs::write(&source,"{\"type\":\"session_meta\",\"payload\":{\"id\":\"curated-synthetic\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n").unwrap();
        let session = crate::parser::parse_file(&source, false).unwrap().unwrap();
        let store = HistoryStore::open(&path).unwrap();
        let key = store.observe(&source, &session, 1).unwrap().key;
        let initial = store
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        let rated = store
            .edit_annotation(&AnnotationEdit {
                identity: initial.identity.clone(),
                revision: initial.revision,
                pinned: true,
                note: "PRIVATE_NOTE_STAYS_PRIVATE".into(),
                tags: vec!["Review".into()],
                outcome: Some(HumanOutcome {
                    label: "accepted".into(),
                    ..Default::default()
                }),
            })
            .unwrap();
        let measurements = serde_json::to_value(store.session_summaries().unwrap()).unwrap();
        store.connection().unwrap().execute_batch("DROP TABLE offline_cases; DROP TABLE offline_experiments; DROP TABLE curated_cases; DROP TABLE curated_changes; UPDATE history_meta SET value='14' WHERE key='schema_version'; PRAGMA user_version=14;").unwrap();
        drop(store);
        let mut steps = Vec::new();
        let store = HistoryStore::open_with_progress(&path, |e| {
            if e.elapsed_ms.is_some() {
                steps.push(e.step)
            }
        })
        .unwrap();
        assert_eq!(
            steps,
            [
                "v14_to_v15_curated_dataset",
                "v15_to_v16_offline_comparisons"
            ]
        );
        assert_eq!(
            serde_json::to_value(store.session_summaries().unwrap()).unwrap(),
            measurements
        );
        assert_eq!(
            store
                .get_annotation(&initial.identity)
                .unwrap()
                .summary
                .outcome,
            rated.summary.outcome
        );
        assert!(store.curated_dataset().unwrap().cases.is_empty());
        // Direct synthetic insert verifies SQL limits/journal/cascade independently
        // of the source gate, which is covered by the isolated native reader test.
        let content = CuratedContent {
            name: "Synthetic reviewed case".into(),
            rubric: String::new(),
            expected_outcome: "accepted".into(),
            source_provider: "codex".into(),
            source_fingerprint_at_capture: initial.identity.fingerprint.clone(),
            source_records: vec!["synthetic-anchor".into()],
            captured_at: "2026-01-01T00:00:00Z".into(),
            blocks: vec![CuratedBlock {
                role: "user".into(),
                text: "sanitized synthetic excerpt".into(),
                truncated: false,
            }],
            redactions: 0,
        };
        let json = serde_json::to_string(&content).unwrap();
        store.connection().unwrap().execute("INSERT INTO curated_cases(version,session_key,first_event_fingerprint,content_json,content_hash) VALUES(1,?1,?2,?3,'synthetic-hash')",params![key,initial.identity.fingerprint,json]).unwrap();
        let first = store.curated_dataset().unwrap();
        assert_eq!(first.revision, 1);
        assert_eq!(first.changes[0].change, "added");
        validate_case_owner(
            &store.connection().unwrap(),
            first.cases[0].id,
            &initial.identity,
        )
        .unwrap();
        let mut foreign = initial.identity.clone();
        foreign.session_key = "codex:thread:other-source".into();
        assert!(
            validate_case_owner(&store.connection().unwrap(), first.cases[0].id, &foreign).is_err()
        );
        assert!(!serde_json::to_string(&first)
            .unwrap()
            .contains("PRIVATE_NOTE"));
        assert!(store.remove_curated(first.cases[0].id, 0).is_err());
        let removed = store.remove_curated(first.cases[0].id, 1).unwrap();
        assert!(removed.cases.is_empty());
        assert_eq!(removed.revision, 2);
        assert_eq!(removed.changes[1].change, "removed");
        assert!(!serde_json::to_string(&removed)
            .unwrap()
            .contains("sanitized synthetic excerpt"));
        assert!(store.connection().unwrap().execute("INSERT INTO curated_cases(version,session_key,first_event_fingerprint,content_json,content_hash) VALUES(3,?1,?2,?3,'oversized')",params![key,initial.identity.fingerprint,"x".repeat(MAX_CASE_BYTES+1)]).is_err());
        drop(store);
        let store = HistoryStore::open(&path).unwrap();
        assert_eq!(store.curated_dataset().unwrap().revision, 2);
        let mut conn = store.connection().unwrap();
        let tx = conn.transaction().unwrap();
        tx.execute("INSERT INTO curated_cases(version,session_key,first_event_fingerprint,content_json,content_hash) VALUES(3,?1,?2,?3,'journal')",params![key,initial.identity.fingerprint,json]).unwrap();
        let id = tx.last_insert_rowid();
        for version in 4..=4103 {
            tx.execute(
                "UPDATE curated_cases SET content_json=?1,version=?2 WHERE id=?3",
                params![json, version, id],
            )
            .unwrap();
        }
        tx.execute("DELETE FROM curated_cases WHERE id=?1", [id])
            .unwrap();
        tx.commit().unwrap();
        drop(conn);
        let bounded = store.curated_dataset().unwrap();
        assert!(bounded.cases.is_empty());
        assert_eq!(bounded.revision, 4104);
        assert_eq!(bounded.changes.len(), JOURNAL_LIMIT as usize);
        assert_eq!(bounded.earliest_change_revision, Some(9));
        assert_eq!(bounded.changes.last().unwrap().change, "removed");
    }

    #[cfg(unix)]
    #[test]
    fn reviewed_source_gate_races_archives_restart_and_purge_in_isolated_process() {
        let Some(root) = std::env::var_os("ODOMETER_CURATED_TEST_ROOT") else {
            let root = tempfile::tempdir().unwrap();
            let output=std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact","history_store::curated::tests::reviewed_source_gate_races_archives_restart_and_purge_in_isolated_process","--nocapture"])
                .env("ODOMETER_CURATED_TEST_ROOT",root.path()).env("HOME",root.path().join("home"))
                .env("XDG_CONFIG_HOME",root.path().join("config")).env("XDG_DATA_HOME",root.path().join("data")).env("XDG_CACHE_HOME",root.path().join("cache")).output().unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = PathBuf::from(root);
        let sources = root.join("sources");
        let archives = root.join("archives");
        std::fs::create_dir_all(&sources).unwrap();
        std::fs::create_dir_all(&archives).unwrap();
        let mut config = crate::config::Config::default().normalized();
        config.config_version = crate::config::CONFIG_VERSION;
        for value in config.providers.values_mut() {
            value.live_roots.clear();
            value.archive_roots.clear();
        }
        let roots = config
            .providers
            .get_mut(&crate::provider::codex_provider_id())
            .unwrap();
        roots.live_roots = vec![sources.clone()];
        roots.archive_roots = vec![archives.clone()];
        config.save().unwrap();
        let path = sources.join("synthetic.jsonl");
        let raw=concat!("{\"type\":\"session_meta\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"payload\":{\"id\":\"curated-native-test\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n",
            "{\"type\":\"response_item\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"CURATED_SELECTED_SOURCE password=synthetic-secret https://example.invalid/private\"}]}}\n",
            "{\"type\":\"response_item\",\"timestamp\":\"2026-01-01T00:00:02Z\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"CURATED_OTHER_SOURCE\"}]}}\n");
        std::fs::write(&path, raw).unwrap();
        let history =
            std::sync::Arc::new(HistoryStore::open(&root.join("history.sqlite")).unwrap());
        let session = crate::parser::parse_file(&path, false).unwrap().unwrap();
        let key = history.observe(&path, &session, 1).unwrap().key;
        let state = AppState::new();
        state.set_history_ready(Some(history.clone()));
        state.publish_watched_session(&path, session);
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            transcript::source_generation(&path),
        );
        let page = candidates(&state, key.clone(), None);
        assert_eq!(page.records.len(), 2);
        assert!(!serde_json::to_string(&page)
            .unwrap()
            .contains("synthetic-secret"));
        let initial = history
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        let request = |identity: AnnotationIdentity,
                       outcome_revision: i64,
                       dataset_revision: i64,
                       case_id: Option<i64>| CuratedRequest {
            identity,
            outcome_revision,
            dataset_revision,
            case_id,
            record_ids: vec![page.records[0].record_id.clone()],
            name: "Synthetic selected case".into(),
            rubric: "Private review phrase".into(),
            redact_phrases: vec!["Private review phrase".into()],
        };
        assert!(history
            .prepare_curated(&state, request(initial.identity.clone(), 0, 0, None))
            .is_err());
        let rated = history
            .edit_annotation(&AnnotationEdit {
                identity: initial.identity.clone(),
                revision: 0,
                pinned: false,
                note: "ANNOTATION_PRIVATE_NOTE".into(),
                tags: vec!["Private".into()],
                outcome: Some(HumanOutcome {
                    label: "accepted".into(),
                    ..Default::default()
                }),
            })
            .unwrap();
        let expired = history
            .prepare_curated(
                &state,
                request(initial.identity.clone(), rated.summary.revision, 0, None),
            )
            .unwrap();
        history.curated_previews.lock().unwrap()[0].created -= Duration::from_secs(301);
        assert!(history
            .commit_curated(&state, &expired.token, true)
            .is_err());
        let mut oldest = String::new();
        for i in 0..9 {
            let preview = history
                .prepare_curated(
                    &state,
                    request(initial.identity.clone(), rated.summary.revision, 0, None),
                )
                .unwrap();
            if i == 0 {
                oldest = preview.token;
            }
        }
        assert_eq!(history.curated_previews.lock().unwrap().len(), 8);
        assert!(history.commit_curated(&state, &oldest, true).is_err());
        let preview = history
            .prepare_curated(
                &state,
                request(initial.identity.clone(), rated.summary.revision, 0, None),
            )
            .unwrap();
        assert!(history.curated_dataset().unwrap().cases.is_empty());
        assert!(history
            .commit_curated(&state, &preview.token, false)
            .is_err());
        assert_eq!(preview.content.rubric, "[review phrase redacted]");
        assert!(!serde_json::to_string(&preview)
            .unwrap()
            .contains("synthetic-secret"));
        let first = history
            .commit_curated(&state, &preview.token, true)
            .unwrap();
        let id = first.cases[0].id;
        assert_eq!(first.revision, 1);
        assert!(history
            .prepare_curated(
                &state,
                request(
                    initial.identity.clone(),
                    rated.summary.revision,
                    1,
                    Some(id + 1)
                )
            )
            .is_err());
        assert!(history
            .commit_curated(&state, &preview.token, true)
            .is_err());
        let body = serde_json::to_string(&first).unwrap();
        assert!(!body.contains("CURATED_OTHER_SOURCE"));
        assert!(!body.contains("ANNOTATION_PRIVATE_NOTE"));
        assert!(!body.contains("https://example.invalid"));
        let saved = history
            .prepare_curated(
                &state,
                request(
                    initial.identity.clone(),
                    rated.summary.revision,
                    1,
                    Some(id),
                ),
            )
            .unwrap();
        let competing = history
            .prepare_curated(
                &state,
                request(
                    initial.identity.clone(),
                    rated.summary.revision,
                    1,
                    Some(id),
                ),
            )
            .unwrap();
        let edited = history.commit_curated(&state, &saved.token, true).unwrap();
        assert_eq!(edited.cases[0].id, id);
        assert_eq!(edited.cases[0].version, 2);
        assert_eq!(edited.changes[1].change, "edited");
        assert!(history
            .commit_curated(&state, &competing.token, true)
            .is_err());
        let stale = history
            .prepare_curated(
                &state,
                request(initial.identity.clone(), rated.summary.revision, 2, None),
            )
            .unwrap();
        let rejected = history
            .edit_annotation(&AnnotationEdit {
                identity: initial.identity.clone(),
                revision: rated.summary.revision,
                pinned: false,
                tags: vec![],
                note: String::new(),
                outcome: Some(HumanOutcome {
                    label: "rejected".into(),
                    ..Default::default()
                }),
            })
            .unwrap();
        assert!(history.commit_curated(&state, &stale.token, true).is_err());
        // The label remains valid, but the exact selected record was replaced.
        let stale = history
            .prepare_curated(
                &state,
                request(initial.identity.clone(), rejected.summary.revision, 2, None),
            )
            .unwrap();
        std::fs::write(
            &path,
            raw.replace("CURATED_SELECTED_SOURCE", "CURATED_REPLACED_SOURCE"),
        )
        .unwrap();
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            transcript::source_generation(&path),
        );
        assert!(history.commit_curated(&state, &stale.token, true).is_err());
        std::fs::write(&path, raw).unwrap();
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            transcript::source_generation(&path),
        );
        let archived = archives.join("synthetic-imported.jsonl");
        std::fs::copy(&path, &archived).unwrap();
        let mut session = crate::parser::parse_file(&archived, false)
            .unwrap()
            .unwrap();
        session.archived = true;
        assert_eq!(history.observe(&archived, &session, 2).unwrap().key, key);
        state.publish_watched_session(&archived, session);
        std::fs::remove_file(&path).unwrap();
        history.mark_path_missing(&path).unwrap();
        state.record_transcript_observation(
            state.current_scan_generation(),
            &archived,
            transcript::source_generation(&archived),
        );
        let imported_page = candidates(&state, key.clone(), None);
        assert!(!imported_page.records.is_empty());
        let current = history
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        let mut imported = request(current.identity.clone(), current.revision, 2, None);
        imported.record_ids = vec![imported_page.records[0].record_id.clone()];
        let preview = history.prepare_curated(&state, imported).unwrap();
        let next = history
            .commit_curated(&state, &preview.token, true)
            .unwrap();
        assert_eq!(next.revision, 3);
        assert_eq!(next.cases[1].content.expected_outcome, "rejected");
        let reopened = HistoryStore::open(&root.join("history.sqlite")).unwrap();
        assert_eq!(reopened.curated_dataset().unwrap().cases.len(), 2);
        drop(reopened);
        let current = history
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        let mut appended = request(current.identity.clone(), current.revision, 3, None);
        appended.record_ids = vec![imported_page.records[0].record_id.clone()];
        let appended = history.prepare_curated(&state, appended).unwrap();
        use std::io::Write;
        writeln!(std::fs::OpenOptions::new().append(true).open(&archived).unwrap(), "{{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"APPENDED_UNSELECTED\"}}]}}}}").unwrap();
        state.record_transcript_observation(
            state.current_scan_generation(),
            &archived,
            transcript::source_generation(&archived),
        );
        let mut full = history
            .commit_curated(&state, &appended.token, true)
            .unwrap();
        assert_eq!(full.revision, 4);
        assert!(!serde_json::to_string(&full)
            .unwrap()
            .contains("APPENDED_UNSELECTED"));
        while full.cases.len() < MAX_CASES as usize {
            let mut addition = request(
                current.identity.clone(),
                current.revision,
                full.revision,
                None,
            );
            addition.record_ids = vec![imported_page.records[0].record_id.clone()];
            let addition = history.prepare_curated(&state, addition).unwrap();
            full = history
                .commit_curated(&state, &addition.token, true)
                .unwrap();
        }
        let mut pending = request(
            current.identity.clone(),
            current.revision,
            full.revision,
            None,
        );
        pending.record_ids = vec![imported_page.records[0].record_id.clone()];
        let capped = history.prepare_curated(
            &state,
            request(
                current.identity.clone(),
                current.revision,
                full.revision,
                None,
            ),
        );
        // The old live-file anchor also fails after the move to the archive.
        assert!(capped.is_err());
        let capped = history
            .prepare_curated(
                &state,
                CuratedRequest {
                    identity: current.identity.clone(),
                    outcome_revision: current.revision,
                    dataset_revision: full.revision,
                    case_id: None,
                    record_ids: pending.record_ids.clone(),
                    name: "Beyond cap".into(),
                    rubric: String::new(),
                    redact_phrases: vec![],
                },
            )
            .unwrap();
        assert!(history.commit_curated(&state, &capped.token, true).is_err());
        let pending = history.prepare_curated(&state, pending).unwrap();
        std::fs::remove_file(&archived).unwrap();
        history.mark_path_missing(&archived).unwrap();
        history
            .set_retention_policy(&RetentionPolicy {
                retained_days: Some(1),
            })
            .unwrap();
        let now = "2026-10-04T12:00:00Z".parse().unwrap();
        let purge = history.preview_purge(now).unwrap();
        assert_eq!(purge.sessions, 1);
        history.purge_retained(&purge, now).unwrap();
        let removed = history.curated_dataset().unwrap();
        assert!(removed.cases.is_empty());
        assert_eq!(removed.revision, full.revision + MAX_CASES);
        assert_eq!(
            removed
                .changes
                .iter()
                .filter(|c| c.change == "removed")
                .count(),
            MAX_CASES as usize
        );
        assert!(!serde_json::to_string(&removed)
            .unwrap()
            .contains("CURATED_SELECTED_SOURCE"));
        assert!(history
            .commit_curated(&state, &pending.token, true)
            .is_err());
    }
}
