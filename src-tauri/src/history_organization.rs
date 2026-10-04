//! Private, user-authored organization. Never joined into accounting, summaries,
//! diagnostics, ordinary exports or MCP. An explicit human-outcome export uses
//! structured fields only. Record anchors are opaque and contain no content.
use super::*;
use serde::{Deserialize, Serialize};

/// Explicit human input. Never derived from Git, pricing, timing or tokens.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HumanOutcome {
    pub label: String,
    pub repair_minutes: Option<u32>,
    pub first_pass_accepted: Option<bool>,
}
impl Default for HumanOutcome {
    fn default() -> Self {
        Self { label: "not_rated".into(), repair_minutes: None, first_pass_accepted: None }
    }
}
impl HumanOutcome {
    fn validate(&self) -> Result<()> {
        if !["not_rated", "accepted", "rejected", "unresolved"].contains(&self.label.as_str())
            || self.repair_minutes.is_some_and(|minutes| minutes > 525_600)
            || (self.first_pass_accepted == Some(true) && self.label != "accepted")
            || (self.first_pass_accepted.is_some() && self.label == "not_rated") {
            bail!("Use an explicit outcome, whole repair minutes up to 525600, and a first-pass report only for a rated task");
        }
        Ok(())
    }
}

pub(super) fn install_outcome_schema(tx: &Transaction<'_>) -> Result<()> {
    if !table_has_column(tx, "session_annotations", "outcome_label")? {
        // NULL means no explicit human outcome edit. During recovery it must
        // remain unknown even if the user subsequently edits a pin or note.
        tx.execute_batch("ALTER TABLE session_annotations ADD COLUMN outcome_label TEXT CHECK(outcome_label IS NULL OR outcome_label IN ('not_rated','accepted','rejected','unresolved'));
            ALTER TABLE session_annotations ADD COLUMN repair_minutes INTEGER CHECK(repair_minutes IS NULL OR repair_minutes BETWEEN 0 AND 525600);
            ALTER TABLE session_annotations ADD COLUMN first_pass_accepted INTEGER CHECK(first_pass_accepted IS NULL OR first_pass_accepted IN (0,1));")?;
    }
    Ok(())
}

fn read_outcome(conn: &Connection, identity: &AnnotationIdentity) -> Result<Option<HumanOutcome>> {
    let outcome = conn.query_row("SELECT outcome_label,repair_minutes,first_pass_accepted FROM session_annotations WHERE session_key=?1 AND anchor=?2",
        params![identity.session_key, identity.anchor], |r| {
            let label: Option<String> = r.get(0)?;
            let repair_minutes = r.get(1)?;
            let first_pass_accepted = r.get(2)?;
            Ok(label.map(|label| HumanOutcome { label, repair_minutes, first_pass_accepted }))
        }).optional()?.flatten();
    if let Some(outcome) = &outcome { outcome.validate()?; }
    Ok(outcome)
}

pub(super) fn install_schema(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS durable_sessions_organization_identity_idx ON durable_sessions(session_key,first_event_fingerprint);
         CREATE TABLE IF NOT EXISTS session_annotations (
           session_key TEXT NOT NULL, first_event_fingerprint TEXT NOT NULL,
           anchor TEXT NOT NULL DEFAULT '', revision INTEGER NOT NULL DEFAULT 0,
           pinned INTEGER NOT NULL DEFAULT 0, note TEXT NOT NULL DEFAULT '',
           PRIMARY KEY(session_key,anchor),
           FOREIGN KEY(session_key,first_event_fingerprint) REFERENCES durable_sessions(session_key,first_event_fingerprint) ON UPDATE CASCADE ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS organization_tags (label TEXT PRIMARY KEY);
         CREATE TABLE IF NOT EXISTS annotation_tags (
           session_key TEXT NOT NULL, anchor TEXT NOT NULL, label TEXT NOT NULL REFERENCES organization_tags(label) ON UPDATE CASCADE ON DELETE CASCADE,
           PRIMARY KEY(session_key,anchor,label),
           FOREIGN KEY(session_key,anchor) REFERENCES session_annotations(session_key,anchor) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS saved_searches (id INTEGER PRIMARY KEY AUTOINCREMENT, revision INTEGER NOT NULL, definition_json TEXT NOT NULL);",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotationIdentity {
    pub session_key: String,
    pub fingerprint: String,
    /// Empty means the session; nonempty is a validated source-record anchor.
    #[serde(default)]
    pub anchor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationSummary {
    pub identity: AnnotationIdentity,
    pub revision: i64,
    pub pinned: bool,
    pub has_note: bool,
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<HumanOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAnnotation {
    pub summary: OrganizationSummary,
    pub note: String,
    #[serde(default)]
    pub recovery_backup_unrestored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationEdit {
    pub identity: AnnotationIdentity,
    pub revision: i64,
    pub pinned: bool,
    pub note: String,
    pub tags: Vec<String>,
    /// Missing from older editors means preserve the current human outcome.
    #[serde(default)]
    pub outcome: Option<HumanOutcome>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordBookmark {
    pub identity: AnnotationIdentity,
    pub revision: i64,
    pub bookmarked: bool,
}

#[derive(Debug, Serialize)]
pub struct RecordBookmarkList {
    pub identity: AnnotationIdentity,
    pub bookmarks: Vec<RecordBookmark>,
    pub recovery_backup_unrestored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SavedSearchDefinition {
    pub name: String,
    pub query: String,
    pub scope: String,
    /// `summary` searches the current summary fields. `session_content` requires
    /// the explicit bounded transcript search service; never silently downgraded.
    pub content_scope: String,
    #[serde(default)]
    pub content_classes: crate::transcript_search::ContentScope,
    pub session_key: Option<String>,
    pub fingerprint: Option<String>,
    pub from: Option<String>,
    pub to: Option<String>,
    pub model: String,
    pub show_active: bool,
    pub show_archived: bool,
    pub show_subagents: bool,
    pub pinned_only: bool,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSearch {
    pub id: i64,
    pub revision: i64,
    pub definition: SavedSearchDefinition,
}

fn validate_label(value: &str) -> Result<()> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > 80
        || value.chars().any(char::is_control)
    {
        bail!("Use a nonempty label of at most 80 bytes without control characters");
    }
    Ok(())
}

fn changed_annotations(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("INSERT INTO history_meta(key,value) VALUES('organization_revision','1') ON CONFLICT(key) DO UPDATE SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT)", [])?;
    Ok(())
}

fn validate_tag(value: &str) -> Result<()> {
    validate_label(value)?;
    if value.contains(',') {
        bail!("Tag labels cannot contain commas");
    }
    Ok(())
}

fn validate_identity(identity: &AnnotationIdentity) -> Result<()> {
    if identity.session_key.is_empty()
        || identity.session_key.len() > 1024
        || identity.fingerprint.len() > 1024
        || identity.anchor.len() > 1024
    {
        bail!("Invalid organization target");
    }
    Ok(())
}

fn validate_search(value: &SavedSearchDefinition) -> Result<()> {
    validate_label(&value.name)?;
    if value.query.len() > 4096
        || value.model.len() > 256
        || value.tags.len() > 32
        || value.scope.is_empty()
        || value.scope.len() > 80
        || !value
            .scope
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        || !["summary", "session_content"].contains(&value.content_scope.as_str())
    {
        bail!("Invalid saved search");
    }
    if value.content_scope == "session_content"
        && (value.session_key.is_none() || value.fingerprint.is_none())
    {
        bail!("Content searches require a specific session identity");
    }
    for tag in &value.tags {
        validate_tag(tag)?;
    }
    let mut bounds = Vec::new();
    for date in [&value.from, &value.to] {
        bounds.push(
            date.as_deref()
                .map(chrono::DateTime::parse_from_rfc3339)
                .transpose()
                .map_err(|_| anyhow!("Invalid saved UTC date bound"))?,
        );
    }
    if matches!((&bounds[0], &bounds[1]), (Some(from), Some(to)) if from > to) {
        bail!("Saved date bounds are reversed");
    }
    if value.session_key.as_ref().is_some_and(|v| v.len() > 1024)
        || value.fingerprint.as_ref().is_some_and(|v| v.len() > 1024)
    {
        bail!("Invalid saved session identity");
    }
    Ok(())
}

fn read_annotation(conn: &Connection, identity: &AnnotationIdentity) -> Result<SessionAnnotation> {
    let fingerprint: String = conn
        .query_row(
            "SELECT first_event_fingerprint FROM durable_sessions WHERE session_key=?1",
            [&identity.session_key],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| anyhow!("Session no longer exists"))?;
    if fingerprint != identity.fingerprint {
        bail!("Session identity changed; reload organization before editing");
    }
    let (revision, pinned, note): (i64, bool, String) = conn.query_row("SELECT revision,pinned,note FROM session_annotations WHERE session_key=?1 AND anchor=?2", params![identity.session_key,identity.anchor], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.unwrap_or_default();
    let mut statement = conn.prepare(
        "SELECT label FROM annotation_tags WHERE session_key=?1 AND anchor=?2 ORDER BY label",
    )?;
    let tags = statement
        .query_map(params![identity.session_key, identity.anchor], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(SessionAnnotation {
        summary: OrganizationSummary {
            identity: identity.clone(),
            revision,
            pinned,
            has_note: !note.is_empty(),
            tags,
            outcome: read_outcome(conn, identity)?,
        },
        note,
        recovery_backup_unrestored: conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM history_meta WHERE key='recovered_at_ms')",
            [],
            |r| r.get(0),
        )?,
    })
}

impl HistoryStore {
    pub fn record_bookmarks(&self, key: &str) -> Result<RecordBookmarkList> {
        if key.is_empty() || key.len() > 1024 {
            bail!("Invalid bookmark session");
        }
        let recovery_backup_unrestored = self.organization_recovery_pending()?;
        let conn = self.open_reader()?;
        let fingerprint = conn.query_row(
            "SELECT first_event_fingerprint FROM durable_sessions WHERE session_key=?1",
            [key],
            |r| r.get(0),
        )?;
        let identity = AnnotationIdentity {
            session_key: key.into(),
            fingerprint,
            anchor: String::new(),
        };
        let mut statement = conn.prepare("SELECT anchor,revision,pinned FROM session_annotations WHERE session_key=?1 AND anchor<>'' ORDER BY anchor LIMIT 501")?;
        let bookmarks = statement
            .query_map([key], |r| {
                Ok(RecordBookmark {
                    identity: AnnotationIdentity {
                        anchor: r.get(0)?,
                        ..identity.clone()
                    },
                    revision: r.get(1)?,
                    bookmarked: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if bookmarks.len() > 500 {
            bail!("At most 500 record bookmarks are supported per session");
        }
        Ok(RecordBookmarkList {
            identity,
            bookmarks,
            recovery_backup_unrestored,
        })
    }

    /// Only the desktop source-validation boundary may add a record bookmark.
    pub(crate) fn edit_record_bookmark(&self, edit: &RecordBookmark) -> Result<RecordBookmark> {
        validate_identity(&edit.identity)?;
        if edit.identity.anchor.is_empty() || edit.revision < 0 {
            bail!("Invalid record bookmark");
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_annotation(&tx, &edit.identity)?;
        if current.summary.revision != edit.revision {
            bail!("Bookmark changed; reload before saving");
        }
        if !edit.bookmarked && current.summary.revision == 0 {
            bail!("Bookmark no longer exists; reload bookmarks");
        }
        let revision = edit
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("Bookmark revision exhausted"))?;
        tx.execute("INSERT INTO session_annotations(session_key,first_event_fingerprint,anchor,revision,pinned) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(session_key,anchor) DO UPDATE SET revision=excluded.revision,pinned=excluded.pinned", params![edit.identity.session_key,edit.identity.fingerprint,edit.identity.anchor,revision,edit.bookmarked])?;
        let active: i64 = tx.query_row(
            "SELECT count(*) FROM session_annotations WHERE session_key=?1 AND anchor<>''",
            [&edit.identity.session_key],
            |r| r.get(0),
        )?;
        if active > 500 {
            bail!("At most 500 record bookmarks are supported per session");
        }
        changed_annotations(&tx)?;
        tx.commit()?;
        Ok(RecordBookmark {
            revision,
            ..edit.clone()
        })
    }

    pub fn organization_recovery_pending(&self) -> Result<bool> {
        let marker = self.recovery_receipt()?.is_some();
        let recorded: bool = self.open_reader()?.query_row(
            "SELECT EXISTS(SELECT 1 FROM history_meta WHERE key='recovered_at_ms')",
            [],
            |r| r.get(0),
        )?;
        Ok(marker || recorded)
    }
    /// Bounded session metadata projection: no private note content or snapshots.
    pub fn organization_summaries(&self, keys: &[String]) -> Result<Vec<OrganizationSummary>> {
        if keys.len() > 10_000 || keys.iter().any(|k| k.len() > 1024) {
            bail!("Organization batch is too large");
        }
        let conn = self.open_reader()?;
        let mut output = Vec::new();
        for key in keys {
            let identity = conn
                .query_row(
                    "SELECT first_event_fingerprint FROM durable_sessions WHERE session_key=?1",
                    [key],
                    |r| r.get::<_, String>(0),
                )
                .optional()?;
            let Some(fingerprint) = identity else {
                continue;
            };
            let identity = AnnotationIdentity {
                session_key: key.clone(),
                fingerprint,
                anchor: String::new(),
            };
            // Do not load note bodies for list/filter operations.
            let (revision,pinned,has_note) = conn.query_row("SELECT revision,pinned,length(note)>0 FROM session_annotations WHERE session_key=?1 AND anchor=''", [key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?.unwrap_or((0,false,false));
            let mut statement = conn.prepare("SELECT label FROM annotation_tags WHERE session_key=?1 AND anchor='' ORDER BY label")?;
            let tags = statement
                .query_map([key], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            output.push(OrganizationSummary {
                outcome: read_outcome(&conn, &identity)?,
                identity,
                revision,
                pinned,
                has_note,
                tags,
            });
        }
        Ok(output)
    }

    pub fn get_annotation(&self, identity: &AnnotationIdentity) -> Result<SessionAnnotation> {
        validate_identity(identity)?;
        self.recovery_receipt()?;
        {
            let conn = self.open_reader()?;
            read_annotation(&conn, identity)
        }
    }

    pub fn edit_annotation(&self, edit: &AnnotationEdit) -> Result<SessionAnnotation> {
        validate_identity(&edit.identity)?;
        if edit.revision < 0 {
            bail!("Invalid organization revision");
        }
        if edit.note.len() > 32_768 || edit.tags.len() > 32 {
            bail!("Note or tag selection is too large");
        }
        for tag in &edit.tags {
            validate_tag(tag)?;
        }
        if let Some(outcome) = &edit.outcome {
            outcome.validate()?;
        }
        // Record bookmark writes are enabled only through the inspector's anchor
        // validation API. A generic organization edit cannot fabricate one.
        if !edit.identity.anchor.is_empty() {
            bail!("Record bookmarks require source-anchor validation");
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = read_annotation(&tx, &edit.identity)?;
        if current.summary.revision != edit.revision {
            bail!("Organization changed; reload before saving");
        }
        let revision = edit
            .revision
            .checked_add(1)
            .filter(|v| *v >= 1)
            .ok_or_else(|| anyhow!("Organization revision exhausted"))?;
        tx.execute("INSERT INTO session_annotations(session_key,first_event_fingerprint,anchor,revision,pinned,note) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(session_key,anchor) DO UPDATE SET revision=excluded.revision,pinned=excluded.pinned,note=excluded.note",params![edit.identity.session_key,edit.identity.fingerprint,edit.identity.anchor,revision,edit.pinned,edit.note])?;
        if let Some(outcome) = &edit.outcome {
            tx.execute("UPDATE session_annotations SET outcome_label=?2,repair_minutes=?3,first_pass_accepted=?4 WHERE session_key=?1 AND anchor=''",
                params![edit.identity.session_key, outcome.label, outcome.repair_minutes, outcome.first_pass_accepted])?;
        }
        tx.execute(
            "DELETE FROM annotation_tags WHERE session_key=?1 AND anchor=?2",
            params![edit.identity.session_key, edit.identity.anchor],
        )?;
        for tag in &edit.tags {
            tx.execute(
                "INSERT OR IGNORE INTO organization_tags(label) VALUES(?1)",
                [tag],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO annotation_tags(session_key,anchor,label) VALUES(?1,?2,?3)",
                params![edit.identity.session_key, edit.identity.anchor, tag],
            )?;
        }
        let tag_count: i64 =
            tx.query_row("SELECT count(*) FROM organization_tags", [], |r| r.get(0))?;
        if tag_count > 1000 {
            bail!("At most 1000 tag labels are supported");
        }
        let result = read_annotation(&tx, &edit.identity)?;
        changed_annotations(&tx)?;
        tx.commit()?;
        Ok(result)
    }

    pub fn organization_tags(&self) -> Result<Vec<String>> {
        let conn = self.open_reader()?;
        let mut statement =
            conn.prepare("SELECT label FROM organization_tags ORDER BY label LIMIT 1001")?;
        let tags = statement
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        if tags.len() > 1000 {
            bail!("Too many tags to display; remove unused tags");
        }
        Ok(tags)
    }

    pub fn change_organization_tag(&self, label: &str, replacement: Option<&str>) -> Result<()> {
        validate_tag(label)?;
        if let Some(replacement) = replacement {
            validate_tag(replacement)?;
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Rename/delete also invalidates open editors; a stale save must not
        // resurrect a deleted tag or overwrite a concurrent tag selection.
        tx.execute("UPDATE session_annotations SET revision=revision+1 WHERE EXISTS(SELECT 1 FROM annotation_tags t WHERE t.session_key=session_annotations.session_key AND t.anchor=session_annotations.anchor AND t.label=?1)",[label])?;
        if let Some(replacement) = replacement {
            if replacement != label {
                tx.execute(
                    "UPDATE organization_tags SET label=?2 WHERE label=?1",
                    params![label, replacement],
                )?;
            }
        } else {
            tx.execute("DELETE FROM organization_tags WHERE label=?1", [label])?;
        }
        changed_annotations(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn saved_searches(&self) -> Result<Vec<SavedSearch>> {
        let conn = self.open_reader()?;
        let mut statement = conn.prepare(
            "SELECT id,revision,definition_json FROM saved_searches ORDER BY id LIMIT 201",
        )?;
        let rows = statement.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut output = Vec::new();
        for row in rows {
            let (id, revision, json) = row?;
            output.push(SavedSearch {
                id,
                revision,
                definition: serde_json::from_str(&json)?,
            });
        }
        if output.len() > 200 {
            bail!("Too many saved searches");
        }
        Ok(output)
    }

    pub fn save_search(
        &self,
        id: Option<i64>,
        revision: i64,
        definition: &SavedSearchDefinition,
    ) -> Result<SavedSearch> {
        validate_search(definition)?;
        if revision < 0 {
            bail!("Invalid saved search revision");
        }
        let next = revision
            .checked_add(1)
            .filter(|v| *v >= 1)
            .ok_or_else(|| anyhow!("Saved search revision exhausted"))?;
        let json = serde_json::to_string(definition)?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = if let Some(id) = id {
            if tx.execute("UPDATE saved_searches SET revision=?3,definition_json=?4 WHERE id=?1 AND revision=?2",params![id,revision,next,json])? != 1 { bail!("Saved search changed; reload before editing"); }
            id
        } else {
            if revision != 0 {
                bail!("Invalid new search revision");
            }
            let count: i64 =
                tx.query_row("SELECT count(*) FROM saved_searches", [], |r| r.get(0))?;
            if count >= 200 {
                bail!("At most 200 saved searches are supported");
            }
            tx.execute(
                "INSERT INTO saved_searches(revision,definition_json) VALUES(?1,?2)",
                params![next, json],
            )?;
            tx.last_insert_rowid()
        };
        tx.commit()?;
        Ok(SavedSearch {
            id,
            revision: next,
            definition: definition.clone(),
        })
    }

    pub fn delete_saved_search(&self, id: i64, revision: i64) -> Result<()> {
        if self.connection()?.execute(
            "DELETE FROM saved_searches WHERE id=?1 AND revision=?2",
            params![id, revision],
        )? != 1
        {
            bail!("Saved search changed; reload before deleting");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(id: &str) -> Session {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "{{\"type\":\"session_meta\",\"payload\":{{\"id\":\"{id}\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}}}").unwrap();
        {
            let mut session = crate::parser::parse_file(file.path(), false)
                .unwrap()
                .unwrap();
            session.last_event_at = session.started_at;
            session
        }
    }
    fn edit(summary: &OrganizationSummary) -> AnnotationEdit {
        AnnotationEdit {
            identity: summary.identity.clone(),
            revision: summary.revision,
            pinned: true,
            note: "PRIVATE_SENTINEL_252".into(),
            tags: vec!["Review".into()],
            outcome: None,
        }
    }
    fn search() -> SavedSearchDefinition {
        SavedSearchDefinition {
            name: "January review".into(),
            query: "summary phrase".into(),
            scope: "all".into(),
            content_scope: "summary".into(),
            content_classes: Default::default(),
            session_key: None,
            fingerprint: None,
            from: Some("2026-01-01T00:00:00Z".into()),
            to: Some("2026-01-31T23:59:59Z".into()),
            model: "gpt-test".into(),
            show_active: true,
            show_archived: false,
            show_subagents: false,
            pinned_only: true,
            tags: vec!["Review".into()],
        }
    }

    #[test]
    fn restart_refresh_promotion_keep_private_data_and_reject_stale_edits() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("history.sqlite");
        let path = root.path().join("source.jsonl");
        let store = HistoryStore::open(&db).unwrap();
        let mut session = fixture("organization");
        let key = store.observe(&path, &session, 1).unwrap().key;
        let initial = store
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        let mut rated = edit(&initial);
        rated.outcome = Some(HumanOutcome { label: "accepted".into(), repair_minutes: Some(0), first_pass_accepted: Some(true) });
        let saved = store.edit_annotation(&rated).unwrap();
        assert!(store.edit_annotation(&edit(&initial)).is_err());
        // First real usage promotes the provisional fingerprint in-place.
        session.tokens_history.push(TokenHistoryPoint {
            timestamp: session.started_at,
            model: Some("gpt-test".into()),
            service_tier: None,
            request_input_tokens: Some(3),
            total_tokens: 3,
            delta: TokenTotals {
                input_tokens: 3,
                total_tokens: 3,
                ..Default::default()
            },
        });
        session.tokens_total = session.tokens_history[0].delta.clone();
        assert_eq!(store.observe(&path, &session, 2).unwrap().key, key);
        let promoted = store
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        assert_ne!(promoted.identity.fingerprint, initial.identity.fingerprint);
        assert!(store.get_annotation(&saved.summary.identity).is_err());
        assert!(promoted.pinned && promoted.has_note);
        assert_eq!(promoted.outcome, rated.outcome.clone());
        // Renaming/relinking the source and changing its displayed title do not
        // turn the human assertion into a label for a different task.
        session.thread_name = Some("Renamed synthetic task".into());
        assert_eq!(store.observe(&root.path().join("renamed-source.jsonl"), &session, 3).unwrap().key, key);
        assert_eq!(store.get_annotation(&promoted.identity).unwrap().summary.outcome, promoted.outcome);
        assert_eq!(
            store.get_annotation(&promoted.identity).unwrap().note,
            "PRIVATE_SENTINEL_252"
        );
        let summary_json = serde_json::to_string(&store.session_summaries().unwrap()).unwrap();
        assert!(!summary_json.contains("PRIVATE_SENTINEL_252"));
        assert!(!summary_json.contains("repair_minutes"));
        assert!(!summary_json.contains("first_pass_accepted"));
        assert!(!serde_json::to_string(&promoted)
            .unwrap()
            .contains("PRIVATE_SENTINEL_252"));
        drop(store);
        let store = HistoryStore::open(&db).unwrap();
        assert_eq!(store.get_annotation(&promoted.identity).unwrap().summary.outcome, rated.outcome);
        assert_eq!(
            store.get_annotation(&promoted.identity).unwrap().note,
            "PRIVATE_SENTINEL_252"
        );
        store
            .change_organization_tag("Review", Some("Follow up"))
            .unwrap();
        let renamed = store.get_annotation(&promoted.identity).unwrap();
        assert_eq!(renamed.summary.tags, ["Follow up"]);
        assert!(store.edit_annotation(&edit(&promoted)).is_err());
        store.change_organization_tag("Follow up", None).unwrap();
        let latest = store.get_annotation(&promoted.identity).unwrap();
        let mut cleared = edit(&latest.summary);
        cleared.note.clear();
        cleared.pinned = false;
        cleared.tags.clear();
        let cleared = store.edit_annotation(&cleared).unwrap();
        assert!(
            !cleared.summary.has_note && !cleared.summary.pinned && cleared.summary.tags.is_empty()
        );
        // Older annotation editors omit the field and must not erase a rating.
        assert_eq!(cleared.summary.outcome.as_ref().unwrap().label, "accepted");
        assert_eq!(cleared.summary.outcome.as_ref().unwrap().repair_minutes, Some(0));
    }

    #[test]
    fn confirmed_purge_removes_annotations_and_reused_key_cannot_inherit_them() {
        let root = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(&root.path().join("history.sqlite")).unwrap();
        let path = root.path().join("source.jsonl");
        let session = fixture("purge-organization");
        let key = store.observe(&path, &session, 1).unwrap().key;
        let initial = store
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        let mut rated = edit(&initial);
        rated.outcome = Some(HumanOutcome { label: "rejected".into(), repair_minutes: Some(15), first_pass_accepted: Some(false) });
        store.edit_annotation(&rated).unwrap();
        store
            .edit_record_bookmark(&RecordBookmark {
                identity: AnnotationIdentity {
                    anchor: "synthetic-record-anchor".into(),
                    ..initial.identity.clone()
                },
                revision: 0,
                bookmarked: true,
            })
            .unwrap();
        let saved_search = store.save_search(None, 0, &search()).unwrap();
        store.mark_path_missing(&path).unwrap();
        store
            .set_retention_policy(&RetentionPolicy {
                retained_days: Some(1),
            })
            .unwrap();
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-04T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let preview = store.preview_purge(now).unwrap();
        assert_eq!(preview.sessions, 1);
        let latest = store.get_annotation(&initial.identity).unwrap();
        let mut new_note = edit(&latest.summary);
        new_note.note = "Edited after purge preview".into();
        store.edit_annotation(&new_note).unwrap();
        assert!(store.purge_retained(&preview, now).is_err());
        assert_eq!(
            store.get_annotation(&initial.identity).unwrap().note,
            "Edited after purge preview"
        );
        let refreshed = store.preview_purge(now).unwrap();
        store.purge_retained(&refreshed, now).unwrap();
        assert!(store.get_annotation(&initial.identity).is_err());
        assert_eq!(
            store
                .connection()
                .unwrap()
                .query_row("SELECT count(*) FROM session_annotations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let mut new_lineage = session;
        new_lineage.started_at += chrono::Duration::days(1);
        let new_key = store.observe(&path, &new_lineage, 2).unwrap().key;
        let new_summary = store.organization_summaries(&[new_key]).unwrap().remove(0);
        assert!(!new_summary.has_note && !new_summary.pinned && new_summary.tags.is_empty());
        assert!(new_summary.outcome.is_none());
        assert!(store.edit_annotation(&edit(&initial)).is_err());
        assert_eq!(store.saved_searches().unwrap()[0].id, saved_search.id);
    }

    #[test]
    fn outcomes_are_explicit_bounded_atomic_and_never_guessed_from_other_annotations() {
        let root = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(&root.path().join("history.sqlite")).unwrap();
        let key = store.observe(&root.path().join("source.jsonl"), &fixture("human-outcome"), 1).unwrap().key;
        let initial = store.organization_summaries(std::slice::from_ref(&key)).unwrap().remove(0);
        assert!(initial.outcome.is_none());
        for outcome in [
            HumanOutcome { label: "git_retained".into(), ..Default::default() },
            HumanOutcome { label: "accepted".into(), repair_minutes: Some(525_601), first_pass_accepted: None },
            HumanOutcome { label: "rejected".into(), first_pass_accepted: Some(true), ..Default::default() },
            HumanOutcome { first_pass_accepted: Some(false), ..Default::default() },
        ] {
            let mut invalid = edit(&initial);
            invalid.outcome = Some(outcome);
            assert!(store.edit_annotation(&invalid).is_err());
            let unchanged = store.get_annotation(&initial.identity).unwrap();
            assert_eq!(unchanged.summary.revision, 0);
            assert!(!unchanged.summary.pinned && unchanged.note.is_empty());
        }
        let mut unresolved = edit(&initial);
        unresolved.outcome = Some(HumanOutcome { label: "unresolved".into(), repair_minutes: Some(5), first_pass_accepted: None });
        let saved = store.edit_annotation(&unresolved).unwrap();
        assert!(store.edit_annotation(&unresolved).is_err());
        let mut clear = edit(&saved.summary);
        clear.outcome = Some(HumanOutcome::default());
        let cleared = store.edit_annotation(&clear).unwrap();
        assert_eq!(cleared.summary.outcome, Some(HumanOutcome::default()));
        assert!(cleared.summary.pinned && cleared.summary.has_note);
        let mut payload = serde_json::to_value(&clear).unwrap();
        payload["outcome"] = serde_json::json!({"label":"accepted","repair_minutes":-1,"first_pass_accepted":null});
        assert!(serde_json::from_value::<AnnotationEdit>(payload).is_err());
        let mut old_payload = serde_json::to_value(&clear).unwrap();
        old_payload.as_object_mut().unwrap().remove("outcome");
        assert!(serde_json::from_value::<AnnotationEdit>(old_payload).unwrap().outcome.is_none());
    }

    #[test]
    fn record_bookmarks_are_private_revisioned_and_persist_without_content() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("bookmarks.sqlite");
        let store = HistoryStore::open(&db).unwrap();
        let key = store
            .observe(
                &root.path().join("synthetic.jsonl"),
                &fixture("bookmarks"),
                1,
            )
            .unwrap()
            .key;
        let list = store.record_bookmarks(&key).unwrap();
        let bookmark = RecordBookmark {
            identity: AnnotationIdentity {
                anchor: "opaque-source:0:hash".into(),
                ..list.identity
            },
            revision: 0,
            bookmarked: true,
        };
        let saved = store.edit_record_bookmark(&bookmark).unwrap();
        assert!(store.edit_record_bookmark(&bookmark).is_err());
        assert!(
            !store
                .organization_summaries(std::slice::from_ref(&key))
                .unwrap()[0]
                .pinned
        );
        let summary = serde_json::to_string(&store.session_summaries().unwrap()).unwrap();
        assert!(!summary.contains("opaque-source"));
        assert!(store
            .edit_annotation(&AnnotationEdit {
                identity: saved.identity.clone(),
                revision: 1,
                pinned: true,
                note: "No record bodies".into(),
                tags: vec![],
                outcome: None,
            })
            .is_err());
        drop(store);
        let store = HistoryStore::open(&db).unwrap();
        let restored = store.record_bookmarks(&key).unwrap().bookmarks.remove(0);
        assert_eq!(restored.identity, saved.identity);
        assert_eq!(restored.revision, 1);
        assert!(restored.bookmarked);
        let removed = store
            .edit_record_bookmark(&RecordBookmark {
                bookmarked: false,
                ..restored
            })
            .unwrap();
        assert!(!store.record_bookmarks(&key).unwrap().bookmarks[0].bookmarked);
        assert!(store.edit_record_bookmark(&saved).is_err());
        assert!(
            store
                .edit_record_bookmark(&RecordBookmark {
                    bookmarked: true,
                    ..removed
                })
                .unwrap()
                .bookmarked
        );
        let retained: (String,i64) = store.connection().unwrap().query_row("SELECT note,(SELECT count(*) FROM annotation_tags) FROM session_annotations WHERE anchor<>''", [], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(retained, (String::new(), 0));
    }

    #[test]
    fn record_bookmark_limit_is_transactional_and_removals_cannot_fabricate_anchors() {
        let root = tempfile::tempdir().unwrap();
        let store = HistoryStore::open(&root.path().join("bookmarks.sqlite")).unwrap();
        let key = store
            .observe(
                &root.path().join("synthetic.jsonl"),
                &fixture("bookmark-limit"),
                1,
            )
            .unwrap()
            .key;
        let identity = store.record_bookmarks(&key).unwrap().identity;
        let extra = RecordBookmark {
            identity: AnnotationIdentity {
                anchor: "extra-anchor".into(),
                ..identity.clone()
            },
            revision: 0,
            bookmarked: false,
        };
        assert!(store.edit_record_bookmark(&extra).is_err());
        assert!(store.record_bookmarks(&key).unwrap().bookmarks.is_empty());
        store.connection().unwrap().execute("WITH RECURSIVE n(v) AS (SELECT 1 UNION ALL SELECT v+1 FROM n WHERE v<500) INSERT INTO session_annotations(session_key,first_event_fingerprint,anchor,revision,pinned) SELECT ?1,?2,'synthetic-'||v,1,0 FROM n", params![key,identity.fingerprint]).unwrap();
        let error = store
            .edit_record_bookmark(&RecordBookmark {
                bookmarked: true,
                ..extra
            })
            .unwrap_err();
        assert!(error.to_string().contains("500"));
        assert_eq!(store.record_bookmarks(&key).unwrap().bookmarks.len(), 500);
    }

    #[test]
    fn saved_searches_persist_exact_filters_and_have_revision_guards() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("history.sqlite");
        let store = HistoryStore::open(&db).unwrap();
        let definition = search();
        let saved = store.save_search(None, 0, &definition).unwrap();
        drop(store);
        let store = HistoryStore::open(&db).unwrap();
        assert_eq!(store.saved_searches().unwrap()[0].definition, definition);
        let mut renamed = definition;
        renamed.name = "Renamed search".into();
        let updated = store
            .save_search(Some(saved.id), saved.revision, &renamed)
            .unwrap();
        assert!(store.delete_saved_search(saved.id, saved.revision).is_err());
        store
            .delete_saved_search(updated.id, updated.revision)
            .unwrap();
        assert!(store.saved_searches().unwrap().is_empty());
        let mut invalid = search();
        invalid.content_scope = "session_content".into();
        assert!(store.save_search(None, 0, &invalid).is_err());
        invalid = search();
        invalid.from = Some("bad-date".into());
        assert!(store.save_search(None, 0, &invalid).is_err());
    }

    #[test]
    fn recovery_preserves_private_backup_without_claiming_source_reconstruction_restored_notes() {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("history.sqlite");
        let source = root.path().join("source.jsonl");
        std::fs::write(&source, b"synthetic source remains unchanged\n").unwrap();
        let source_before = std::fs::read(&source).unwrap();
        let store = HistoryStore::open(&db).unwrap();
        let session = fixture("recovery-organization");
        let key = store.observe(&source, &session, 1).unwrap().key;
        let identity = store
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        store.edit_annotation(&edit(&identity)).unwrap();
        assert!(!store.organization_recovery_pending().unwrap());
        drop(store);
        let original = std::fs::read(&db).unwrap();
        let (replacement, receipt) = HistoryStore::recover_unavailable(&db).unwrap();
        let backup = receipt.backup_directory.join("history.sqlite");
        assert_eq!(std::fs::read(&backup).unwrap(), original);
        assert!(replacement.organization_recovery_pending().unwrap());
        let key = replacement.observe(&source, &session, 1).unwrap().key;
        let target = replacement
            .organization_summaries(&[key])
            .unwrap()
            .remove(0);
        let annotation = replacement.get_annotation(&target.identity).unwrap();
        assert!(annotation.note.is_empty() && annotation.recovery_backup_unrestored);
        let preserved = HistoryStore::open_read_only(&backup, QueryControl::default()).unwrap();
        assert_eq!(
            preserved.get_annotation(&identity.identity).unwrap().note,
            "PRIVATE_SENTINEL_252"
        );
        assert_eq!(std::fs::read(&source).unwrap(), source_before);
    }
}
