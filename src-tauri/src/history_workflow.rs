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
        let mut statement = connection.prepare(
            "SELECT d.session_key,d.project_key,s.session_json FROM durable_sessions d
             LEFT JOIN session_snapshots s ON s.session_key=d.session_key
               AND s.version=d.current_snapshot_version ORDER BY d.session_key",
        )?;
        let mut rows = statement.query([])?;
        let mut consumed = 0usize;
        while let Some(row) = rows.next()? {
            self.check_query()?;
            let key: String = row.get(0)?;
            if !session_keys.contains(&key) {
                continue;
            }
            if let Some(control) = &self.query_control {
                control.consume_row()?;
            }
            let project: Option<String> = row.get(1)?;
            let source = match row.get_ref(2)? {
                rusqlite::types::ValueRef::Null => {
                    WorkflowSource::Unavailable("snapshot_not_retained")
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
