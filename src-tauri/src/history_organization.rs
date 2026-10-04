//! Private, user-authored organization. Never joined into accounting, summaries,
//! diagnostics, exports or MCP. Record anchors are opaque and contain no content.
use super::*;
use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionAnnotation {
    pub summary: OrganizationSummary,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnnotationEdit {
    pub identity: AnnotationIdentity,
    pub revision: i64,
    pub pinned: bool,
    pub note: String,
    pub tags: Vec<String>,
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
        validate_label(tag)?;
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
        },
        note,
    })
}

impl HistoryStore {
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
            validate_label(tag)?;
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
        validate_label(label)?;
        if let Some(replacement) = replacement {
            validate_label(replacement)?;
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
        }
    }
    fn search() -> SavedSearchDefinition {
        SavedSearchDefinition {
            name: "January review".into(),
            query: "summary phrase".into(),
            scope: "all".into(),
            content_scope: "summary".into(),
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
        let saved = store.edit_annotation(&edit(&initial)).unwrap();
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
        assert_eq!(
            store.get_annotation(&promoted.identity).unwrap().note,
            "PRIVATE_SENTINEL_252"
        );
        let summary_json = serde_json::to_string(&store.session_summaries().unwrap()).unwrap();
        assert!(!summary_json.contains("PRIVATE_SENTINEL_252"));
        assert!(!serde_json::to_string(&promoted)
            .unwrap()
            .contains("PRIVATE_SENTINEL_252"));
        drop(store);
        let store = HistoryStore::open(&db).unwrap();
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
        store.edit_annotation(&edit(&initial)).unwrap();
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
        store.purge_retained(&preview, now).unwrap();
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
        assert!(store.edit_annotation(&edit(&initial)).is_err());
        assert_eq!(store.saved_searches().unwrap()[0].id, saved_search.id);
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
}
