//! Bounded private projections for workflow evidence; no prompt or output fields.
use super::*;
use crate::workflow::{WorkflowSnapshot, WorkflowSource};
use std::collections::HashSet;

impl HistoryStore {
    pub(crate) fn load_workflow_lifecycle(
        &self,
        report: &mut crate::workflow::WorkflowReport,
    ) -> Result<()> {
        let connection = self.open_reader()?;
        for finding in &mut report.findings {
            self.check_query()?;
            let lifecycle = crate::workflow::lifecycle::read(
                &connection,
                &finding.provider,
                finding.project_id.as_deref(),
                &finding.rule_id,
                report.analyzer_version,
                report.version,
            )?;
            if let Some(lifecycle) = &lifecycle {
                if lifecycle.suppressed {
                    finding.comparison.state = crate::workflow::FindingState::Suppressed;
                } else if lifecycle.analyzer_changed {
                    finding.comparison.state = crate::workflow::FindingState::NotApplicable;
                    finding.comparison.comparable = false;
                    finding.comparison.observed_change_per_100_calls = None;
                    finding
                        .comparison
                        .limitations
                        .push("recorded_analyzer_changed".into());
                }
            }
            finding.lifecycle = lifecycle;
        }
        let current: HashSet<_> = report
            .findings
            .iter()
            .map(|finding| finding.id.clone())
            .collect();
        let mut select = connection.prepare("SELECT provider,project_key,rule_id FROM workflow_finding_lifecycle ORDER BY last_observed_ms DESC,provider,project_key,rule_id LIMIT 1001")?;
        let mut rows = select.query([])?;
        let mut count = 0;
        while let Some(row) = rows.next()? {
            self.check_query()?;
            count += 1;
            if count > 1000 {
                bail!("Workflow lifecycle history exceeds its bounded query limit.");
            }
            let provider: String = row.get(0)?;
            let project: String = row.get(1)?;
            let rule: String = row.get(2)?;
            let project = (!project.is_empty()).then_some(project);
            let id = crate::workflow::finding_identity(
                &provider,
                project.as_deref().unwrap_or(""),
                &rule,
            );
            if current.contains(&id) {
                continue;
            }
            let mut lifecycle = crate::workflow::lifecycle::read(
                &connection,
                &provider,
                project.as_deref(),
                &rule,
                report.analyzer_version,
                report.version,
            )?
            .ok_or_else(|| anyhow!("Lifecycle row changed"))?;
            // Outside current scope / absent rule is never a demonstrated resolution.
            if !lifecycle.suppressed {
                lifecycle.state = crate::workflow::FindingState::NotApplicable;
            }
            report
                .historical_findings
                .push(crate::workflow::HistoricalWorkflowFinding {
                    id,
                    provider,
                    project_id: project,
                    rule_id: rule,
                    lifecycle,
                });
        }
        Ok(())
    }

    /// Explicit action only. The caller drops its read snapshot before this transaction.
    pub(crate) fn record_workflow_measurement(
        &self,
        report: &mut crate::workflow::WorkflowReport,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if report.findings.len() > 1000 {
            bail!("Too many findings to record in one measurement.");
        }
        let count: i64 = transaction.query_row(
            "SELECT count(*) FROM workflow_finding_lifecycle",
            [],
            |row| row.get(0),
        )?;
        let mut new = 0;
        for finding in &report.findings {
            let exists: bool = transaction.query_row("SELECT EXISTS(SELECT 1 FROM workflow_finding_lifecycle WHERE provider=?1 AND project_key=?2 AND rule_id=?3)",
                params![finding.provider,finding.project_id.as_deref().unwrap_or(""),finding.rule_id],|row|row.get(0))?;
            if !exists {
                new += 1;
            }
        }
        if count + new > 1000 {
            bail!("Workflow lifecycle history is full; no measurement was recorded.");
        }
        crate::workflow::lifecycle::invalidate_older_analyzers(
            &transaction,
            report.analyzer_version,
            report.version,
        )?;
        for finding in &mut report.findings {
            let lifecycle = crate::workflow::lifecycle::observe(
                &transaction,
                crate::workflow::lifecycle::Observation {
                    provider: &finding.provider,
                    project: finding.project_id.as_deref(),
                    rule: &finding.rule_id,
                    measured_at_ms: report.generated_at.timestamp_millis(),
                    analyzer_version: report.analyzer_version,
                    comparison_version: report.version,
                    state: finding.comparison.state,
                },
            )?;
            if lifecycle.analyzer_changed {
                finding.comparison.state = crate::workflow::FindingState::NotApplicable;
                finding.comparison.comparable = false;
                finding.comparison.observed_change_per_100_calls = None;
                finding
                    .comparison
                    .limitations
                    .push("recorded_analyzer_changed".into());
            } else if lifecycle.suppressed {
                finding.comparison.state = crate::workflow::FindingState::Suppressed;
            }
            finding.lifecycle = Some(lifecycle);
        }
        if !crate::workflow::fits_output_budget(report, 8 * 1024 * 1024) {
            bail!("Workflow report exceeds its size limit; select fewer sessions.");
        }
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn suppress_workflow_finding(
        &self,
        edit: &crate::workflow::FindingSuppressionEdit,
    ) -> Result<()> {
        let mut connection = self.connection()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        crate::workflow::lifecycle::suppress(&transaction, edit)?;
        transaction.commit()?;
        Ok(())
    }

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
                            Ok(snapshot) => WorkflowSource::Available(Box::new(snapshot)),
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
