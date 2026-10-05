import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, expect, it, vi } from 'vitest';
import WorkflowIntelligence from './WorkflowIntelligence.svelte';
import type { WorkflowReport, WorkflowWindow } from '../lib/types';
import { rates } from '../lib/stores/rates';
import { projectStore } from '../lib/stores/projects.svelte';

const { getWorkflowReport, previewControlledAction, recordWorkflowMeasurement, setWorkflowFindingSuppression, resolveProjects } = vi.hoisted(() => ({
  getWorkflowReport: vi.fn(), previewControlledAction: vi.fn(), recordWorkflowMeasurement: vi.fn(), setWorkflowFindingSuppression: vi.fn(), resolveProjects: vi.fn() }));
vi.mock('../lib/ipc', () => ({ getWorkflowReport, previewControlledAction, recordWorkflowMeasurement, setWorkflowFindingSuppression, resolveProjects }));

function period(): WorkflowWindow {
  return {
    from: '2026-09-27T00:00:00Z', to: '2026-10-04T00:00:00Z',
    ledger_metrics: { schema_version: 2, from: null, to: null, sessions: 3, metrics: [
      { id: 'tool_failure_rate', value: 0, numerator: 0, denominator: 20, denominator_is: 'known outcomes' },
      { id: 'pricing_coverage', value: null, numerator: 0, denominator: 0, denominator_is: 'priceable tokens' },
    ] },
    additional_metrics: [{ id: 'median_time_to_first_edit', value: null, unit: 'milliseconds',
      numerator: 0, denominator: 3, denominator_is: 'timestamped turns', coverage_is: 'linked mutation timestamps',
      covered_samples: 0, eligible_samples: 3, missing_data: 'no_linked_timed_mutations' }],
    analyzed_sessions: 3, unavailable_sessions: 0, drilldowns: [
      { dimension: 'project', value: 'opaque-project', sessions: 3, tool_calls: 20, classified_turns: 0 },
      { dimension: 'model', value: 'gpt-5.4', sessions: 0, tool_calls: 20, classified_turns: 0 },
    ],
  };
}
function report(count = 3): WorkflowReport {
  return { version: 1, generated_at: '2026-10-04T00:00:00Z', analyzer_version: 3,
    selected_sessions: count, coverage_complete: true, before: period(), after: period(), findings: [], historical_findings: [], setup_health: null, limitations: [] };
}
async function open(): Promise<void> {
  const details = screen.getByTestId('workflow-panel') as HTMLDetailsElement;
  details.open = true;
  await fireEvent(details, new Event('toggle'));
}
beforeEach(() => { vi.resetAllMocks(); rates.set(null); getWorkflowReport.mockResolvedValue(report()); resolveProjects.mockResolvedValue([]); });

it('does not measure while closed, then shows zero and missing data distinctly', async () => {
  render(WorkflowIntelligence, { sessionIds: ['codex:one'] });
  expect(getWorkflowReport).not.toHaveBeenCalled();
  await open();
  await screen.findByText(/History coverage: complete/);
  expect(getWorkflowReport).toHaveBeenCalledWith(expect.objectContaining({ session_ids: ['codex:one'] }));
  expect(screen.getAllByText('0.0%')).toHaveLength(2);
  expect(screen.getAllByText('Unavailable')).toHaveLength(4);
  expect(screen.getByText(/no linked timed mutations/)).toBeTruthy();
  expect(screen.getByText(/No findings.*not a quality verdict/)).toBeTruthy();
});

it('shows an honest unavailable state and retries without exposing backend error bodies', async () => {
  getWorkflowReport.mockRejectedValueOnce(new Error('PRIVATE filesystem path and transcript body'));
  render(WorkflowIntelligence, { sessionIds: ['codex:one'] });
  await open();
  await screen.findByRole('alert');
  expect(screen.queryByText(/PRIVATE/)).toBeNull();
  await fireEvent.click(screen.getByRole('button', { name: 'Refresh measurements' }));
  await screen.findByText(/History coverage: complete/);
  expect(getWorkflowReport).toHaveBeenCalledTimes(2);
});

it('rejects superseded results when selected sessions change', async () => {
  let resolveOld!: (value: WorkflowReport) => void;
  getWorkflowReport.mockImplementationOnce(() => new Promise((resolve) => { resolveOld = resolve; }));
  const view = render(WorkflowIntelligence, { sessionIds: ['codex:old'] });
  await open();
  await screen.findByRole('status');
  await view.rerender({ sessionIds: ['codex:new'] });
  await screen.findByText(/3 selected sessions/);
  resolveOld(report(99));
  await waitFor(() => expect(screen.queryByText(/99 selected sessions/)).toBeNull());
});

it('keeps finding limitations visible and follows opaque evidence identity', async () => {
  const measured = report();
  measured.coverage_complete = false;
  measured.after.unavailable_sessions = 2;
  measured.limitations = ['snapshot_not_retained'];
  const observation = { sessions: 1, tool_calls: 2, findings: 1, likely_avoidable_calls: 1,
    analyzer_version: 3, coverage_complete: false, window_duration_ms: 1000 };
  measured.findings = [{ id: 'finding:opaque', provider: 'codex', project_id: 'repo:opaque', rule_id: 'repeated-read',
    before: observation, after: observation, comparison: { version: 1, state: 'not_applicable', comparable: false,
      before_calls_per_100: 50, after_calls_per_100: 50, observed_change_per_100_calls: null,
      limitations: ['low_sample_size', 'incomplete_coverage'] },
    evidence: [{ session_id: 'codex:one', turn_id: 'turn', timestamp: null }], evidence_truncated: true, lifecycle: null }];
  getWorkflowReport.mockResolvedValue(measured);
  const onReview = vi.fn();
  render(WorkflowIntelligence, { sessionIds: ['codex:one'], onReview });
  await open();
  await screen.findByText(/History coverage: incomplete/);
  expect(screen.getByText(/low sample size.*incomplete coverage/)).toBeTruthy();
  expect(screen.getByText(/First 20 evidence anchors/)).toBeTruthy();
  await fireEvent.click(screen.getByRole('button', { name: 'Review session evidence 1' }));
  expect(onReview).toHaveBeenCalledWith('codex:one');
});

it('records only on explicit action, using the displayed measurement window', async () => {
  recordWorkflowMeasurement.mockResolvedValue(report(6));
  render(WorkflowIntelligence, { sessionIds: ['codex:one'] });
  await open();
  await screen.findByText(/3 selected sessions/);
  expect(recordWorkflowMeasurement).not.toHaveBeenCalled();
  await fireEvent.click(screen.getByRole('button', { name: 'Record measurement' }));
  await screen.findByText(/6 selected sessions/);
  expect(recordWorkflowMeasurement).toHaveBeenCalledWith({session_ids:['codex:one'],
    from:period().from,to:period().to});
  expect(setWorkflowFindingSuppression).not.toHaveBeenCalled();
});

it('labels the call scenario as hypothetical and does not turn it into cost or quality savings', async () => {
  render(WorkflowIntelligence, { sessionIds: ['codex:one'] });
  await open();
  expect(await screen.findByText(/20 observed calls × 10% = 2 hypothetical calls avoided/)).toBeTruthy();
  expect(screen.getByText(/Token, cost, quality and delivery effects are unknown/)).toBeTruthy();
});

it('refreshes an open report when project scope is re-resolved', async () => {
  render(WorkflowIntelligence, { sessionIds: ['codex:one'] });
  await open();
  await screen.findByText(/3 selected sessions/);
  expect(getWorkflowReport).toHaveBeenCalledTimes(1);
  await projectStore.refresh();
  await waitFor(() => expect(getWorkflowReport).toHaveBeenCalledTimes(2));
});

it('previews a recorded finding without offering an apply or undo path', async () => {
  const measured = report();
  const observation = { sessions: 3, tool_calls: 12, findings: 2, likely_avoidable_calls: 1,
    analyzer_version: 3, coverage_complete: true, window_duration_ms: 604800000 };
  measured.findings = [{ id: 'finding:opaque', provider: 'codex', project_id: 'repo:opaque', rule_id: 'repeated-read',
    before: observation, after: observation, comparison: { version: 1, state: 'improving', comparable: true,
      before_calls_per_100: 20, after_calls_per_100: 10, observed_change_per_100_calls: -10, limitations: [] },
    evidence: [], evidence_truncated: false, lifecycle: { first_observed_at: period().from,
      last_observed_at: period().to, state: 'improving', suppressed: false, revision: 2, analyzer_changed: false } }];
  getWorkflowReport.mockResolvedValue(measured);
  previewControlledAction.mockResolvedValue({ target_type: 'agent_workflow_configuration',
    proposed_change: 'Future reviewed change', backup_requirement: 'Exact backup required',
    postcondition_requirement: 'Read-back required', apply_available: false, undo_available: false });
  render(WorkflowIntelligence, { sessionIds: ['codex:one'] });
  await open();
  await screen.findByRole('button', { name: 'Preview future action' });
  await fireEvent.click(screen.getByRole('button', { name: 'Preview future action' }));
  await screen.findByText('Future reviewed change');
  expect(previewControlledAction).toHaveBeenCalledWith({ kind: 'workflow_remediation', draft: {
    scope: { session_ids: ['codex:one'], from: period().from, to: period().to },
    finding_id: 'finding:opaque', expected_finding_revision: 2,
  } });
  expect(screen.getByText(/Apply and undo are unavailable/)).toBeTruthy();
  expect(screen.queryByRole('button', { name: /^Apply|^Undo/ })).toBeNull();
});
