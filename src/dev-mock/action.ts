import type { ControlledActionDraft, ControlledActionPreview } from '../lib/types';

/** Browser-only read-only projection of the Rust preview contract. */
export function actionPreviewFixture(action: ControlledActionDraft): ControlledActionPreview {
  const budget = action.kind === 'budget_guard';
  return {
    contract_version: 1,
    kind: action.kind,
    target_type: budget ? 'provider_guard_configuration' : 'agent_workflow_configuration',
    redacted_target: 'target:synthetic',
    source_revision: budget ? action.draft.expected_config_revision : String(action.draft.expected_finding_revision),
    preconditions: [{ code: budget ? 'advisory_budget_exists' : 'recorded_comparable_finding', satisfied: true },
      { code: budget ? 'provider_adapter_reviewed' : 'configuration_target_reviewed', satisfied: false }],
    proposed_change: budget
      ? 'A future reviewed adapter would enforce this existing advisory budget at its provider.'
      : 'A future reviewed remediation would reduce repeated reads without changing session history.',
    backup_requirement: budget
      ? 'A future adapter must identify the exact target and create an atomic private backup before a write.'
      : 'A future adapter must name the exact configuration target and retain an atomic private backup.',
    postcondition_requirement: budget
      ? 'A future adapter must read back the active cap, override, and removal state.'
      : 'A future adapter must verify its exact configured effect; finding counts alone are insufficient.',
    apply_available: false,
    undo_available: false,
    unavailable_reason: 'writes_disabled_pending_security_review',
  };
}
