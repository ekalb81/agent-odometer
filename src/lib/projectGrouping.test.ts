import { describe, expect, it } from 'vitest';
import { addTotals, zeroToolMetrics, zeroTotals } from './sessionProjection';
import type { SessionSummary, TokenTotals } from './types';

/**
 * Project totals (#41) are never computed by a separate aggregation path —
 * they are the same per-session `SessionSummary` totals the rest of the
 * dashboard already sums, grouped by `project_key`. This guards that
 * reconciliation directly: summing every project's sessions must equal
 * summing all sessions, with every session accounted for exactly once
 * (including the one with no project, which must land in neither project's
 * total nor be silently dropped from the overall total).
 */
function totals(input: number): TokenTotals {
  return {
    input_tokens: input,
    cached_input_tokens: 0,
    cache_creation_input_tokens: 0,
    output_tokens: Math.round(input / 2),
    reasoning_output_tokens: 0,
    total_tokens: input + Math.round(input / 2),
  };
}

function fixture(id: string, projectKey: string | null, projectLabel: string | null, input: number): SessionSummary {
  return {
    id,
    storage_id: `codex:thread:${id}`,
    harness: 'codex',
    thread_name: null,
    forked_from_id: null,
    parent_thread_id: null,
    agent_path: null,
    agent_nickname: null,
    file_path: `${id}.jsonl`,
    source_availability: 'present',
    archived: false,
    started_at: '2026-01-01T00:00:00Z',
    last_event_at: '2026-01-01T01:00:00Z',
    working_directory: projectKey ? `/home/dev/${projectLabel}` : null,
    originator: null,
    source: null,
    cli_version: null,
    model_provider: 'openai',
    model: 'gpt-test',
    service_tier: null,
    plan_type: null,
    credits_unlimited: null,
    credits_balance: null,
    context_window: null,
    total_turns: 1,
    first_user_message: null,
    tokens_total: totals(input),
    buckets: [],
    tool_metrics: zeroToolMetrics(),
    tool_metrics_by_model: {},
    category_totals: {},
    optimization_findings_count: 0,
    project_key: projectKey,
    project_label: projectLabel,
    project_provenance: projectKey ? 'repository_root' : null,
  };
}

describe('project totals reconcile to unfiltered session totals (#41)', () => {
  const sessions: SessionSummary[] = [
    fixture('a1', 'repo:alpha', 'alpha', 1000),
    fixture('a2', 'repo:alpha', 'alpha', 2500),
    fixture('b1', 'repo:beta', 'beta', 400),
    fixture('no-project', null, null, 750),
  ];

  function groupByProject(rows: SessionSummary[]): Map<string, { tokens: TokenTotals; sessions: SessionSummary[] }> {
    const groups = new Map<string, { tokens: TokenTotals; sessions: SessionSummary[] }>();
    for (const session of rows) {
      if (!session.project_key) continue;
      const group = groups.get(session.project_key) ?? { tokens: zeroTotals(), sessions: [] };
      addTotals(group.tokens, session.tokens_total);
      group.sessions.push(session);
      groups.set(session.project_key, group);
    }
    return groups;
  }

  it('sums exactly to the unfiltered totals of the same session set', () => {
    const overall = zeroTotals();
    for (const session of sessions) addTotals(overall, session.tokens_total);

    const grouped = groupByProject(sessions);
    const reconciled = zeroTotals();
    let groupedSessionCount = 0;
    for (const group of grouped.values()) {
      addTotals(reconciled, group.tokens);
      groupedSessionCount += group.sessions.length;
    }

    // Every session with a project is counted in exactly one project group;
    // the session without a project is neither dropped from the corpus nor
    // silently folded into a project total.
    const withProject = sessions.filter((s) => s.project_key !== null);
    expect(groupedSessionCount).toBe(withProject.length);
    expect(groupedSessionCount).toBe(sessions.length - 1);

    const expectedWithoutNoProject = zeroTotals();
    for (const session of withProject) addTotals(expectedWithoutNoProject, session.tokens_total);
    expect(reconciled).toEqual(expectedWithoutNoProject);

    // And per-project totals are exactly what a direct filter+sum produces —
    // the reconciliation guarantee the acceptance criterion asks for.
    expect(grouped.get('repo:alpha')!.tokens).toEqual(totals(1000 + 2500));
    expect(grouped.get('repo:beta')!.tokens).toEqual(totals(400));

    // Sanity: the overall (unfiltered) total minus the one project-less
    // session's tokens equals the reconciled grouped total.
    const withoutNoProject = zeroTotals();
    addTotals(withoutNoProject, overall);
    withoutNoProject.input_tokens -= totals(750).input_tokens;
    withoutNoProject.output_tokens -= totals(750).output_tokens;
    withoutNoProject.total_tokens -= totals(750).total_tokens;
    expect(reconciled).toEqual(withoutNoProject);
  });
});
