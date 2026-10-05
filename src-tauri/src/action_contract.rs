//! Read-only, versioned previews for possible controlled actions. Issue #46
//! explicitly does not authorize an executor, provider hook, or undo write.

use crate::quota_store::QuotaStoreFile;
use crate::workflow::{FindingState, WorkflowFinding, WorkflowRequest};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const ACTION_CONTRACT_VERSION: u32 = 1;
const MAX_ID_BYTES: usize = 128;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewRequest {
    pub contract_version: u32,
    pub action: ActionDraft,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "draft",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ActionDraft {
    BudgetGuard(BudgetGuardDraft),
    WorkflowRemediation(WorkflowRemediationDraft),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetGuardDraft {
    pub budget_id: String,
    pub expected_config_revision: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRemediationDraft {
    pub scope: WorkflowScope,
    pub finding_id: String,
    pub expected_finding_revision: i64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowScope {
    pub session_ids: Vec<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl From<WorkflowScope> for WorkflowRequest {
    fn from(value: WorkflowScope) -> Self {
        Self {
            session_ids: value.session_ids,
            from: value.from,
            to: value.to,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ActionPreview {
    pub contract_version: u32,
    pub kind: &'static str,
    pub target_type: &'static str,
    pub redacted_target: String,
    pub source_revision: String,
    pub preconditions: Vec<PreviewPrecondition>,
    pub proposed_change: &'static str,
    pub backup_requirement: &'static str,
    pub postcondition_requirement: &'static str,
    pub apply_available: bool,
    pub undo_available: bool,
    pub unavailable_reason: &'static str,
}

#[derive(Debug, Serialize)]
pub struct PreviewPrecondition {
    pub code: &'static str,
    pub satisfied: bool,
}

fn checked_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > MAX_ID_BYTES || value.chars().any(char::is_control) {
        return Err("Action draft identifier is invalid.");
    }
    Ok(())
}

fn redacted_target(kind: &str, identity: &str) -> String {
    let mut bytes = Vec::with_capacity(kind.len() + identity.len() + 1);
    bytes.extend_from_slice(kind.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(identity.as_bytes());
    format!("target:{:016x}", crate::stable_hash::fnv1a64(&bytes))
}

pub fn validate_version(version: u32) -> Result<(), &'static str> {
    (version == ACTION_CONTRACT_VERSION)
        .then_some(())
        .ok_or("Action contract version is unsupported.")
}

pub fn preview_budget_guard(
    draft: &BudgetGuardDraft,
    store: &QuotaStoreFile,
) -> Result<ActionPreview, &'static str> {
    checked_id(&draft.budget_id)?;
    checked_id(&draft.expected_config_revision)?;
    let actual_revision = store.config_revision();
    if draft.expected_config_revision != actual_revision {
        return Err("Action source changed; refresh before previewing.");
    }
    let budget = store
        .budgets
        .iter()
        .find(|budget| budget.id == draft.budget_id && budget.enabled)
        .ok_or("Action source is unavailable; refresh before previewing.")?;
    Ok(ActionPreview {
        contract_version: ACTION_CONTRACT_VERSION,
        kind: "budget_guard",
        target_type: "provider_guard_configuration",
        redacted_target: redacted_target("budget", &budget.id),
        source_revision: actual_revision,
        preconditions: vec![
            PreviewPrecondition {
                code: "advisory_budget_exists",
                satisfied: true,
            },
            PreviewPrecondition {
                code: "provider_adapter_reviewed",
                satisfied: false,
            },
        ],
        proposed_change: "A future reviewed adapter would enforce this existing advisory budget at its provider.",
        backup_requirement: "A future adapter must identify the exact target and create an atomic private backup before a write.",
        postcondition_requirement: "A future adapter must read back the active cap, override, and removal state.",
        apply_available: false,
        undo_available: false,
        unavailable_reason: "writes_disabled_pending_security_review",
    })
}

pub fn preview_workflow_remediation(
    draft: &WorkflowRemediationDraft,
    findings: &[WorkflowFinding],
) -> Result<ActionPreview, &'static str> {
    checked_id(&draft.finding_id)?;
    if draft.expected_finding_revision < 1 {
        return Err("Action source revision is invalid.");
    }
    let finding: &WorkflowFinding = findings
        .iter()
        .find(|finding| finding.id == draft.finding_id)
        .ok_or("Action source is unavailable; refresh before previewing.")?;
    let lifecycle = finding
        .lifecycle
        .as_ref()
        .ok_or("Record a workflow measurement before previewing an action.")?;
    if lifecycle.revision != draft.expected_finding_revision
        || lifecycle.suppressed
        || !finding.comparison.comparable
        || matches!(
            finding.comparison.state,
            FindingState::Resolved | FindingState::Suppressed | FindingState::NotApplicable
        )
    {
        return Err("Action source changed or is not comparable; refresh before previewing.");
    }
    if finding.rule_id != "repeated-read" {
        return Err("No reviewed action draft exists for this finding rule.");
    }
    Ok(ActionPreview {
        contract_version: ACTION_CONTRACT_VERSION,
        kind: "workflow_remediation",
        target_type: "agent_workflow_configuration",
        redacted_target: redacted_target("finding", &finding.id),
        source_revision: lifecycle.revision.to_string(),
        preconditions: vec![
            PreviewPrecondition {
                code: "recorded_comparable_finding",
                satisfied: true,
            },
            PreviewPrecondition {
                code: "configuration_target_reviewed",
                satisfied: false,
            },
        ],
        proposed_change: "A future reviewed remediation would reduce repeated reads without changing session history.",
        backup_requirement: "A future adapter must name the exact configuration target and retain an atomic private backup.",
        postcondition_requirement: "A future adapter must verify its exact configured effect; finding counts alone are insufficient.",
        apply_available: false,
        undo_available: false,
        unavailable_reason: "writes_disabled_pending_security_review",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ProviderId;
    use crate::quota_store::{BudgetUnit, QuotaBudget};
    use crate::workflow::{FindingComparison, FindingLifecycle, FindingObservation};

    #[test]
    fn strict_draft_rejects_unknown_actions_and_fields() {
        for json in [
            r#"{"contract_version":1,"action":{"kind":"shell","draft":{}}}"#,
            r#"{"contract_version":1,"action":{"kind":"budget_guard","draft":{"budget_id":"b","expected_config_revision":"r","command":"rm"}}}"#,
            r#"{"contract_version":1,"action":{"kind":"budget_guard","draft":{"budget_id":"b","expected_config_revision":"r"},"apply":true}}"#,
            r#"{"contract_version":1,"action":{"kind":"budget_guard","draft":{"budget_id":"b","expected_config_revision":"r"}},"apply":true}"#,
        ] {
            assert!(serde_json::from_str::<PreviewRequest>(json).is_err());
        }
        assert!(validate_version(2).is_err());
    }

    #[test]
    fn budget_preview_is_redacted_and_cannot_apply_or_undo() {
        let mut store = QuotaStoreFile::default();
        store.budgets.push(QuotaBudget {
            id: "private-budget-name".into(),
            provider: ProviderId::new("codex").unwrap(),
            project_key: None,
            unit: BudgetUnit::Tokens,
            window_kind: None,
            period_hours: Some(24),
            threshold: 1000.0,
            enabled: true,
        });
        let draft = BudgetGuardDraft {
            budget_id: "private-budget-name".into(),
            expected_config_revision: store.config_revision(),
        };
        let preview = preview_budget_guard(&draft, &store).unwrap();
        let serialized = serde_json::to_string(&preview).unwrap();
        assert!(!serialized.contains("private-budget-name"));
        assert!(!preview.apply_available && !preview.undo_available);
        let stale = BudgetGuardDraft {
            expected_config_revision: "stale".into(),
            ..draft
        };
        assert!(preview_budget_guard(&stale, &store).is_err());
    }

    #[test]
    fn workflow_preview_requires_current_recorded_comparable_allowlisted_finding() {
        let finding = WorkflowFinding {
            id: "finding:private".into(),
            provider: "codex".into(),
            project_id: Some("path:private".into()),
            rule_id: "repeated-read".into(),
            before: FindingObservation::default(),
            after: FindingObservation::default(),
            comparison: FindingComparison {
                version: 2,
                state: FindingState::Improving,
                comparable: true,
                before_calls_per_100: Some(30.0),
                after_calls_per_100: Some(10.0),
                observed_change_per_100_calls: Some(-20.0),
                limitations: vec![],
            },
            evidence: vec![],
            evidence_truncated: false,
            lifecycle: Some(FindingLifecycle {
                first_observed_at: Utc::now(),
                last_observed_at: Utc::now(),
                state: FindingState::Improving,
                suppressed: false,
                revision: 2,
                analyzer_changed: false,
            }),
        };
        let draft = WorkflowRemediationDraft {
            scope: WorkflowScope {
                session_ids: vec![],
                from: None,
                to: None,
            },
            finding_id: finding.id.clone(),
            expected_finding_revision: 2,
        };
        let mut findings = [finding];
        let preview = preview_workflow_remediation(&draft, &findings).unwrap();
        assert!(!preview.apply_available && !preview.undo_available);
        let serialized = serde_json::to_string(&preview).unwrap();
        assert!(!serialized.contains("finding:private"));
        assert!(!serialized.contains("path:private"));
        let mut stale = draft;
        stale.expected_finding_revision = 1;
        assert!(preview_workflow_remediation(&stale, &findings).is_err());
        stale.expected_finding_revision = 2;
        findings[0].rule_id = "unknown-rule".into();
        assert!(preview_workflow_remediation(&stale, &findings).is_err());
        findings[0].rule_id = "repeated-read".into();
        findings[0].lifecycle.as_mut().unwrap().suppressed = true;
        assert!(preview_workflow_remediation(&stale, &findings).is_err());
    }
}
