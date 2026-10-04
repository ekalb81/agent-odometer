<script lang="ts">
  import { getWorkflowReport } from '../lib/ipc';
  import { rates } from '../lib/stores/rates';
  import { findingRuleTitle } from '../lib/optimization';
  import type { WorkflowMetric, WorkflowReport } from '../lib/types';

  interface Props {
    active?: boolean;
    sessionIds: string[];
    onReview?: (sessionId: string) => void;
  }
  let { active = true, sessionIds, onReview = () => {} }: Props = $props();
  let opened = $state(false);
  let period = $state('7');
  let refresh = $state(0);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let report = $state<WorkflowReport | null>(null);
  let generation = 0;
  const selectionKey = $derived(JSON.stringify(sessionIds));
  const labels: Record<string, string> = {
    tool_failure_rate: 'Tool failure rate', mutation_rework_rate: 'Mutation rework rate',
    context_to_output_ratio: 'Context / output', cached_input_share: 'Cached input share',
    pricing_coverage: 'Published pricing coverage', tools_per_tool_active_turn: 'Tools per tool-active turn',
    planning_turn_share: 'Planning turn share', observed_subagent_session_share: 'Observed subagent session share',
    median_time_to_first_edit: 'Median time to first edit', user_correction_rate: 'User correction rate',
    accepted_delivery_time: 'Accepted delivery time', realized_causal_savings: 'Realized causal savings',
  };
  const percentIds = new Set(['tool_failure_rate', 'mutation_rework_rate', 'cached_input_share',
    'pricing_coverage', 'planning_turn_share', 'observed_subagent_session_share']);
  function value(metric: WorkflowMetric | undefined): string {
    if (!metric || metric.value === null || !Number.isFinite(metric.value)) return 'Unavailable';
    return percentIds.has(metric.id) ? `${(metric.value * 100).toFixed(1)}%`
      : `${metric.value.toLocaleString(undefined, { maximumFractionDigits: 1 })}${metric.id === 'median_time_to_first_edit' ? ' ms' : ''}`;
  }
  function utcDate(timestamp: string): string {
    return new Date(timestamp).toLocaleDateString(undefined, { timeZone: 'UTC' });
  }
  async function load(ids: string[], days: number, token: number): Promise<void> {
    loading = true;
    error = null;
    report = null;
    const end = new Date();
    const start = new Date(end.getTime() - days * 86_400_000 + 1);
    try {
      const next = await getWorkflowReport({ session_ids: ids, from: start.toISOString(), to: end.toISOString() });
      if (token !== generation) return;
      report = next;
    } catch {
      if (token !== generation) return;
      error = 'Workflow analysis could not be loaded. Check history readiness, or select fewer sessions and retry.';
    } finally {
      if (token === generation) loading = false;
    }
  }
  $effect(() => {
    const token = ++generation;
    if (!active || !opened) return;
    const ids: string[] = JSON.parse(selectionKey);
    const days = Number(period);
    void refresh;
    void $rates;
    void load(ids, days, token);
    return () => { generation += 1; };
  });
</script>

<details class="bg-card border border-edge rounded-lg px-3 py-2" bind:open={opened} data-testid="workflow-panel">
  <summary class="cursor-pointer text-xs font-semibold text-ink">Workflow intelligence</summary>
  <div class="mt-3 flex flex-col gap-3 text-xs">
    <p class="text-ink-muted">Local, versioned observations for the selected sessions. Tool success and speed do not establish accepted quality or causal savings.</p>
    <div class="flex flex-wrap items-center gap-2">
      <label class="text-ink-muted" for="workflow-period">Compare adjacent UTC periods</label>
      <select id="workflow-period" bind:value={period} class="rounded-sm border border-edge bg-panel px-2 py-1 text-ink">
        <option value="7">7 days each</option><option value="14">14 days each</option><option value="30">30 days each</option>
      </select>
      <button type="button" onclick={() => refresh += 1} disabled={loading} class="rounded-sm border border-edge px-2 py-1 text-ink disabled:opacity-50">Refresh measurements</button>
    </div>
    {#if loading}
      <p role="status" class="text-ink-muted">Measuring durable workflow evidence…</p>
    {:else if error}
      <p role="alert" class="text-amber-500">{error}</p>
    {:else if report}
      <div class="rounded-sm border border-edge bg-panel p-2 text-ink-muted">
        <p>History coverage: {report.coverage_complete ? 'complete' : 'incomplete'} · {report.selected_sessions} selected sessions</p>
        <p>Current normalized evidence: {report.after.analyzed_sessions} analyzed · {report.after.unavailable_sessions} unavailable</p>
        <p>Before {utcDate(report.before.from)}–{utcDate(report.before.to)} · after {utcDate(report.after.from)}–{utcDate(report.after.to)} · UTC</p>
      </div>
      <div class="overflow-x-auto">
        <table class="w-full min-w-96 text-left text-[11px]">
          <thead class="text-ink-muted"><tr><th class="py-1">Measurement</th><th>Before</th><th>After</th><th>Current evidence</th></tr></thead>
          <tbody>
            {#each report.after.ledger_metrics.metrics as metric (metric.id)}
              <tr class="border-t border-edgerow">
                <th class="py-2 font-medium text-ink" title={metric.denominator_is}>{labels[metric.id] ?? metric.id}</th>
                <td class="font-mono text-ink-muted">{value(report.before.ledger_metrics.metrics.find((row) => row.id === metric.id))}</td>
                <td class="font-mono text-ink">{value(metric)}</td>
                <td class="text-ink-muted">{metric.numerator.toLocaleString()} / {metric.denominator.toLocaleString()} · {metric.denominator_is}</td>
              </tr>
            {/each}
            {#each report.after.additional_metrics as metric (metric.id)}
              <tr class="border-t border-edgerow">
                <th class="py-2 font-medium text-ink" title={metric.coverage_is}>{labels[metric.id] ?? metric.id}</th>
                <td class="font-mono text-ink-muted">{value(report.before.additional_metrics.find((row) => row.id === metric.id))}</td>
                <td class="font-mono text-ink">{value(metric)}</td>
                <td class="text-ink-muted">{metric.covered_samples} / {metric.eligible_samples} covered · {metric.denominator_is}{#if metric.missing_data}<span class="block">{metric.missing_data.replaceAll('_', ' ')}</span>{/if}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <p class="text-ink-faint">Metrics v{report.after.ledger_metrics.schema_version} · analyzer v{report.analyzer_version}. Subagent metadata is an observed subset; tool-free turns are excluded from tools-per-turn. Hover a measurement for its coverage rule.</p>
      {#if report.findings.length === 0}
        <p class="text-ink-muted">No findings in these periods. This is not a quality verdict.</p>
      {:else}
        <ul class="flex flex-col gap-2">
          {#each report.findings as finding (finding.id)}
            <li class="rounded-sm border border-edge p-2">
              <p class="font-medium text-ink">{findingRuleTitle(finding.rule_id)} · {finding.comparison.state.replaceAll('_', ' ')} · {finding.provider}</p>
              <p class="text-ink-muted">{finding.before.findings} → {finding.after.findings} observations · {finding.before.sessions} / {finding.after.sessions} session samples</p>
              {#if finding.comparison.observed_change_per_100_calls !== null}<p class="text-ink-muted">Observed change: {finding.comparison.observed_change_per_100_calls.toFixed(1)} likely avoidable calls per 100 calls. This is not realized savings.</p>{/if}
              <p class="text-ink-faint">{finding.comparison.limitations.map((reason) => reason.replaceAll('_', ' ')).join(' · ')}</p>
              <div class="mt-1 flex flex-wrap gap-2">
                {#each finding.evidence as evidence, index (`${evidence.session_id}:${evidence.turn_id}:${index}`)}
                  <button type="button" onclick={() => onReview(evidence.session_id)} class="rounded-sm border border-edge px-2 py-1 text-ink">Review session evidence {index + 1}</button>
                {/each}
              </div>
              {#if finding.evidence_truncated}<p class="text-ink-faint">First 20 evidence anchors shown.</p>{/if}
            </li>
          {/each}
        </ul>
      {/if}
      {#if report.limitations.length > 0}<p class="text-ink-faint">{report.limitations.map((reason) => reason.replaceAll('_', ' ')).join(' · ')}</p>{/if}
    {/if}
  </div>
</details>
