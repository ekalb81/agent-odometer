<script lang="ts">
  import { getSpeedReport, writeExport } from '../lib/ipc';
  import type { SpeedQuery, SpeedReport, SpeedSample } from '../lib/types';
  import { rowsToCsv } from '../lib/sessionProjection';

  interface Props {
    active: boolean;
  }

  let { active }: Props = $props();

  type WindowChoice = 'today' | '7days' | '14days';
  type Measurement = SpeedQuery['measurement'];
  const WINDOWS: { id: WindowChoice; label: string }[] = [
    { id: 'today', label: 'Today' },
    { id: '7days', label: 'Last 7 days' },
    { id: '14days', label: 'Last 14 days' },
  ];
  const MEASUREMENTS: { id: Measurement; label: string }[] = [
    { id: 'turn', label: 'Turn throughput' },
    { id: 'response', label: 'Response throughput' },
  ];

  let windowChoice = $state<WindowChoice>('today');
  let measurement = $state<Measurement>('turn');
  let report = $state<SpeedReport | null>(null);
  let reportWindowKey = $state('');
  let loading = $state(false);
  let requestError = $state<string | null>(null);
  let exportBusy = $state(false);
  let exportError = $state<string | null>(null);
  let selectedModel = $state('all');
  let selectedEffort = $state('all');
  let generation = 0;
  let inFlight = false;
  let queuedRefresh = false;
  let timer: ReturnType<typeof setInterval> | undefined;

  function currentRange(choice: WindowChoice, now = new Date()): { from: string; to: string } {
    const start = new Date(now);
    if (choice === 'today') start.setHours(0, 0, 0, 0);
    if (choice === '7days') start.setDate(start.getDate() - 7);
    if (choice === '14days') start.setDate(start.getDate() - 14);
    return { from: start.toISOString(), to: now.toISOString() };
  }

  function refresh(force = false) {
    if (!active || document.visibilityState === 'hidden') return;
    if (inFlight) {
      queuedRefresh = true;
      loading = true;
      return;
    }
    const nextRange = currentRange(windowChoice);
    const nextMeasurement = measurement;
    const nextReportKey = `${windowChoice}|${nextMeasurement}`;
    if (!force && nextReportKey === reportWindowKey && report?.status === 'ready') return;

    const requestGeneration = ++generation;
    inFlight = true;
    loading = true;
    requestError = null;
    getSpeedReport({ ...nextRange, measurement: nextMeasurement })
      .then((value) => {
        if (requestGeneration !== generation || !active) return;
        if (value.measurement !== nextMeasurement) {
          throw new Error(`Speed report returned ${value.measurement} data for a ${nextMeasurement} query`);
        }
        report = value;
        reportWindowKey = nextReportKey;
        requestError = null;
      })
      .catch((error) => {
        if (requestGeneration === generation && active) requestError = String(error);
      })
      .finally(() => {
        inFlight = false;
        if (requestGeneration === generation) loading = false;
        if (queuedRefresh) {
          queuedRefresh = false;
          refresh(true);
        }
      });
  }

  function onVisibilityChange() {
    if (document.visibilityState === 'visible') {
      if (active && !timer) timer = setInterval(() => refresh(true), 15_000);
      refresh(true);
    } else if (timer) {
      clearInterval(timer);
      timer = undefined;
    }
  }

  $effect(() => {
    const isActive = active;
    const choice = windowChoice;
    const requestedMeasurement = measurement;
    // Read these choices so changing either refreshes immediately.
    void choice;
    void requestedMeasurement;
    if (!isActive) {
      generation += 1;
      queuedRefresh = false;
      loading = false;
      if (timer) clearInterval(timer);
      timer = undefined;
      return;
    }

    refresh(true);
    if (document.visibilityState === 'visible') timer = setInterval(() => refresh(true), 15_000);
    document.addEventListener('visibilitychange', onVisibilityChange);
    return () => {
      generation += 1;
      queuedRefresh = false;
      loading = false;
      if (timer) clearInterval(timer);
      timer = undefined;
      document.removeEventListener('visibilitychange', onVisibilityChange);
    };
  });

  const reportMatchesRange = $derived(Boolean(
    report && report.measurement === measurement && reportWindowKey === `${windowChoice}|${measurement}`,
  ));
  const matchingReport = $derived(reportMatchesRange ? report : null);
  const readyReport = $derived(matchingReport?.status === 'ready' ? matchingReport : null);
  const allRows = $derived(readyReport?.rows ?? []);
  const models = $derived([...new Set(allRows.map((row) => row.model))].sort());
  const efforts = $derived([...new Set(allRows.map((row) => row.reasoning_effort ?? 'unknown'))].sort());

  const filteredRows = $derived(allRows.filter((row) =>
    (selectedModel === 'all' || row.model === selectedModel) &&
    (selectedEffort === 'all' || (row.reasoning_effort ?? 'unknown') === selectedEffort),
  ));
  const fastRows = $derived(filteredRows.filter((row) => row.mode === 'fast'));
  const standardRows = $derived(filteredRows.filter((row) => row.mode === 'standard'));
  const unknownRows = $derived(filteredRows.filter((row) => row.mode === 'unknown'));
  const latestRows = $derived([...filteredRows].sort((a, b) => b.completed_at.localeCompare(a.completed_at)).slice(0, 20));

  function median(values: number[]): number | null {
    if (values.length === 0) return null;
    const sorted = [...values].sort((a, b) => a - b);
    const middle = Math.floor(sorted.length / 2);
    return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
  }

  function weightedTps(rows: SpeedSample[]): number | null {
    const duration = rows.reduce((sum, row) => sum + row.duration_ms, 0);
    return duration > 0 ? rows.reduce((sum, row) => sum + row.output_tokens, 0) / duration * 1_000 : null;
  }

  function totalOutput(rows: SpeedSample[]): number {
    return rows.reduce((sum, row) => sum + row.output_tokens, 0);
  }

  function formatNumber(value: number | null, digits = 0): string {
    return value === null ? '—' : value.toLocaleString(undefined, { maximumFractionDigits: digits, minimumFractionDigits: digits });
  }

  function formatDate(value: string): string {
    return new Date(value).toLocaleString();
  }

  function groupRows(rows: SpeedSample[]) {
    const groups = new Map<string, SpeedSample[]>();
    for (const row of rows) {
      const key = `${row.model}\u0000${row.reasoning_effort ?? 'unknown'}\u0000${row.mode}`;
      const group = groups.get(key);
      if (group) group.push(row);
      else groups.set(key, [row]);
    }
    return [...groups.entries()].map(([key, samples]) => {
      const [model, effort, mode] = key.split('\u0000');
      return { key, model, effort, mode, count: samples.length, output: totalOutput(samples), tps: weightedTps(samples), medianDuration: median(samples.map((sample) => sample.duration_ms)) };
    }).sort((a, b) => a.model.localeCompare(b.model) || a.effort.localeCompare(b.effort) || a.mode.localeCompare(b.mode));
  }

  const groupedRows = $derived(groupRows(filteredRows));

  const EXPORT_COLUMNS = ['completed_at', 'model', 'reasoning_effort', 'mode', 'output_tokens', 'reasoning_tokens', 'duration_ms', 'output_tps', 'visible_tps', 'time_to_first_token_ms', 'timing_source', 'source', 'measurement'] as const;

  async function exportFilteredRows() {
    if (!readyReport || requestError || loading || filteredRows.length === 0) return;
    exportBusy = true;
    exportError = null;
    try {
      const rows = filteredRows.map((sample) => Object.fromEntries(EXPORT_COLUMNS.map((key) => [key,
        key === 'source' ? readyReport.source : key === 'measurement' ? readyReport.measurement : sample[key as keyof SpeedSample],
      ])));
      const content = rowsToCsv(rows);
      await writeExport(`odometer-speed-${windowChoice}-${new Date().toISOString().slice(0, 10)}.csv`, 'csv', content);
    } catch (error) {
      exportError = String(error);
    } finally {
      exportBusy = false;
    }
  }
</script>

<div class="flex min-w-0 max-w-full flex-col gap-3" aria-busy={loading}>
    <div class="flex flex-wrap items-center gap-2">
      <label class="text-[11px] text-ink-muted">Window
        <select class="ml-1 rounded-sm border border-edge bg-surface px-2 py-1 text-xs text-ink" bind:value={windowChoice} aria-label="Speed report window">
          {#each WINDOWS as choice (choice.id)}<option value={choice.id}>{choice.label}</option>{/each}
        </select>
      </label>
      <label class="text-[11px] text-ink-muted">Measure
        <select class="ml-1 rounded-sm border border-edge bg-surface px-2 py-1 text-xs text-ink" bind:value={measurement} aria-label="Speed report measurement">
          {#each MEASUREMENTS as option (option.id)}<option value={option.id}>{option.label}</option>{/each}
        </select>
      </label>
      <label class="text-[11px] text-ink-muted">Model
        <select class="ml-1 max-w-48 rounded-sm border border-edge bg-surface px-2 py-1 text-xs text-ink" bind:value={selectedModel} aria-label="Speed report model" disabled={!readyReport}>
          <option value="all">All models</option>
          {#if selectedModel !== 'all' && !models.includes(selectedModel)}<option value={selectedModel}>{selectedModel} (no current samples)</option>{/if}
          {#each models as model (model)}<option value={model}>{model}</option>{/each}
        </select>
      </label>
      <label class="text-[11px] text-ink-muted">{measurement === 'turn' ? 'Final effort' : 'Effort'}
        <select class="ml-1 rounded-sm border border-edge bg-surface px-2 py-1 text-xs text-ink" bind:value={selectedEffort} aria-label={measurement === 'turn' ? 'Speed report final effort' : 'Speed report effort'} disabled={!readyReport}>
          <option value="all">{measurement === 'turn' ? 'All final efforts' : 'All efforts'}</option>
          {#if selectedEffort !== 'all' && !efforts.includes(selectedEffort)}<option value={selectedEffort}>{selectedEffort === 'unknown' ? 'Unknown effort (no current samples)' : `${selectedEffort} (no current samples)`}</option>{/if}
          {#each efforts as effort (effort)}<option value={effort}>{effort === 'unknown' ? 'Unknown effort' : effort}</option>{/each}
        </select>
      </label>
      <button class="ml-auto rounded-md border border-edge bg-card px-2 py-1 text-[11px] text-ink hover:bg-panel disabled:opacity-50" onclick={() => refresh(true)} disabled={!active || loading}>Refresh</button>
      <button class="rounded-md border border-edge bg-card px-2 py-1 text-[11px] text-ink hover:bg-panel disabled:opacity-50" onclick={exportFilteredRows} disabled={!readyReport || loading || !!requestError || filteredRows.length === 0 || exportBusy}>Export CSV</button>
    </div>

    {#if measurement === 'turn'}
      <p class="text-[10px] text-ink-faint">Turn throughput divides output by full turn duration, including tools, reasoning, and waiting. Fast/Standard mode and final effort reflect configuration. Parent and subagent durations can overlap, so these rates are not accepted-delivery speed. Uses retained Codex session logs across all sessions, regardless of the session filters above.</p>
    {:else}
      <p class="text-[10px] text-ink-faint">Response throughput uses retained local Codex logs across all sessions, regardless of the session filters above. Output rate includes reasoning and request latency, so it is not pure decode speed or task delivery speed. Responses under 100 output tokens or 5 seconds, non-text responses, and built-in tool responses are excluded. Mode shows the tier recorded for each response.</p>
    {/if}

    {#if loading && !readyReport}
      <p class="py-2 text-xs text-ink-faint">Loading local {measurement === 'turn' ? 'turn' : 'response'} timings…</p>
    {:else if matchingReport?.status === 'unavailable'}
      <div class="flex flex-wrap items-center gap-2 text-xs text-ink-faint" role="status"><span>Unavailable: {matchingReport.reason ?? 'local Codex timing data is unavailable'}.</span><button class="underline" onclick={() => refresh(true)}>Retry</button></div>
    {:else if requestError && !readyReport}
      <div class="flex flex-wrap items-center gap-2 text-xs text-neg" role="alert"><span>Could not load speed report: {requestError}</span><button class="underline" onclick={() => refresh(true)}>Retry</button></div>
    {:else if !readyReport}
      <p class="py-2 text-xs text-ink-faint">Waiting for a local speed report.</p>
    {:else}
      {#if loading || requestError}
        <p class="text-[10px] text-amber-500" role={requestError ? 'alert' : 'status'}>{requestError ? `Showing the last report; refresh failed: ${requestError}` : 'Refreshing; the displayed report may be stale.'}</p>
      {/if}
      <div class="grid grid-cols-1 gap-2 sm:grid-cols-3">
        {#each [{ label: measurement === 'turn' ? 'Fast (configured)' : 'Fast (observed)', rows: fastRows }, { label: measurement === 'turn' ? 'Standard (configured)' : 'Standard (observed)', rows: standardRows }] as card (card.label)}
          <div class="rounded-md border border-edge bg-surface p-2">
            <p class="text-[11px] font-semibold text-ink">{card.label}</p>
            <dl class="mt-1 grid grid-cols-2 gap-x-2 gap-y-1 text-[10px] text-ink-muted">
              <dt>{measurement === 'turn' ? 'Turns' : 'Responses'}</dt><dd class="text-right text-ink">{card.rows.length}</dd>
              <dt>Output tokens</dt><dd class="text-right text-ink">{formatNumber(totalOutput(card.rows))}</dd>
              <dt>Duration-weighted output</dt><dd class="text-right text-ink">{formatNumber(weightedTps(card.rows), 1)} tok/s</dd>
              <dt>Median output</dt><dd class="text-right text-ink">{formatNumber(median(card.rows.map((row) => row.output_tokens)))} tokens</dd>
            </dl>
          </div>
        {/each}
        <div class="rounded-md border border-edge bg-surface p-2">
          <p class="text-[11px] font-semibold text-ink">{measurement === 'turn' ? 'Unknown configured mode' : 'Unknown observed mode'}</p>
          <p class="mt-1 text-[10px] text-ink-muted">{unknownRows.length} {measurement === 'turn' ? 'turns' : 'responses'} · {formatNumber(totalOutput(unknownRows))} output tokens</p>
          <p class="mt-1 text-[10px] text-ink-faint">No {measurement === 'turn' ? 'configured' : 'observed'} Fast or Standard mode is inferred.</p>
        </div>
      </div>

      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-[10px] text-ink-faint">
        <span>{readyReport.scanned_rows.toLocaleString()} local {measurement === 'turn' ? 'turns' : 'responses'} scanned</span><span>{readyReport.excluded_count.toLocaleString()} excluded</span>
        {#if readyReport.truncated}<span class="text-amber-500">Report was truncated</span>{/if}
        <span>Updated {formatDate(readyReport.generated_at)}</span>
      </div>

      {#if groupedRows.length > 0}
        <div class="min-w-0 max-w-full overflow-x-auto">
          <table class="w-full min-w-[560px] text-[10px] font-mono">
            <caption class="pb-1 text-left font-sans text-[11px] font-semibold text-ink">Comparable {measurement === 'turn' ? 'turn' : 'response'} groups</caption>
            <thead class="text-ink-muted"><tr><th class="text-left">{measurement === 'turn' ? 'Model / final effort' : 'Model / effort'}</th><th class="text-left">{measurement === 'turn' ? 'Configured mode' : 'Observed mode'}</th><th class="text-right">{measurement === 'turn' ? 'Turns' : 'Responses'}</th><th class="text-right">Output tokens</th><th class="text-right">Weighted tok/s</th><th class="text-right">Median duration</th></tr></thead>
            <tbody>{#each groupedRows as row (row.key)}<tr class="border-t border-edgerow"><td class="py-1 text-ink">{row.model} · {row.effort === 'unknown' ? 'unknown effort' : row.effort}</td><td>{row.mode}</td><td class="text-right">{row.count}</td><td class="text-right">{formatNumber(row.output)}</td><td class="text-right">{formatNumber(row.tps, 1)}</td><td class="text-right">{row.medianDuration === null ? '—' : `${formatNumber(row.medianDuration / 1_000, 1)}s`}</td></tr>{/each}</tbody>
          </table>
        </div>
      {/if}
      {#if allRows.length === 0 && readyReport.reason}
        <p class="text-xs text-ink-faint" role="status">{readyReport.reason}</p>
      {/if}

      <div class="min-w-0 max-w-full overflow-x-auto">
        <table class="w-full min-w-[620px] text-[10px] font-mono">
          <caption class="pb-1 text-left font-sans text-[11px] font-semibold text-ink">Latest {measurement === 'turn' ? 'turns' : 'responses'} (up to 20)</caption>
          <thead class="text-ink-muted"><tr><th class="text-left">Completed</th><th class="text-left">{measurement === 'turn' ? 'Model / final effort' : 'Model / effort'}</th><th class="text-left">{measurement === 'turn' ? 'Configured mode' : 'Observed mode'}</th><th class="text-right">Output</th><th class="text-right">Reasoning</th><th class="text-right">Duration</th><th class="text-right">Output tok/s</th><th class="text-right">Visible tok/s</th><th class="text-right">TTFT</th></tr></thead>
          <tbody>{#each latestRows as row}<tr class="border-t border-edgerow"><td class="py-1 pr-2 text-ink">{formatDate(row.completed_at)}</td><td class="pr-2">{row.model} · {row.reasoning_effort ?? 'unknown effort'}</td><td>{row.mode}</td><td class="text-right">{formatNumber(row.output_tokens)}</td><td class="text-right">{formatNumber(row.reasoning_tokens)}</td><td class="text-right">{formatNumber(row.duration_ms / 1_000, 1)}s</td><td class="text-right">{formatNumber(row.output_tps, 1)}</td><td class="text-right">{formatNumber(row.visible_tps, 1)}</td><td class="text-right">{row.time_to_first_token_ms === null ? '—' : `${formatNumber(row.time_to_first_token_ms / 1_000, 2)}s`}</td></tr>{/each}</tbody>
        </table>
        {#if latestRows.length === 0}<p class="py-2 text-xs text-ink-faint">No {measurement === 'turn' ? 'turns' : 'responses'} match these filters.</p>{/if}
      </div>
      {#if exportError}<p class="text-[11px] text-neg" role="alert">Export failed: {exportError}</p>{/if}
    {/if}
</div>
