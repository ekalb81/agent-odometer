<script lang="ts">
  import { getWorkflowReport, previewControlledAction, recordWorkflowMeasurement, setWorkflowFindingSuppression } from '../lib/ipc';
  import { rates } from '../lib/stores/rates';
  import { projectStore } from '../lib/stores/projects.svelte';
  import { findingRuleTitle } from '../lib/optimization';
  import type { ControlledActionPreview, WorkflowFinding, WorkflowMetric, WorkflowReport } from '../lib/types';

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
  let actionGeneration = 0;
  let saving = $state(false);
  let hypotheticalReduction = $state('10');
  let actionPreview = $state<{ findingId: string; value: ControlledActionPreview } | null>(null);
  let actionError = $state<{ findingId: string; message: string } | null>(null);
  const afterCalls = $derived(report?.after.drilldowns.filter((row) => row.dimension === 'project')
    .reduce((sum, row) => sum + row.tool_calls, 0) ?? 0);
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
  async function load(ids: string[], days: number, token: number, record = false): Promise<void> {
    const previousWindow = record ? report?.after : null;
    loading = true;
    error = null;
    report = null;
    actionGeneration++;
    actionPreview = null;
    actionError = null;
    const end = new Date();
    const start = new Date(end.getTime() - days * 86_400_000 + 1);
    try {
      const next = await (record ? recordWorkflowMeasurement : getWorkflowReport)({ session_ids: ids,
        from: previousWindow?.from ?? start.toISOString(), to: previousWindow?.to ?? end.toISOString() });
      if (token !== generation) return;
      report = next;
    } catch {
      if (token !== generation) return;
      error = 'Workflow analysis could not be loaded. Check history readiness, or select fewer sessions and retry.';
    } finally {
      if (token === generation) loading = false;
    }
  }
  async function suppression(finding: WorkflowFinding): Promise<void> {
    if (!finding.lifecycle || saving) return;
    const token = ++generation;
    actionGeneration++;
    actionPreview = null;
    actionError = null;
    saving = true;
    error = null;
    try {
      await setWorkflowFindingSuppression({ provider: finding.provider, project_id: finding.project_id,
        rule_id: finding.rule_id, expected_revision: finding.lifecycle.revision,
        suppressed: !finding.lifecycle.suppressed });
      if (token === generation) refresh += 1;
    } catch {
      if (token === generation) error = 'Finding changed or suppression could not be saved. Refresh before trying again.';
    } finally { saving = false; }
  }
  async function previewAction(finding: WorkflowFinding): Promise<void> {
    if (!report || !finding.lifecycle || saving) return;
    const token = generation;
    const actionToken = ++actionGeneration;
    actionPreview = null;
    actionError = null;
    try {
      const value = await previewControlledAction({ kind: 'workflow_remediation', draft: {
        scope: { session_ids: JSON.parse(selectionKey), from: report.after.from, to: report.after.to },
        finding_id: finding.id, expected_finding_revision: finding.lifecycle.revision,
      } });
      if (token === generation && actionToken === actionGeneration) actionPreview = { findingId: finding.id, value };
    } catch {
      if (token === generation && actionToken === actionGeneration) actionError = { findingId: finding.id,
        message: 'This action preview is stale or unavailable. Refresh the measurement.' };
    }
  }
  $effect(() => {
    const token = ++generation;
    if (!active || !opened) return;
    void projectStore.revision;
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
      <select id="workflow-period" bind:value={period} disabled={saving || loading} class="rounded-sm border border-edge bg-panel px-2 py-1 text-ink">
        <option value="7">7 days each</option><option value="14">14 days each</option><option value="30">30 days each</option>
      </select>
      <button type="button" onclick={() => refresh += 1} disabled={loading || saving} class="rounded-sm border border-edge px-2 py-1 text-ink disabled:opacity-50">Refresh measurements</button>
      <button type="button" onclick={() => void load(JSON.parse(selectionKey), Number(period), ++generation, true)} disabled={loading || saving || !report} class="rounded-sm border border-edge px-2 py-1 text-ink disabled:opacity-50">Record measurement</button>
    </div>
    <p class="text-ink-faint">Viewing is read-only. Record measurement saves only finding states and observation times locally; it changes no agent configuration.</p>
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
        <p class="mb-1 text-ink-faint sm:hidden">Scroll sideways for the evidence column.</p>
        <details class="mb-2 rounded-sm border border-edge p-2">
          <summary class="cursor-pointer text-ink">Local setup health</summary>
          {#if report.setup_health}
            <p class="mt-2 text-ink-muted">Source configuration: {report.setup_health.source_configuration_valid ? 'valid' : 'needs attention'}. Root availability is separate from durable evidence coverage.</p>
            <ul class="mt-1 flex flex-col gap-1 text-ink-muted">
              {#each report.setup_health.providers as provider (provider.provider)}
                <li>{provider.provider} · {provider.state.replaceAll('_', ' ')} · {provider.available_roots}/{provider.configured_roots} roots available · {provider.parse_failures} parse failures · {provider.durable_sessions} durable sessions{#if provider.fallback_pricing_used} · fallback pricing{/if}{#if provider.reasons.length} · {provider.reasons.map((reason) => reason.replaceAll('_', ' ')).join(', ')}{/if}</li>
              {/each}
            </ul>
          {:else}<p class="mt-2 text-ink-muted">Setup health is unavailable because saved source configuration could not be read.</p>{/if}
        </details>
        <table class="w-full min-w-[660px] text-left text-[11px]">
          <thead class="text-ink-muted"><tr><th class="px-2 py-1">Measurement</th><th class="px-2">Before</th><th class="px-2">After</th><th class="px-2">Current evidence</th></tr></thead>
          <tbody>
            {#each report.after.ledger_metrics.metrics as metric (metric.id)}
              <tr class="border-t border-edgerow">
                <th class="px-2 py-2 font-medium text-ink" title={metric.denominator_is}>{labels[metric.id] ?? metric.id}</th>
                <td class="px-2 font-mono text-ink-muted">{value(report.before.ledger_metrics.metrics.find((row) => row.id === metric.id))}</td>
                <td class="px-2 font-mono text-ink">{value(metric)}</td>
                <td class="px-2 text-ink-muted">{metric.numerator.toLocaleString()} / {metric.denominator.toLocaleString()} · {metric.denominator_is}</td>
              </tr>
            {/each}
            {#each report.after.additional_metrics as metric (metric.id)}
              <tr class="border-t border-edgerow">
                <th class="px-2 py-2 font-medium text-ink" title={metric.coverage_is}>{labels[metric.id] ?? metric.id}</th>
                <td class="px-2 font-mono text-ink-muted">{value(report.before.additional_metrics.find((row) => row.id === metric.id))}</td>
                <td class="px-2 font-mono text-ink">{value(metric)}</td>
                <td class="px-2 text-ink-muted">{metric.covered_samples} / {metric.eligible_samples} covered · {metric.denominator_is}{#if metric.missing_data}<span class="block">{metric.missing_data.replaceAll('_', ' ')}</span>{/if}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <p class="text-ink-faint">Metrics v{report.after.ledger_metrics.schema_version} · analyzer v{report.analyzer_version}. Subagent metadata is an observed subset; tool-free turns are excluded from tools-per-turn. Hover a measurement for its coverage rule.</p>
      <details class="rounded-sm border border-edge p-2">
        <summary class="cursor-pointer text-ink">Project, model and task category evidence</summary>
        <p class="mt-2 text-ink-muted">Current period counts. Project and model calls come from the ledger; categories count turns with the existing classifier. These dimensions have different denominators.</p>
        <ul class="mt-2 text-ink-muted">{#each report.after.drilldowns as row (`${row.dimension}:${row.value}`)}
          <li>{row.dimension}: {row.value}{#if row.dimension === 'project'} · {row.sessions} sessions · {row.tool_calls} calls{:else if row.dimension === 'model'} · {row.tool_calls} calls{:else} · {row.classified_turns} classified turns{/if}</li>
        {/each}</ul>
      </details>
      <div class="rounded-sm border border-edge p-2 text-ink-muted">
        <label for="workflow-hypothetical">Illustrative call reduction assumption</label>
        <select id="workflow-hypothetical" bind:value={hypotheticalReduction} class="ml-2 rounded-sm border border-edge bg-panel px-2 py-1 text-ink">
          <option value="10">10%</option><option value="25">25%</option>
        </select>
        <p class="mt-1">{afterCalls} observed calls × {hypotheticalReduction}% = {Math.floor(afterCalls * Number(hypotheticalReduction) / 100)} hypothetical calls avoided over this period.</p>
        <p class="text-ink-faint">This is a scenario, not measured or causal savings. Token, cost, quality and delivery effects are unknown.</p>
      </div>
      {#if report.findings.length === 0}
        <p class="text-ink-muted">No findings in these periods. This is not a quality verdict.</p>
      {:else}
        <ul class="flex flex-col gap-2">
          {#each report.findings as finding (finding.id)}
            <li class="rounded-sm border border-edge p-2">
              <p class="font-medium text-ink">{findingRuleTitle(finding.rule_id)} · {finding.comparison.state.replaceAll('_', ' ')} · {finding.provider}</p>
              <p class="text-ink-muted">{finding.before.findings} → {finding.after.findings} observations · {finding.before.sessions} / {finding.after.sessions} session samples</p>
              {#if finding.lifecycle}
                <p class="text-ink-faint">Recorded measurement observations: {utcDate(finding.lifecycle.first_observed_at)}–{utcDate(finding.lifecycle.last_observed_at)} UTC · revision {finding.lifecycle.revision}. These are not occurrence dates.</p>
                <button type="button" disabled={saving || loading} onclick={() => void suppression(finding)} class="mt-1 rounded-sm border border-edge px-2 py-1 text-ink disabled:opacity-50">{finding.lifecycle.suppressed ? 'Unsuppress finding' : 'Suppress finding'}</button>
                {#if finding.rule_id === 'repeated-read' && finding.comparison.comparable && !finding.lifecycle.suppressed}
                  <button type="button" disabled={saving || loading} onclick={() => void previewAction(finding)} class="ml-2 mt-1 rounded-sm border border-edge px-2 py-1 text-ink disabled:opacity-50">Preview future action</button>
                {/if}
              {:else}<p class="text-ink-faint">Not recorded. Record a measurement before editing suppression.</p>{/if}
              {#if actionPreview?.findingId === finding.id}
                <div class="mt-2 space-y-1 rounded-sm border border-edge bg-panel p-2 text-ink-muted" role="status">
                  <p>Dry run only · {actionPreview.value.target_type.replaceAll('_', ' ')}</p>
                  <p>{actionPreview.value.proposed_change}</p>
                  <p>{actionPreview.value.backup_requirement}</p>
                  <p>{actionPreview.value.postcondition_requirement}</p>
                  <p>Apply and undo are unavailable pending a separate security review.</p>
                </div>
              {:else if actionError?.findingId === finding.id}<p role="alert" class="text-amber-500">{actionError.message}</p>{/if}
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
      {#if report.historical_findings.length}
        <details class="rounded-sm border border-edge p-2">
          <summary class="cursor-pointer text-ink">Previously recorded findings outside this measurement ({report.historical_findings.length})</summary>
          <p class="mt-2 text-ink-muted">Different project scopes, missing rules and absent current observations do not prove resolution. Suppression stays with its original scope.</p>
          <ul class="mt-2 text-ink-faint">{#each report.historical_findings as finding (finding.id)}
            <li>{findingRuleTitle(finding.rule_id)} · {finding.provider} · {finding.lifecycle.state.replaceAll('_', ' ')} · last measured {utcDate(finding.lifecycle.last_observed_at)} UTC</li>
          {/each}</ul>
        </details>
      {/if}
      {#if report.limitations.length > 0}<p class="text-ink-faint">{report.limitations.map((reason) => reason.replaceAll('_', ' ')).join(' · ')}</p>{/if}
    {/if}
  </div>
</details>
