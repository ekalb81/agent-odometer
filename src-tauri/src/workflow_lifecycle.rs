//! Private finding metadata. Installation belongs only to the ledger migration.
use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::workflow::FindingState;

pub(crate) fn install_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE workflow_finding_lifecycle (
           provider TEXT NOT NULL, project_key TEXT NOT NULL, rule_id TEXT NOT NULL,
           first_observed_ms INTEGER NOT NULL, last_observed_ms INTEGER NOT NULL,
           analyzer_version INTEGER NOT NULL, comparison_version INTEGER NOT NULL,
           state TEXT NOT NULL, suppressed INTEGER NOT NULL DEFAULT 0,
           revision INTEGER NOT NULL,
           PRIMARY KEY(provider,project_key,rule_id)
         );",
    )?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindingLifecycle {
    pub first_observed_at: chrono::DateTime<chrono::Utc>,
    pub last_observed_at: chrono::DateTime<chrono::Utc>,
    pub state: FindingState,
    pub suppressed: bool,
    pub revision: i64,
    pub analyzer_changed: bool,
}

#[derive(Debug, Deserialize)]
pub struct FindingSuppressionEdit {
    pub provider: String,
    pub project_id: Option<String>,
    pub rule_id: String,
    pub expected_revision: i64,
    pub suppressed: bool,
}

pub(crate) struct Observation<'a> {
    pub provider: &'a str,
    pub project: Option<&'a str>,
    pub rule: &'a str,
    pub measured_at_ms: i64,
    pub analyzer_version: u32,
    pub comparison_version: u32,
    pub state: FindingState,
}

fn state_text(state: FindingState) -> &'static str {
    match state {
        FindingState::New => "new",
        FindingState::Persistent => "persistent",
        FindingState::Improving => "improving",
        FindingState::Resolved => "resolved",
        FindingState::Suppressed => "suppressed",
        FindingState::NotApplicable => "not_applicable",
    }
}

pub(crate) fn read(
    connection: &Connection,
    provider: &str,
    project: Option<&str>,
    rule: &str,
    analyzer: u32,
    comparison: u32,
) -> Result<Option<FindingLifecycle>> {
    let row: Option<(i64,i64,u32,u32,String,bool,i64)> = connection.query_row(
        "SELECT first_observed_ms,last_observed_ms,analyzer_version,comparison_version,state,suppressed,revision
         FROM workflow_finding_lifecycle WHERE provider=?1 AND project_key=?2 AND rule_id=?3",
        params![provider,project.unwrap_or(""),rule],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
    ).optional()?;
    row.map(|row| {
        Ok(FindingLifecycle {
            first_observed_at: chrono::DateTime::from_timestamp_millis(row.0)
                .ok_or_else(|| anyhow::anyhow!("Invalid observation time"))?,
            last_observed_at: chrono::DateTime::from_timestamp_millis(row.1)
                .ok_or_else(|| anyhow::anyhow!("Invalid observation time"))?,
            state: serde_json::from_value(serde_json::Value::String(row.4))?,
            suppressed: row.5,
            revision: row.6,
            analyzer_changed: row.2 != analyzer || row.3 != comparison,
        })
    })
    .transpose()
}

pub(crate) fn observe(
    connection: &Connection,
    observation: Observation<'_>,
) -> Result<FindingLifecycle> {
    let project = observation.project.unwrap_or("");
    let previous: Option<(i64, i64, u32, u32, bool, i64)> = connection.query_row(
        "SELECT first_observed_ms,last_observed_ms,analyzer_version,comparison_version,suppressed,revision
         FROM workflow_finding_lifecycle WHERE provider=?1 AND project_key=?2 AND rule_id=?3",
        params![observation.provider, project, observation.rule],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).optional()?;
    if previous.is_some_and(|previous| previous.1 > observation.measured_at_ms) {
        bail!("A newer workflow measurement already exists; refresh before recording.");
    }
    let first = previous.map_or(observation.measured_at_ms, |previous| previous.0);
    let changed = previous.is_some_and(|previous| {
        previous.2 != observation.analyzer_version || previous.3 != observation.comparison_version
    });
    let suppressed = previous.is_some_and(|previous| previous.4);
    let revision = previous
        .map_or(Some(1), |previous| previous.5.checked_add(1))
        .filter(|revision| *revision > 0 && *revision <= 9_007_199_254_740_991)
        .ok_or_else(|| anyhow::anyhow!("Finding revision exhausted"))?;
    let state = if suppressed {
        FindingState::Suppressed
    } else if changed {
        FindingState::NotApplicable
    } else {
        observation.state
    };
    connection.execute(
        "INSERT INTO workflow_finding_lifecycle VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
         ON CONFLICT(provider,project_key,rule_id) DO UPDATE SET
           last_observed_ms=excluded.last_observed_ms,analyzer_version=excluded.analyzer_version,
           comparison_version=excluded.comparison_version,state=excluded.state,revision=excluded.revision",
        params![observation.provider,project,observation.rule,first,observation.measured_at_ms,
            observation.analyzer_version,observation.comparison_version,state_text(state),suppressed,revision],
    )?;
    Ok(FindingLifecycle {
        first_observed_at: chrono::DateTime::from_timestamp_millis(first)
            .ok_or_else(|| anyhow::anyhow!("Invalid observation time"))?,
        last_observed_at: chrono::DateTime::from_timestamp_millis(observation.measured_at_ms)
            .ok_or_else(|| anyhow::anyhow!("Invalid observation time"))?,
        state,
        suppressed,
        revision,
        analyzer_changed: changed,
    })
}

pub(crate) fn suppress(connection: &Connection, edit: &FindingSuppressionEdit) -> Result<()> {
    if edit.provider.is_empty()
        || edit.expected_revision < 1
        || edit.expected_revision >= 9_007_199_254_740_991
        || edit.provider.len() > 64
        || edit.rule_id.is_empty()
        || edit.rule_id.len() > 128
        || edit
            .project_id
            .as_ref()
            .is_some_and(|value| value.is_empty() || value.len() > 1024)
    {
        bail!("Invalid finding scope.");
    }
    let changed = connection.execute(
        "UPDATE workflow_finding_lifecycle SET suppressed=?1,
           state=CASE WHEN ?1 THEN 'suppressed' ELSE 'not_applicable' END,revision=revision+1
         WHERE provider=?2 AND project_key=?3 AND rule_id=?4 AND revision=?5",
        params![
            edit.suppressed,
            edit.provider,
            edit.project_id.as_deref().unwrap_or(""),
            edit.rule_id,
            edit.expected_revision
        ],
    )?;
    if changed != 1 {
        bail!("Finding changed or is unavailable; refresh before editing suppression.");
    }
    Ok(())
}

/// Missing rules are unavailable observations, never proof of resolved behavior.
pub(crate) fn invalidate_older_analyzers(
    connection: &Connection,
    analyzer: u32,
    comparison: u32,
) -> Result<()> {
    connection.execute(
        "UPDATE workflow_finding_lifecycle SET state=CASE WHEN suppressed THEN 'suppressed' ELSE 'not_applicable' END,
          revision=revision+1 WHERE (analyzer_version<>?1 OR comparison_version<>?2)
          AND state NOT IN ('not_applicable','suppressed')",
        params![analyzer,comparison],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record(
        connection: &Connection,
        at: i64,
        analyzer: u32,
        state: FindingState,
    ) -> FindingLifecycle {
        observe(
            connection,
            Observation {
                provider: "codex",
                project: Some("project:synthetic"),
                rule: "repeated-read",
                measured_at_ms: at,
                analyzer_version: analyzer,
                comparison_version: 1,
                state,
            },
        )
        .unwrap()
    }
    #[test]
    fn restart_and_analyzer_upgrade_preserve_identity_without_a_false_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        {
            let connection = Connection::open(&path).unwrap();
            install_schema(&connection).unwrap();
            assert_eq!(record(&connection, 1000, 3, FindingState::New).revision, 1);
        }
        let connection = Connection::open(&path).unwrap();
        let persistent = record(&connection, 2000, 3, FindingState::Persistent);
        assert_eq!(persistent.first_observed_at.timestamp_millis(), 1000);
        let upgraded = record(&connection, 3000, 4, FindingState::Resolved);
        assert!(upgraded.analyzer_changed);
        assert_eq!(upgraded.state, FindingState::NotApplicable);
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM workflow_finding_lifecycle",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
    #[test]
    fn suppression_is_scoped_revision_checked_and_survives_new_measurements() {
        let connection = Connection::open_in_memory().unwrap();
        install_schema(&connection).unwrap();
        let first = record(&connection, 1000, 3, FindingState::New);
        let edit = FindingSuppressionEdit {
            provider: "codex".into(),
            project_id: Some("project:synthetic".into()),
            rule_id: "repeated-read".into(),
            expected_revision: first.revision,
            suppressed: true,
        };
        suppress(&connection, &edit).unwrap();
        assert!(suppress(&connection, &edit).is_err());
        let next = record(&connection, 2000, 4, FindingState::Resolved);
        assert!(next.suppressed);
        assert_eq!(next.state, FindingState::Suppressed);
        let wrong = FindingSuppressionEdit {
            project_id: Some("project:other".into()),
            expected_revision: next.revision,
            ..edit
        };
        assert!(suppress(&connection, &wrong).is_err());
        assert!(observe(
            &connection,
            Observation {
                provider: "codex",
                project: Some("project:synthetic"),
                rule: "repeated-read",
                measured_at_ms: 1500,
                analyzer_version: 3,
                comparison_version: 1,
                state: FindingState::Resolved
            }
        )
        .is_err());
    }
    #[test]
    fn renamed_rule_is_not_a_resolved_win() {
        let connection = Connection::open_in_memory().unwrap();
        install_schema(&connection).unwrap();
        record(&connection, 1000, 3, FindingState::Persistent);
        invalidate_older_analyzers(&connection, 4, 1).unwrap();
        let state: String = connection
            .query_row("SELECT state FROM workflow_finding_lifecycle", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(state, "not_applicable");
    }

    #[test]
    fn version_twelve_history_upgrades_once_and_keeps_measurements() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite3");
        drop(crate::history_store::HistoryStore::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "DROP TABLE workflow_finding_lifecycle;
            UPDATE history_meta SET value='12' WHERE key='schema_version';
            PRAGMA user_version=12;",
            )
            .unwrap();
        drop(connection);
        drop(crate::history_store::HistoryStore::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            14
        );
        assert_eq!(record(&connection, 1000, 3, FindingState::New).revision, 1);
        drop(connection);
        drop(crate::history_store::HistoryStore::open(&path).unwrap());
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            record(&connection, 2000, 3, FindingState::Persistent).revision,
            2
        );
    }
}
