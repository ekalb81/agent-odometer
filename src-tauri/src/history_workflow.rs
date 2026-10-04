//! Bounded private projections for workflow evidence; no prompt or output fields.
use super::*;
use crate::workflow::{WorkflowSnapshot, WorkflowSource};
use std::collections::HashSet;

impl HistoryStore {
    /// Own one validated read transaction for a compound desktop workflow query.
    pub(crate) fn workflow_reader(&self) -> Result<Self> {
        Self::open_read_only(&self.path, QueryControl::default())
    }

    pub(crate) fn stream_workflow_snapshots(
        &self,
        session_keys: &HashSet<String>,
        mut visit: impl FnMut(&str, Option<&str>, WorkflowSource) -> Result<()>,
    ) -> Result<()> {
        const PER_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;
        const TOTAL_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
        let connection = self.open_reader()?;
        self.check_session_count(&connection)?;
        let overrides =
            load_project_overrides_controlled(&connection, self.query_control.as_ref())?;
        let mut selected: Vec<_> = session_keys.iter().collect();
        selected.sort();
        let selection = serde_json::to_string(&selected)?;
        let mut statement = connection.prepare(
            "SELECT d.session_key,COALESCE(o.project_key,d.project_key),length(CAST(s.session_json AS BLOB)),
               CASE WHEN length(CAST(s.session_json AS BLOB)) <= ?2 THEN s.session_json END
             FROM durable_sessions d
             LEFT JOIN project_session_overrides o ON o.session_key=d.session_key
             LEFT JOIN session_snapshots s ON s.session_key=d.session_key
               AND s.version=d.current_snapshot_version
             WHERE d.session_key IN (SELECT value FROM json_each(?1)) ORDER BY d.session_key",
        )?;
        let mut rows = statement.query(params![selection, PER_SNAPSHOT_BYTES as i64])?;
        let mut consumed = 0usize;
        while let Some(row) = rows.next()? {
            self.check_query()?;
            let key: String = row.get(0)?;
            if let Some(control) = &self.query_control {
                control.consume_row()?;
            }
            let project: Option<String> = row.get(1)?;
            let project = project.map(|key| resolve_canonical_project_key(&overrides, &key));
            let snapshot_bytes: Option<i64> = row.get(2)?;
            let source = match row.get_ref(3)? {
                rusqlite::types::ValueRef::Null => {
                    WorkflowSource::Unavailable(if snapshot_bytes.is_some() {
                        "snapshot_analysis_limit"
                    } else {
                        "snapshot_not_retained"
                    })
                }
                value => {
                    let raw = value.as_bytes()?;
                    if raw.len() > PER_SNAPSHOT_BYTES
                        || raw.len() > TOTAL_SNAPSHOT_BYTES.saturating_sub(consumed)
                    {
                        WorkflowSource::Unavailable("snapshot_analysis_limit")
                    } else {
                        consumed += raw.len();
                        match serde_json::from_slice::<WorkflowSnapshot>(raw) {
                            Ok(snapshot) => WorkflowSource::Available(snapshot),
                            Err(_) => WorkflowSource::Unavailable("invalid_workflow_snapshot"),
                        }
                    }
                }
            };
            self.check_query()?;
            visit(&key, project.as_deref(), source)?;
        }
        Ok(())
    }
}
