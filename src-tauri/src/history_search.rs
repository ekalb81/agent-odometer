//! Bounded, explicit reads of retained prompt/reply fields. No body index, new
//! snapshot, accounting write, or whole-Session materialization is introduced.
use super::*;
use crate::transcript_search::RetainedMessageField;

pub(crate) const MAX_SNAPSHOT_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const TURN_PAGE_SIZE: usize = 25;

#[derive(Debug)]
pub(crate) struct RetainedSearchInfo {
    pub identity: String,
    pub revision: String,
    pub bytes: u64,
    pub format_supported: bool,
    version: i64,
}

#[derive(Debug)]
pub(crate) struct RetainedTurnMessage {
    pub turn_id: String,
    pub field: RetainedMessageField,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug)]
pub(crate) struct RetainedSearchMessages {
    pub info: RetainedSearchInfo,
    pub messages: Vec<RetainedTurnMessage>,
    pub next_turn: Option<usize>,
}

fn info(connection: &Connection, key: &str) -> Result<RetainedSearchInfo> {
    let (identity, artifact, version, hash, bytes, format):
        (String, Option<String>, i64, String, i64, i64) = connection.query_row(
        "SELECT d.identity_key,
                (SELECT artifact_key FROM source_artifacts WHERE session_key=d.session_key ORDER BY created_at_ms,artifact_key LIMIT 1),
                s.version,s.snapshot_hash,length(s.session_json),s.format_version
         FROM durable_sessions d JOIN session_snapshots s
           ON s.session_key=d.session_key AND s.version=d.current_snapshot_version
         WHERE d.session_key=?1",
        [key],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)),
    )?;
    if identity.len() > 4096
        || artifact.as_ref().is_some_and(|s| s.len() > 4096)
        || hash.len() > 128
    {
        bail!("retained_identity_unavailable");
    }
    let binding = serde_json::to_vec(&(key, identity, artifact))?;
    Ok(RetainedSearchInfo {
        identity: format!("lineage:{:016x}", crate::stable_hash::fnv1a64(&binding)),
        revision: format!("snapshot:{version}:{hash}"),
        bytes: bytes.try_into()?,
        format_supported: format == SNAPSHOT_FORMAT_VERSION,
        version,
    })
}

fn messages(
    connection: &Connection,
    key: &str,
    expected_identity: Option<&str>,
    expected_revision: Option<&str>,
    offset: usize,
) -> Result<RetainedSearchMessages> {
    let info = info(connection, key)?;
    if expected_identity.is_some_and(|id| id != info.identity)
        || expected_revision.is_some_and(|revision| revision != info.revision)
    {
        bail!("retained_target_changed");
    }
    if info.bytes > MAX_SNAPSHOT_BYTES {
        bail!("retained_snapshot_too_large");
    }
    if !info.format_supported {
        bail!("retained_snapshot_unsupported");
    }
    let has_turns: bool = connection.query_row(
        "SELECT json_type(CAST(session_json AS TEXT),'$.turns')='array' FROM session_snapshots WHERE session_key=?1 AND version=?2",
        params![key,info.version], |r| r.get(0),
    )?;
    if !has_turns {
        bail!("retained_messages_unavailable");
    }
    let mut statement = connection.prepare(
        "SELECT CAST(t.key AS INTEGER),json_extract(t.value,'$.turn_id'),
                substr(json_extract(t.value,'$.user_message'),1,500),
                substr(json_extract(t.value,'$.last_agent_message'),1,500),
                COALESCE(length(json_extract(t.value,'$.user_message'))>=500,0),
                COALESCE(length(json_extract(t.value,'$.last_agent_message'))>=500,0)
         FROM session_snapshots s,json_each(CAST(s.session_json AS TEXT),'$.turns') t
         WHERE s.session_key=?1 AND s.version=?2 AND CAST(t.key AS INTEGER)>=?3
         ORDER BY CAST(t.key AS INTEGER) LIMIT 26",
    )?;
    let rows = statement.query_map(params![key, info.version, i64::try_from(offset)?], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, bool>(4)?,
            r.get::<_, bool>(5)?,
        ))
    })?;
    let mut result = Vec::new();
    let mut next_turn = None;
    let started = Instant::now();
    for (count, row) in rows.enumerate() {
        if started.elapsed() > Duration::from_secs(2) {
            bail!("retained_search_deadline");
        }
        let (index, turn_id, prompt, reply, prompt_truncated, reply_truncated) = row?;
        if count == TURN_PAGE_SIZE {
            next_turn = Some(usize::try_from(index)?);
            break;
        }
        if turn_id.is_empty() || turn_id.len() > 1024 {
            bail!("retained_turn_identity_unavailable");
        }
        for (field, text, truncated) in [
            (RetainedMessageField::UserMessage, prompt, prompt_truncated),
            (
                RetainedMessageField::LastAgentMessage,
                reply,
                reply_truncated,
            ),
        ] {
            if let Some(text) = text {
                result.push(RetainedTurnMessage {
                    turn_id: turn_id.clone(),
                    field,
                    text,
                    truncated,
                });
            }
        }
    }
    Ok(RetainedSearchMessages {
        info,
        messages: result,
        next_turn,
    })
}

impl HistoryStore {
    pub(crate) fn retained_search_info(&self, key: &str) -> Result<RetainedSearchInfo> {
        self.exclusion_cache.lock().unwrap().verify()?;
        let reader = self.open_reader()?;
        info(&reader, key)
    }

    pub(crate) fn retained_search_messages(
        &self,
        key: &str,
        identity: Option<&str>,
        revision: Option<&str>,
        offset: usize,
    ) -> Result<RetainedSearchMessages> {
        self.exclusion_cache.lock().unwrap().verify()?;
        // open_reader already starts one read transaction; metadata and message
        // fields must come from the same committed snapshot.
        let reader = self.open_reader()?;
        messages(&reader, key, identity, revision, offset)
    }

    /// Resolve exactly the reviewed snapshot field. Never choose the first or
    /// closest turn when an opaque target has expired or its turn is ambiguous.
    pub(crate) fn retained_search_target(
        &self,
        key: &str,
        identity: &str,
        revision: &str,
        turn_id: &str,
        field: RetainedMessageField,
    ) -> Result<RetainedTurnMessage> {
        self.exclusion_cache.lock().unwrap().verify()?;
        let connection = self.open_reader()?;
        let info = info(&connection, key)?;
        if identity != info.identity || revision != info.revision {
            bail!("retained_target_changed");
        }
        if info.bytes > MAX_SNAPSHOT_BYTES {
            bail!("retained_snapshot_too_large");
        }
        if !info.format_supported {
            bail!("retained_snapshot_unsupported");
        }
        let json_field = match field {
            RetainedMessageField::UserMessage => "$.user_message",
            RetainedMessageField::LastAgentMessage => "$.last_agent_message",
        };
        let mut statement = connection.prepare(
            "SELECT substr(json_extract(t.value,?4),1,500),
                    COALESCE(length(json_extract(t.value,?4))>=500,0)
             FROM session_snapshots s,json_each(CAST(s.session_json AS TEXT),'$.turns') t
             WHERE s.session_key=?1 AND s.version=?2 AND json_extract(t.value,'$.turn_id')=?3 LIMIT 2"
        )?;
        let mut rows = statement.query(params![key, info.version, turn_id, json_field])?;
        let Some(row) = rows.next()? else {
            bail!("retained_target_missing");
        };
        let text: Option<String> = row.get(0)?;
        let truncated = row.get(1)?;
        let Some(text) = text else {
            bail!("retained_target_missing");
        };
        if rows.next()?.is_some() {
            bail!("retained_target_ambiguous");
        }
        Ok(RetainedTurnMessage {
            turn_id: turn_id.into(),
            field,
            text,
            truncated,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TurnInfo;
    use std::fs::File;
    use std::io::Write;

    fn fixture() -> (tempfile::TempDir, HistoryStore, String) {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.jsonl");
        let mut file = File::create(&path).unwrap();
        writeln!(file,r#"{{"type":"session_meta","payload":{{"id":"search","timestamp":"2026-01-01T00:00:00Z"}}}}"#).unwrap();
        let mut session = crate::parser::parse_file(&path, false).unwrap().unwrap();
        session.last_event_at = session.started_at;
        session.turns = (0..30)
            .map(|index| TurnInfo {
                turn_id: format!("turn-{index}"),
                index: index + 1,
                user_message: Some("🦀".repeat(510)),
                last_agent_message: Some(format!("reply {index}")),
                ..Default::default()
            })
            .collect();
        let history = HistoryStore::open(&root.path().join("history.sqlite")).unwrap();
        let key = history.observe(&path, &session, 1).unwrap().key;
        (root, history, key)
    }

    #[test]
    fn retained_pages_bound_turns_and_unicode_fields_and_land_exactly() {
        let (_root, history, key) = fixture();
        let first = history
            .retained_search_messages(&key, None, None, 0)
            .unwrap();
        assert_eq!(first.messages.len(), 50);
        assert_eq!(first.next_turn, Some(25));
        assert_eq!(first.messages[0].text.chars().count(), 500);
        assert!(first.messages[0].truncated);
        let second = history
            .retained_search_messages(
                &key,
                Some(&first.info.identity),
                Some(&first.info.revision),
                25,
            )
            .unwrap();
        assert_eq!(second.messages.len(), 10);
        assert!(second.next_turn.is_none());
        let exact = history
            .retained_search_target(
                &key,
                &first.info.identity,
                &first.info.revision,
                "turn-29",
                RetainedMessageField::LastAgentMessage,
            )
            .unwrap();
        assert_eq!(exact.text, "reply 29");
        assert!(history
            .retained_search_target(
                &key,
                &first.info.identity,
                &first.info.revision,
                "missing",
                RetainedMessageField::UserMessage
            )
            .is_err());
    }

    #[test]
    fn stale_oversized_and_ambiguous_snapshots_cannot_redirect_targets() {
        let (_root, history, key) = fixture();
        let before = history.retained_search_info(&key).unwrap();
        history
            .connection()
            .unwrap()
            .execute(
                "UPDATE session_snapshots SET snapshot_hash='changed' WHERE session_key=?1",
                [&key],
            )
            .unwrap();
        assert!(history
            .retained_search_messages(&key, Some(&before.identity), Some(&before.revision), 0)
            .unwrap_err()
            .to_string()
            .contains("retained_target_changed"));
        let current = history.retained_search_info(&key).unwrap();
        history.connection().unwrap().execute("UPDATE session_snapshots SET session_json=?1 WHERE session_key=?2",params![br#"{"turns":[{"turn_id":"duplicate","user_message":"one"},{"turn_id":"duplicate","user_message":"two"}],"tokens_history":"not a Session"}"#.as_slice(),key]).unwrap();
        assert!(history
            .retained_search_target(
                &key,
                &current.identity,
                &current.revision,
                "duplicate",
                RetainedMessageField::UserMessage
            )
            .unwrap_err()
            .to_string()
            .contains("ambiguous"));
        // No whole-Session decode: invalid accounting fields do not affect explicit message reads.
        assert_eq!(
            history
                .retained_search_messages(&key, None, None, 0)
                .unwrap()
                .messages
                .len(),
            2
        );
        history
            .connection()
            .unwrap()
            .execute(
                "UPDATE session_snapshots SET session_json=zeroblob(?1) WHERE session_key=?2",
                params![MAX_SNAPSHOT_BYTES as i64 + 1, key],
            )
            .unwrap();
        assert!(history
            .retained_search_messages(&key, None, None, 0)
            .unwrap_err()
            .to_string()
            .contains("too_large"));
    }

    #[test]
    fn confirmed_purge_invalidates_retained_lineage_targets() {
        let (root, history, key) = fixture();
        let info = history.retained_search_info(&key).unwrap();
        history
            .mark_path_missing(&root.path().join("source.jsonl"))
            .unwrap();
        history
            .set_retention_policy(&crate::history_store::RetentionPolicy {
                retained_days: Some(1),
            })
            .unwrap();
        let now = "2026-10-04T00:00:00Z".parse().unwrap();
        let preview = history.preview_purge(now).unwrap();
        assert_eq!(
            history.purge_retained(&preview, now).unwrap().removed_keys,
            vec![key.clone()]
        );
        assert!(history
            .retained_search_target(
                &key,
                &info.identity,
                &info.revision,
                "turn-0",
                RetainedMessageField::UserMessage
            )
            .is_err());
    }
}
