<script lang="ts">
  import { accountingUnavailable } from '../lib/accountingAvailability';
  import { historyStore } from '../lib/stores/history.svelte';
  import { correlateEvents } from '../lib/ipc';
  import { formatCredits, harnessCurrency } from '../lib/currency';
  import {
    comparisonReady,
    nextCorrelationBoundaryDelay,
    rapidRevertLabels,
    readyUsageContext,
  } from '../lib/configTimeline';
  import { rates } from '../lib/stores/rates';
  import { sessionsStore } from '../lib/stores/sessions.svelte';
  import { providersStore } from '../lib/stores/providers.svelte';
  import type { EventCorrelation, ExternalEvent } from '../lib/types';

  interface Props { active?: boolean; events: ExternalEvent[]; title?: string; }
  let { active = true, events, title = 'Configuration timeline' }: Props = $props();
  let correlations = $state<EventCorrelation[]>([]);
  let loading = $state(false);
  let requestGeneration = 0;
  let error = $state<string | null>(null);
  let boundaryRefresh = $state(0);
  let displayEvents = $derived(events.slice(-50).reverse());
  let revertLabels = $derived.by(() => rapidRevertLabels(events.slice(-50)));

  $effect(() => {
    const generation = ++requestGeneration;
    const source = events;
    const mutation = sessionsStore.mutationLog.generation;
    const boundary = boundaryRefresh;
    const rateCard = $rates;
    const history = historyStore.status;
    // Event descriptions remain inspectable; numeric correlations require a
    // fresh proof whenever their data, window, history or pricing changes.
    correlations = [];
    error = null;
    const current = () => active && generation === requestGeneration
      && source === events && mutation === sessionsStore.mutationLog.generation
      && boundary === boundaryRefresh && rateCard === $rates
      && history === historyStore.status;
    if (!active) {
      loading = false;
      return;
    }
    loading = true;
    let boundaryTimer: ReturnType<typeof setTimeout> | null = null;
    const requestTimer = setTimeout(() => {
      if (!current()) return;
      const recent = source.slice(-50).reverse();
      (recent.length > 0 ? correlateEvents({ events: recent, before_days: 7, after_days: 7, exclude_confounded: false, include_subagents: true }) : Promise.resolve({ results: [] }))
        .then((result) => {
          if (!current()) return;
          correlations = result.results;
          const delay = nextCorrelationBoundaryDelay(result.results);
          if (delay !== null) {
            boundaryTimer = setTimeout(() => {
              if (current()) boundaryRefresh += 1;
            }, delay);
          }
        })
        .catch((reason) => { if (current()) { correlations = []; error = accountingUnavailable(reason); } })
        .finally(() => { if (current()) loading = false; });
    }, 250);
    return () => {
      if (generation === requestGeneration) requestGeneration += 1;
      clearTimeout(requestTimer);
      if (boundaryTimer !== null) clearTimeout(boundaryTimer);
    };
  });

  function costs(item: EventCorrelation): string {
    const before = item.before.pricing_by_harness;
    const after = item.after.pricing_by_harness;
    const delta = (left: number | undefined, right: number | undefined, currency: string, signed = false) => {
      if (left === undefined || right === undefined) return 'unavailable';
      const difference = right - left;
      return `${signed && difference >= 0 ? '+' : ''}${signed && currency === 'credits' ? difference.toFixed(2) : formatCredits(difference, currency)}`;
    };
    const codexCurrency = $rates ? harnessCurrency($rates, 'codex') : 'credits';
    const claudeCurrency = $rates ? harnessCurrency($rates, 'claude_code') : 'USD';
    return `${codexCurrency} ${delta(before?.codex?.plan.total, after?.codex?.plan.total, codexCurrency, true)} · Codex flat API ${delta(before?.codex?.api?.total, after?.codex?.api?.total, 'USD')} · Claude ${delta(before?.claude_code?.plan.total, after?.claude_code?.plan.total, claudeCurrency)}`;
  }

  function countContext(item: EventCorrelation): string {
    const sessions = `${item.before.session_count.toLocaleString()} → ${item.after.session_count.toLocaleString()} sessions`;
    const turns = `${item.before.turn_count.toLocaleString()} → ${item.after.turn_count.toLocaleString()} turns`;
    return `${sessions} · ${turns}`;
  }

  function collectingContext(item: EventCorrelation): string {
    const end = new Date(item.after_window_end);
    const remainingMs = Math.max(0, end.getTime() - Date.now());
    const remainingDays = Math.ceil(remainingMs / 86_400_000);
    const remaining = remainingDays > 1 ? `about ${remainingDays} days remaining` : remainingDays === 1 ? 'about 1 day remaining' : 'less than a day remaining';
    return `After period still collecting data until ${end.toLocaleString()} (${remaining}).`;
  }

</script>

{#if loading || events.length > 0 || error}
  <details class="bg-card border border-edge rounded-lg px-3 py-2">
    <summary class="cursor-pointer text-xs font-semibold text-ink">{title} · {events.length} recent changes</summary>
    {#if loading}<p class="text-xs text-ink-faint py-2">Loading local change history…</p>{/if}
    {#if error}<p data-testid="accounting-correlation-status" role="status" class="text-xs text-neg py-2">{error}</p>{/if}
    <div class="mt-2 max-h-56 overflow-y-auto space-y-1.5">
      {#each displayEvents as event (event.id)}
        {@const correlation = correlations.find((item) => item.event.id === event.id)}
        <div class="border-t border-edgerow pt-1.5 text-[11px]">
          <div class="flex gap-2"><span class="font-mono text-ink-faint">{new Date(event.timestamp).toLocaleString()}</span><span class="font-semibold text-ink">{event.kind}</span><span class="text-ink-muted">{providersStore.displayName(event.metadata.harness)}</span></div>
          <div class="text-ink-faint">{event.scope ? 'project' : 'global'} · {event.metadata.safe_diff ?? 'redacted content change'}</div>
          {#if revertLabels.has(event.id)}<div class="text-amber-500">{revertLabels.get(event.id)}</div>{/if}
          {#if correlation}
            {@const usage = readyUsageContext(correlation)}
            <div class="text-ink-2">Observed samples · {countContext(correlation)}</div>
            {#if !correlation.after_window_complete}
              <div class="mt-1 rounded-sm border border-edge bg-panel px-2 py-1 text-ink-muted">{collectingContext(correlation)} Comparisons use seven full days on each side.</div>
            {/if}
            {#if comparisonReady(correlation)}
              <div class="text-ink-2">Tokens {correlation.token_delta >= 0 ? '+' : ''}{correlation.token_delta.toLocaleString()} · sessions {correlation.session_delta >= 0 ? '+' : ''}{correlation.session_delta} · {costs(correlation)}</div>
            {:else}
              <div class="text-ink-faint">Outcome deltas hidden until the after period is complete and both sides contain at least {correlation.minimum_session_count} sessions.</div>
            {/if}
            {#if usage}<div class="text-ink-muted">{usage}</div>{/if}
            {#if correlation.warnings.length > 0}<div class="text-amber-500">{correlation.warnings.join(' · ')}</div>{/if}
          {/if}
        </div>
      {/each}
    </div>
  </details>
{/if}
