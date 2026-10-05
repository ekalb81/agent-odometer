<script lang="ts">
  import { untrack } from 'svelte';
  import { getHistoryStatus, sessionsInRanges } from '../lib/ipc';
  import { calendarDays, activityDays, calendarEvidence, type ActivityDay, type ActivityMetric, type CalendarZone } from '../lib/calendarActivity';
  import { historyStore } from '../lib/stores/history.svelte';
  import { scanStore } from '../lib/stores/scan.svelte';
  import { sessionsStore } from '../lib/stores/sessions.svelte';
  import { MutationAccumulator, RangeDataCache } from '../lib/rangeData';
  import type { RangeTotals } from '../lib/types';
  import { activitySummary } from '../lib/activitySummary';
  import ActivitySummary from './ActivitySummary.svelte';

  let { active = true, from = null, to = null, sessionIds, harness = null, projects = [], projectLoaded = true, projectError = null, onbucket }: {
    active?: boolean; from?: string | null; to?: string | null; sessionIds: string[]; onbucket: (day: ActivityDay) => void;
    harness?: string | null;
    projects?: { key: string; label: string; sessionIds: string[] }[]; projectLoaded?: boolean; projectError?: string | null;
  } = $props();
  let zone = $state<CalendarZone>('local');
  let metric = $state<ActivityMetric>('tokens');
  let projectKey = $state('');
  let days = $state<ActivityDay[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let retry = $state(0);
  let summary = $state<{ svg: string; markdown: string } | null>(null);
  let summaryError = $state('');
  let epoch = 0;
  let jobGeneration = 0;
  let layoutKey = '';
  let queue: Promise<void> = Promise.resolve();
  const cache = new RangeDataCache();
  const mutations = new MutationAccumulator();
  const evidence = $derived(calendarEvidence(historyStore.status, scanStore.status.complete));
  const localZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  const maximum = $derived(Math.max(1, ...days.map((day) => day[metric])));
  const total = $derived(days.reduce((sum, day) => sum + day[metric], 0));
  const months = $derived([...new Set(days.map((day) => day.month))]);
  const metricLabel = $derived(metric === 'tokens' ? 'tokens' : 'tool calls');
  const selectedProject = $derived(projects.find((project) => project.key === projectKey));
  const selectedIds = $derived(projectKey ? selectedProject?.sessionIds ?? [] : sessionIds);

  $effect(() => {
    void days; void loading; void error; void active; void from; void to;
    void metric; void zone; void harness; void projectKey; void selectedIds; void evidence;
    summary = null; summaryError = '';
  });

  function previewSummary(): void {
    if (loading || error || !days.length || (evidence.state !== 'complete' && evidence.state !== 'partial')) return;
    try { summary = activitySummary({ days, metric, zone, harness, selectedProject: !!projectKey, coverage: evidence.state }); }
    catch (reason) { summaryError = String(reason).replace(/^Error: /, ''); }
  }

  $effect(() => {
    const job = ++jobGeneration;
    const ids = selectedIds;
    const fromBound = from; const toBound = to; const selectedZone = zone;
    const state = evidence.state;
    const scanComplete = scanStore.status.complete;
    void retry;
    mutations.observe(sessionsStore.mutationLog);
    if (!active || state === 'pending' || state === 'unavailable') {
      epoch++;
      days = []; loading = false; error = null; cache.invalidate(); layoutKey = '';
      return;
    }
    if (projectKey && (!projectLoaded || projectError || !selectedProject)) {
      epoch++; days = []; loading = false;
      error = projectError ? 'Project assignments are unavailable. Retry project resolution before using this scope.' : 'The selected project is unavailable in the current provider and session filters.';
      return;
    }
    let bounds;
    try { bounds = calendarDays(fromBound, toBound, selectedZone); }
    catch (reason) { epoch++; error = String(reason).replace(/^Error: /, ''); days = []; loading = false; return; }
    // An open current day advances with streaming events; its layout stays
    // stable, and the established mutation cache fetches changed IDs only.
    const key = `${selectedZone}|${fromBound}|${toBound}|${bounds.map((day) => day.date).join(',')}|${ids.join(',')}`;
    if (key !== layoutKey) { epoch++; days = []; cache.invalidate(); layoutKey = key; }
    const request = epoch;
    loading = true; error = null;
    const timer = setTimeout(() => {
      queue = queue.then(async () => {
        if (request !== epoch || job !== jobGeneration) return;
        try {
          // The archive can become unhealthy after opening. Read current
          // provenance before every query rather than trusting mount state.
          const history = await getHistoryStatus();
          if (request !== epoch) return;
          if (calendarEvidence(history, scanComplete).state !== state) {
            historyStore.set(history);
            cache.invalidate(); days = []; loading = false;
            return;
          }
          const drained = mutations.drain();
          const plan = cache.plan({ rangesKey: key, ids, changedIds: drained.changedIds, removedIds: drained.removedIds });
          const fetchIds = plan.mode === 'full' ? ids : plan.mode === 'delta' ? plan.fetchIds : [];
          let fetched: Record<string, RangeTotals>[] | null = null;
          if (plan.mode === 'full' || fetchIds.length) {
            fetched = [];
            // IPC permits at most 64 windows. Serialized chunks keep backend
            // work bounded without creating a separate aggregation authority.
            for (let index = 0; index < bounds.length; index += 64) {
              const batch = bounds.slice(index, index + 64);
              const result = await sessionsInRanges(batch.map(({ from, to }) => ({ from, to })), fetchIds);
              if (request !== epoch) { cache.invalidate(); return; }
              if (result.length !== batch.length) throw new Error('Activity response is incomplete. Retry the range.');
              fetched.push(...result);
            }
          }
          const result = plan.mode === 'full' ? cache.applyFull(key, ids, fetched!)
            : plan.mode === 'delta' ? cache.applyDelta(plan.fetchIds, drained.removedIds, fetched) : cache.current();
          if (request !== epoch) return;
          days = activityDays(bounds, result!); loading = false;
        } catch (reason) {
          if (request !== epoch) return;
          cache.invalidate(); days = []; loading = false;
          error = `Activity unavailable: ${String(reason).replace(/^Error: /, '')}`;
        }
      }).catch(() => {});
    }, untrack(() => days.length ? 250 : 0));
    return () => clearTimeout(timer);
  });

  function bucketLabel(day: ActivityDay): string {
    return `${day.date}: ${day[metric].toLocaleString()} ${metricLabel}${evidence.state === 'partial' ? ' recorded' : ''}. Show ${day.metricSessionIds[metric].length} event sessions.`;
  }

  function selectBucket(day: ActivityDay): void {
    onbucket({ ...day, sessionIds: day.metricSessionIds[metric] });
  }
</script>

<section class="bg-card border border-edge rounded-lg px-3 py-2" data-testid="calendar-activity">
  <div class="calendar-controls flex flex-wrap items-center gap-3">
    <h3 class="text-xs font-semibold text-ink">Calendar &amp; daily trend</h3>
    <label class="text-[11px] text-ink-muted">Metric
      <select aria-label="Activity metric" class="ml-1 bg-panel border border-edge rounded-sm text-ink" bind:value={metric}>
        <option value="tokens">Tokens</option><option value="tool_calls">Tool calls</option>
      </select>
    </label>
    <label class="text-[11px] text-ink-muted">Calendar timezone
      <select class="ml-1 bg-panel border border-edge rounded-sm text-ink" bind:value={zone}>
        <option value="local">Local · {localZone}</option><option value="utc">UTC</option>
      </select>
    </label>
    <label class="text-[11px] text-ink-muted">Activity project
      <select class="ml-1 bg-panel border border-edge rounded-sm text-ink" bind:value={projectKey} disabled={!projectLoaded || !!projectError}>
        <option value="">All projects</option>
        {#if projectKey && !selectedProject}<option value={projectKey}>Unavailable project</option>{/if}
        {#each projects as project (project.key)}<option value={project.key}>{project.label}</option>{/each}
      </select>
    </label>
    <button type="button" class="text-[11px] text-accent underline disabled:opacity-50" disabled={loading || !!error || !days.length || !active || evidence.state === 'pending' || evidence.state === 'unavailable'} onclick={previewSummary}>Preview summary card</button>
  </div>
  {#if summaryError}<p role="alert" class="mt-1 text-[11px] text-neg">{summaryError}</p>{/if}
  <p class="mt-1 text-[10px] text-ink-faint">Uses the current provider and session filters · {selectedProject?.label ?? 'all projects'}. {from ? 'Selected date range' : 'Latest 90 calendar days'} · {zone === 'utc' ? 'UTC' : localZone}. Choose dates above to inspect up to 366 days. Click a day to show its event sessions.</p>
  {#if projectError}<p class="mt-1 text-[11px] text-neg">Project choices unavailable: {projectError}</p>{/if}
  <p class="mt-1 text-[11px] text-ink-muted" role="status">{evidence.message}</p>
  {#if error}
    <p role="alert" class="mt-1 text-[11px] text-neg">{error} <button class="underline" type="button" onclick={() => retry++}>Retry</button></p>
  {:else if loading && !days.length}
    <p role="status" class="mt-2 text-[11px] text-ink-muted">Loading recorded activity…</p>
  {:else if days.length}
    <p class="mt-2 text-xs text-ink font-mono" data-testid="calendar-total">{total.toLocaleString()} {metricLabel}{evidence.state === 'partial' ? ' recorded · partial history' : ''} · {days[0].date} – {days[days.length - 1].date}{loading ? ' · updating' : ''}</p>
    {#if total === 0}<p class="mt-1 text-[11px] text-ink-muted">{evidence.state === 'partial' ? 'No activity recorded in this range; missing activity is unknown.' : 'Zero activity in the recorded history for this range.'}</p>{/if}
    <div class="calendar-months mt-2" aria-label="Calendar heatmap">
      {#each months as month (month)}
        <div><h4 class="text-[11px] text-ink-muted mb-1">{month}</h4>
          <div class="calendar-grid">
            {#each ['S', 'M', 'T', 'W', 'T', 'F', 'S'] as weekday}<span class="text-[10px] text-ink-faint" aria-hidden="true">{weekday}</span>{/each}
            {#each days.filter((day) => day.month === month) as day, index (day.date)}
              <button type="button" class="calendar-day" aria-label={bucketLabel(day)} title={bucketLabel(day)}
                style:grid-column-start={index === 0 ? day.weekday + 1 : undefined}
                style:background={day[metric] === 0 ? 'var(--panel)' : `color-mix(in srgb, var(--accent) ${Math.max(15, Math.round(day[metric] / maximum * 70))}%, var(--panel))`}
                onclick={() => selectBucket(day)}>{day.day}</button>
            {/each}
          </div>
        </div>
      {/each}
    </div>
    <p class="mt-2 text-[10px] text-ink-faint">Color shows relative {metricLabel}; each day exposes its exact count. Volumes are descriptive, not productivity scores.</p>
    <h4 class="mt-2 text-[11px] font-semibold text-ink-muted">Daily trend · {metricLabel}</h4>
    <div class="daily-trend" aria-label="Daily activity trend">
      {#each days as day (day.date)}
        <button type="button" class="trend-day" aria-label={`Trend ${bucketLabel(day)}`} title={bucketLabel(day)} onclick={() => selectBucket(day)}>
          <span class="trend-track"><span class="trend-fill" style:height={`${day[metric] / maximum * 100}%`}></span></span>
          <span class="text-[9px] text-ink-faint">{day.date.slice(5)}</span>
        </button>
      {/each}
    </div>
  {/if}
</section>

{#if summary}<ActivitySummary {...summary} onclose={() => summary = null} />{/if}

<style>
  .calendar-controls label { display: flex; flex-wrap: wrap; align-items: center; gap: 0.25rem; min-width: 0; max-width: 100%; }
  .calendar-controls select { max-width: 100%; margin-left: 0; }
  .calendar-months { display: flex; flex-wrap: wrap; gap: 1rem; max-height: 18rem; overflow-y: auto; }
  .calendar-grid { display: grid; grid-template-columns: repeat(7, 1.75rem); gap: 0.25rem; text-align: center; }
  .calendar-day { height: 1.75rem; border: 1px solid var(--border); border-radius: 0.25rem; font-size: 0.625rem; color: var(--text); }
  button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .daily-trend { display: flex; gap: 0.25rem; overflow-x: auto; margin-top: 0.5rem; padding-bottom: 0.5rem; }
  .trend-day { display: flex; flex-direction: column; gap: 0.25rem; flex: 1 0 2.25rem; min-width: 2.25rem; }
  .trend-track { height: 4rem; display: flex; align-items: flex-end; background: var(--panel); border-bottom: 1px solid var(--border); }
  .trend-fill { display: block; width: 100%; background: var(--accent); }
</style>
