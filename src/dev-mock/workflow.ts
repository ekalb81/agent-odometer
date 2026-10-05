import type { WorkflowFinding, WorkflowMetric, WorkflowMeasure, WorkflowReport } from '../lib/types';

let recorded = false;
let suppressed = false;
let revision = 0;

const beforeFrom = '2026-07-15T15:30:00.000Z';
const beforeTo = '2026-07-22T15:29:59.999Z';
const afterFrom = '2026-07-22T15:30:00.000Z';
const afterTo = '2026-07-29T15:30:00.000Z';

function metric(id: string, value: number | null, numerator: number, denominator: number, denominatorIs: string): WorkflowMetric {
  return { id, value, numerator, denominator, denominator_is: denominatorIs };
}

function measure(id: string, value: number | null, numerator: number, denominator: number,
  denominatorIs: string, missingData: string | null = null): WorkflowMeasure {
  return { ...metric(id, value, numerator, denominator, denominatorIs), unit: 'ratio',
    coverage_is: 'observed selected sessions', covered_samples: missingData ? 0 : 3,
    eligible_samples: 3, missing_data: missingData };
}

function finding(): WorkflowFinding {
  return {
    id: 'finding:synthetic-repeated-read', provider: 'codex', project_id: 'project:synthetic',
    rule_id: 'repeated-read', lifecycle: recorded ? {
      first_observed_at: afterTo, last_observed_at: afterTo, revision,
      state: suppressed ? 'suppressed' : 'improving', suppressed, analyzer_changed: false,
    } : null,
    before: { sessions: 3, tool_calls: 12, findings: 3, likely_avoidable_calls: 6,
      analyzer_version: 2, coverage_complete: true, window_duration_ms: 604_800_000 },
    after: { sessions: 3, tool_calls: 9, findings: 1, likely_avoidable_calls: 2,
      analyzer_version: 2, coverage_complete: true, window_duration_ms: 604_800_000 },
    comparison: { version: 2, state: suppressed ? 'suppressed' : 'improving', comparable: true,
      before_calls_per_100: 50, after_calls_per_100: 22.2, observed_change_per_100_calls: -27.8,
      limitations: ['observational_comparison', 'accepted_quality_unavailable'] },
    evidence: [{ session_id: 'codex:synthetic-workflow', turn_id: null, timestamp: afterTo }],
    evidence_truncated: false,
  };
}

export function workflowFixture(command: string, payload: Record<string, unknown>): WorkflowReport | { revision: number } {
  if (command === 'set_workflow_finding_suppression') {
    const edit = payload.edit as { expected_revision: number; suppressed: boolean };
    if (!recorded || edit.expected_revision !== revision) throw new Error('Finding changed or is unavailable');
    suppressed = edit.suppressed;
    revision += 1;
    return { revision };
  }
  if (command === 'record_workflow_measurement') {
    recorded = true;
    revision += 1;
  }
  const before = [
    metric('tool_failure_rate', 0.25, 3, 12, 'recorded tool calls'),
    metric('mutation_rework_rate', 0.2, 1, 5, 'recorded mutations'),
    metric('context_to_output_ratio', 4, 4000, 1000, 'output tokens'),
    metric('cached_input_share', 0.2, 800, 4000, 'input tokens'),
    metric('pricing_coverage', 1, 3, 3, 'priced sessions'),
  ];
  const after = [
    metric('tool_failure_rate', 0.11, 1, 9, 'recorded tool calls'),
    metric('mutation_rework_rate', 0, 0, 4, 'recorded mutations'),
    metric('context_to_output_ratio', 3, 3000, 1000, 'output tokens'),
    metric('cached_input_share', 0.3, 900, 3000, 'input tokens'),
    metric('pricing_coverage', 1, 3, 3, 'priced sessions'),
  ];
  const additional = (old: boolean) => [
    measure('tools_per_tool_active_turn', old ? 3 : 2.25, old ? 12 : 9, 4, 'tool-active turns'),
    measure('planning_turn_share', old ? 0.25 : 0.5, old ? 1 : 2, 4, 'classified turns'),
    measure('observed_subagent_session_share', 0.33, 1, 3, 'sessions with recorded usage'),
    measure('median_time_to_first_edit', old ? 120000 : 90000, 0, 3, 'sessions with edit timing'),
    measure('user_correction_rate', null, 0, 0, 'user-reviewed deliveries', 'human_review_unavailable'),
    measure('accepted_delivery_time', null, 0, 0, 'accepted deliveries', 'human_acceptance_unavailable'),
    measure('realized_causal_savings', null, 0, 0, 'controlled comparisons', 'causal_evidence_unavailable'),
  ];
  return {
    version: 2, generated_at: afterTo, analyzer_version: 2, selected_sessions: 6,
    coverage_complete: true, historical_findings: [], findings: [finding()],
    before: { from: beforeFrom, to: beforeTo, analyzed_sessions: 3, unavailable_sessions: 0,
      ledger_metrics: { schema_version: 2, from: beforeFrom, to: beforeTo, sessions: 3, metrics: before },
      additional_metrics: additional(true), drilldowns: [] },
    after: { from: afterFrom, to: afterTo, analyzed_sessions: 3, unavailable_sessions: 0,
      ledger_metrics: { schema_version: 2, from: afterFrom, to: afterTo, sessions: 3, metrics: after },
      additional_metrics: additional(false), drilldowns: [
        { dimension: 'project', value: 'Synthetic project', sessions: 3, tool_calls: 9, classified_turns: 0 },
        { dimension: 'model', value: 'gpt-5.4', sessions: 0, tool_calls: 9, classified_turns: 0 },
        { dimension: 'category', value: 'implementation', sessions: 0, tool_calls: 0, classified_turns: 4 },
      ] },
    setup_health: { source_configuration_valid: true, generated_at: afterTo, last_scan_at: afterTo,
      providers: [{ provider: 'codex', state: 'ready', configured_roots: 1, available_roots: 1,
        parsed_files: 6, parse_failures: 0, durable_sessions: 6, fallback_pricing_used: false, reasons: [] }] },
    limitations: ['quality_and_causality_not_measured'],
  };
}
