<script lang="ts">
  import { compareToolImpact, listToolImpactTargets, writeExport } from '../lib/ipc';
  import type { ToolDimensionKind, ToolImpactCohort, ToolImpactResult, ToolImpactTarget } from '../lib/types';
  import { topDimensionValues, type DimensionTotals } from '../lib/toolDimensions';
  import { dimensionExportRows, rowsToCsv } from '../lib/sessionProjection';

  interface Props {
    active?: boolean;
    sessionIds: string[];
    from: string | null;
    to: string | null;
    windowLabel: string;
    /** Issue #44 open-set dimension totals, already aggregated by the
     *  caller across the sessions in view. */
    dimensionTotals?: DimensionTotals;
    /** Whether at least one provider in view supports each dimension kind
     *  — `false` renders "unavailable", never a fabricated zero. */
    dimensionAvailability?: Record<ToolDimensionKind, boolean>;
  }

  let {
    active = true,
    sessionIds,
    from,
    to,
    windowLabel,
    dimensionTotals = {},
    dimensionAvailability = {
      mcp_server: false,
      shell_family: false,
      language: false,
      context_source: false,
    },
  }: Props = $props();

  const DIMENSION_PANELS: { kind: ToolDimensionKind; label: string; unit: 'calls' | 'tokens' }[] = [
    { kind: 'mcp_server', label: 'MCP servers', unit: 'calls' },
    { kind: 'shell_family', label: 'Shell command families', unit: 'calls' },
    { kind: 'language', label: 'Languages', unit: 'calls' },
    { kind: 'context_source', label: 'Context source', unit: 'tokens' },
  ];

  const CONTEXT_SOURCE_LABELS: Record<string, string> = {
    conversation_cache: 'Conversation / cache reuse',
    newly_cached_context: 'Newly established context',
    unknown: 'Unattributed (system, tools, files, or new conversation)',
  };

  function dimensionValueLabel(kind: ToolDimensionKind, value: string): string {
    return kind === 'context_source' ? (CONTEXT_SOURCE_LABELS[value] ?? value) : value;
  }

  const dimensionRows = $derived(
    DIMENSION_PANELS.map((panel) => ({
      ...panel,
      available: dimensionAvailability[panel.kind],
      rows: topDimensionValues(dimensionTotals, panel.kind),
    })),
  );

  function compactNumber(value: number): string {
    return Math.round(value).toLocaleString();
  }

  // Export wiring for the dimension-attribution panel (issue #44). Exports
  // exactly what is rendered above: one row per (dimension_kind,
  // dimension_value) pair, currently in view — no raw arguments, tool
  // output, prompts, or unrestricted paths ever enter `dimensionTotals` in
  // the first place (see `dimensionExportRows`'s doc comment), so there is
  // nothing additional to redact on the way out.
  let dimensionExportBusy = $state(false);
  let dimensionExportError = $state<string | null>(null);

  async function exportDimensions(format: 'csv' | 'json') {
    dimensionExportBusy = true;
    dimensionExportError = null;
    try {
      const rows = dimensionExportRows(dimensionTotals);
      const content = format === 'json' ? `${JSON.stringify(rows, null, 2)}\n` : rowsToCsv(rows);
      await writeExport(`odometer-tool-dimensions-${new Date().toISOString().slice(0, 10)}.${format}`, format, content);
    } catch (error) {
      dimensionExportError = String(error);
    } finally {
      dimensionExportBusy = false;
    }
  }

  let targets = $state<ToolImpactTarget[]>([]);
  let selectedTargetId = $state('');
  let result = $state<ToolImpactResult | null>(null);
  let loadingTargets = $state(false);
  let loadingComparison = $state(false);
  let targetError = $state<string | null>(null);
  let comparisonError = $state<string | null>(null);
  let targetRequestGeneration = 0;
  let comparisonRequestGeneration = 0;

  const targetId = (target: ToolImpactTarget) => `${target.kind}:${target.key}`;
  const providerTargets = $derived(targets.filter((target) => target.kind === 'provider'));
  const toolTargets = $derived(targets.filter((target) => target.kind === 'tool'));
  const selectedTarget = $derived(
    targets.find((target) => targetId(target) === selectedTargetId) ?? null,
  );

  $effect(() => {
    const generation = ++targetRequestGeneration;
    const ids = sessionIds;
    const rangeFrom = from;
    const rangeTo = to;
    if (!active || ids.length === 0) {
      targets = [];
      selectedTargetId = '';
      result = null;
      loadingTargets = false;
      return;
    }
    loadingTargets = true;
    targetError = null;
    listToolImpactTargets(rangeFrom, rangeTo, ids)
      .then((value) => {
        if (!active || generation !== targetRequestGeneration) return;
        targets = value;
        const selectionStillExists = value.some(
          (target) => targetId(target) === selectedTargetId,
        );
        if (!selectionStillExists) {
          selectedTargetId = value[0] ? targetId(value[0]) : '';
        }
      })
      .catch((reason) => {
        if (generation === targetRequestGeneration) targetError = String(reason);
      })
      .finally(() => {
        if (generation === targetRequestGeneration) loadingTargets = false;
      });
    return () => {
      if (generation === targetRequestGeneration) targetRequestGeneration += 1;
    };
  });

  $effect(() => {
    const generation = ++comparisonRequestGeneration;
    const target = selectedTarget;
    const ids = sessionIds;
    const rangeFrom = from;
    const rangeTo = to;
    if (!active || !target || ids.length === 0) {
      result = null;
      loadingComparison = false;
      return;
    }
    loadingComparison = true;
    comparisonError = null;
    compareToolImpact(target.kind, target.key, rangeFrom, rangeTo, ids)
      .then((value) => {
        if (active && generation === comparisonRequestGeneration) result = value;
      })
      .catch((reason) => {
        if (generation === comparisonRequestGeneration) comparisonError = String(reason);
      })
      .finally(() => {
        if (generation === comparisonRequestGeneration) loadingComparison = false;
      });
    return () => {
      if (generation === comparisonRequestGeneration) comparisonRequestGeneration += 1;
    };
  });

  const useMatched = $derived(Boolean(result && result.matched_pairs >= 3));
  const assisted = $derived<ToolImpactCohort | null>(
    result ? (useMatched ? result.matched_observed : result.observed) : null,
  );
  const baseline = $derived<ToolImpactCohort | null>(
    result ? (useMatched ? result.matched_baseline : result.baseline) : null,
  );

  function average(total: number, count: number): number | null {
    return count > 0 ? total / count : null;
  }

  function percentDelta(value: number | null, reference: number | null): string {
    if (value === null || reference === null || reference === 0) return '—';
    const delta = ((value / reference) - 1) * 100;
    return `${delta >= 0 ? '+' : ''}${delta.toFixed(1)}%`;
  }

  function compact(value: number | null): string {
    if (value === null) return '—';
    return Math.round(value).toLocaleString();
  }

  function elapsed(value: number | null): string {
    if (value === null) return '—';
    if (value < 60_000) return `${(value / 1_000).toFixed(1)}s`;
    return `${(value / 60_000).toFixed(1)}m`;
  }

  const rows = $derived((() => {
    if (!assisted || !baseline) return [];
    const assistedTokens = average(assisted.tokens.total_tokens, assisted.turn_count);
    const baselineTokens = average(baseline.tokens.total_tokens, baseline.turn_count);
    const assistedOutput = average(assisted.tokens.output_tokens, assisted.turn_count);
    const baselineOutput = average(baseline.tokens.output_tokens, baseline.turn_count);
    const assistedDuration = average(assisted.total_duration_ms, assisted.duration_sample_count);
    const baselineDuration = average(baseline.total_duration_ms, baseline.duration_sample_count);
    const assistedTtft = average(assisted.total_ttft_ms, assisted.ttft_sample_count);
    const baselineTtft = average(baseline.total_ttft_ms, baseline.ttft_sample_count);
    const assistedCalls = average(assisted.tool_metrics.calls, assisted.turn_count);
    const baselineCalls = average(baseline.tool_metrics.calls, baseline.turn_count);
    return [
      { label: 'Tokens / turn', baseline: compact(baselineTokens), assisted: compact(assistedTokens), delta: percentDelta(assistedTokens, baselineTokens) },
      { label: 'Output tokens / turn', baseline: compact(baselineOutput), assisted: compact(assistedOutput), delta: percentDelta(assistedOutput, baselineOutput) },
      { label: 'Elapsed / turn', baseline: elapsed(baselineDuration), assisted: elapsed(assistedDuration), delta: percentDelta(assistedDuration, baselineDuration) },
      { label: 'First token', baseline: elapsed(baselineTtft), assisted: elapsed(assistedTtft), delta: percentDelta(assistedTtft, baselineTtft) },
      { label: 'Tool calls / turn', baseline: compact(baselineCalls), assisted: compact(assistedCalls), delta: percentDelta(assistedCalls, baselineCalls) },
    ];
  })());
</script>

<details class="bg-card border border-edge rounded-lg px-3 py-2">
  <summary class="cursor-pointer text-xs font-semibold text-ink">
    Tool impact comparison · observed use · {windowLabel}
  </summary>
  <div class="mt-2 flex flex-col gap-2">
    {#if loadingTargets}
      <p class="text-xs text-ink-faint py-2">Finding observed tools and providers…</p>
    {:else if targetError}
      <p class="text-xs text-neg py-2" role="alert">{targetError}</p>
    {:else if targets.length === 0}
      <p class="text-xs text-ink-faint py-2">
        No tool calls were linked to turns in this view.
      </p>
    {:else}
      <label class="flex flex-wrap items-center gap-2 text-[11px] text-ink-muted">
        <span class="font-semibold text-ink">Compare</span>
        <select
          class="min-w-0 max-w-full rounded-sm border border-edge bg-surface px-2 py-1 text-xs text-ink"
          bind:value={selectedTargetId}
          aria-label="Tool impact target"
        >
          {#if providerTargets.length > 0}
            <optgroup label="Providers">
              {#each providerTargets as target (targetId(target))}
                <option value={targetId(target)}>
                  {target.label} · {target.turn_count} {target.turn_count === 1 ? 'turn' : 'turns'}
                </option>
              {/each}
            </optgroup>
          {/if}
          {#if toolTargets.length > 0}
            <optgroup label="Tools">
              {#each toolTargets as target (targetId(target))}
                <option value={targetId(target)}>
                  {target.label} · {target.turn_count} {target.turn_count === 1 ? 'turn' : 'turns'}
                </option>
              {/each}
            </optgroup>
          {/if}
        </select>
      </label>

      {#if loadingComparison}
        <p class="text-xs text-ink-faint py-2">Comparing observed and baseline turns…</p>
      {:else if comparisonError}
        <p class="text-xs text-neg py-2" role="alert">{comparisonError}</p>
      {:else if !result || result.observed.turn_count === 0}
        <p class="text-xs text-ink-faint py-2">
          The selected target was not observed in this view.
        </p>
      {:else if result.baseline.turn_count === 0}
        <p class="text-xs text-ink-faint py-2">
          The target was observed, but this view has no turns without it to use as a baseline.
        </p>
      {:else}
        <div class="flex flex-wrap items-baseline gap-x-3 gap-y-1 text-[11px] text-ink-muted">
          <span class="font-semibold text-ink">{useMatched ? `${result.matched_pairs} matched pairs` : 'All observed turns'}</span>
          <span>{useMatched ? 'same harness, model, and task category; nearest in time' : `${result.observed.turn_count} observed · ${result.baseline.turn_count} not observed`}</span>
        </div>
        <div class="overflow-x-auto">
          <table class="w-full text-[11px] font-mono">
            <thead class="text-ink-muted">
              <tr><th class="text-left py-1">Metric</th><th class="text-right">Not observed</th><th class="text-right">Observed</th><th class="text-right">Difference</th></tr>
            </thead>
            <tbody>
              {#each rows as row (row.label)}
                <tr class="border-t border-edgerow">
                  <td class="py-1.5 text-ink">{row.label}</td>
                  <td class="text-right">{row.baseline}</td>
                  <td class="text-right font-semibold">{row.assisted}</td>
                  <td class="text-right">{row.delta}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        <p class="text-[10px] text-ink-faint">
          Duration coverage: observed {assisted?.duration_sample_count ?? 0}/{assisted?.turn_count ?? 0} turns · not observed {baseline?.duration_sample_count ?? 0}/{baseline?.turn_count ?? 0}. Whole overlapping turns are compared; differences are observational, not proof that the selected tool or provider caused them.
        </p>
        {#if result.warnings.length > 0}
          <p class="text-[10px] text-amber-500">{result.warnings.join(' · ')}</p>
        {/if}
      {/if}
    {/if}
  </div>
</details>

<details class="bg-card border border-edge rounded-lg px-3 py-2">
  <summary class="cursor-pointer text-xs font-semibold text-ink">
    Tool, MCP, shell &amp; context attribution · {windowLabel}
  </summary>
  <div class="mt-2 flex flex-col gap-3">
    <div class="flex flex-wrap items-center justify-between gap-2">
      <span class="text-[10px] text-ink-faint">Export the rows below — one row per dimension value currently in view.</span>
      <div class="flex items-center gap-2">
        <button
          class="px-2 py-1 rounded-md border border-edge bg-card hover:bg-panel disabled:opacity-50 text-[11px]"
          disabled={dimensionExportBusy || Object.keys(dimensionTotals).length === 0}
          onclick={() => exportDimensions('csv')}
        >Export CSV</button>
        <button
          class="px-2 py-1 rounded-md border border-edge bg-card hover:bg-panel disabled:opacity-50 text-[11px]"
          disabled={dimensionExportBusy || Object.keys(dimensionTotals).length === 0}
          onclick={() => exportDimensions('json')}
        >Export JSON</button>
      </div>
    </div>
    {#if dimensionExportError}
      <p class="text-[11px] text-neg" role="alert">{dimensionExportError}</p>
    {/if}
    {#each dimensionRows as panel (panel.kind)}
      <div class="flex flex-col gap-1">
        <p class="text-[11px] font-semibold text-ink">{panel.label}</p>
        {#if !panel.available}
          <p class="text-[11px] text-ink-faint italic">
            Unavailable — no provider in this view is corroborated to supply this dimension.
          </p>
        {:else if panel.rows.length === 0}
          <p class="text-[11px] text-ink-faint">No {panel.label.toLowerCase()} data in this view.</p>
        {:else}
          <div class="overflow-x-auto">
            <table class="w-full text-[11px] font-mono">
              <thead class="text-ink-muted">
                <tr>
                  <th class="text-left py-0.5">Value</th>
                  <th class="text-right">{panel.unit === 'tokens' ? 'Tokens' : 'Calls'}</th>
                  {#if panel.unit === 'calls'}
                    <th class="text-right">Failures</th>
                    <th class="text-right">Output bytes</th>
                  {/if}
                </tr>
              </thead>
              <tbody>
                {#each panel.rows as row (row.value)}
                  <tr class="border-t border-edgerow">
                    <td class="py-1 text-ink">{dimensionValueLabel(panel.kind, row.value)}</td>
                    <td class="text-right font-semibold">
                      {compactNumber(panel.unit === 'tokens' ? row.metrics.tokens : row.metrics.calls)}
                    </td>
                    {#if panel.unit === 'calls'}
                      <td class="text-right">{compactNumber(row.metrics.failures)}</td>
                      <td class="text-right">{compactNumber(row.metrics.output_bytes)}</td>
                    {/if}
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      </div>
    {/each}
    <p class="text-[10px] text-ink-faint">
      MCP server identity and shell command family are privacy-bounded: MCP server names come from the tool call itself, and shell families are matched against a fixed allowlist (unmatched commands appear as "other") — raw command arguments are never retained. Context source is derived from token accounting and reconciles to the provider-reported cache dimensions where available; "unattributed" covers system instructions, tool schemas, file reads, and new conversation content, which cannot be told apart from token totals alone.
    </p>
  </div>
</details>
