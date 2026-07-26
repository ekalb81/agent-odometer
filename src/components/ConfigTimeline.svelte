<script lang="ts">
  import { correlateEvents } from '../lib/ipc';
  import { apiCostFromBuckets, creditsFromBuckets, formatCredits } from '../lib/credits';
  import {
    comparisonReady,
    nextCorrelationBoundaryDelay,
    rapidRevertLabels,
    readyUsageContext,
  } from '../lib/configTimeline';
  import { rates } from '../lib/stores/rates';
  import { sessionsStore } from '../lib/stores/sessions.svelte';
  import type { EventCorrelation, ExternalEvent } from '../lib/types';

  interface Props { active?: boolean; events: ExternalEvent[]; title?: string; }
  let { active = true, events, title = 'Configuration timeline' }: Props = $props();
  let correlations = $state<EventCorrelation[]>([]);
  let loading = $state(false);
  let requestGeneration = 0;
  let error = $state<string | null>(null);
<<<<<<< HEAD
  const RAPID_REVERT_MS = 5 * 60_000;
  let displayEvents = $derived(events.slice(-50).reverse());
  let revertLabels = $derived.by(() => rapidRevertLabels(events.slice(-50)));

  function sizeChange(event: ExternalEvent): [number, number] | null {
    const match = event.metadata.safe_diff?.match(/size ([\d,]+) -> ([\d,]+) bytes/);
    return match ? [Number(match[1].replaceAll(',', '')), Number(match[2].replaceAll(',', ''))] : null;
  }

  function rapidRevertLabels(source: ExternalEvent[]): Map<string, string> {
    const labels = new Map<string, string>();
    const ordered = [...source].sort((a, b) => Date.parse(a.timestamp) - Date.parse(b.timestamp));
    for (let index = 1; index < ordered.length; index += 1) {
      const before = ordered[index - 1];
      const after = ordered[index];
      const elapsed = Date.parse(after.timestamp) - Date.parse(before.timestamp);
      const beforeSize = sizeChange(before);
      const afterSize = sizeChange(after);
      const sameSurface = before.metadata.path_id && before.metadata.path_id === after.metadata.path_id
        && before.metadata.harness === after.metadata.harness && before.scope === after.scope;
      if (sameSurface && elapsed >= 0 && elapsed <= RAPID_REVERT_MS && beforeSize && afterSize
        && beforeSize[0] === afterSize[1] && beforeSize[1] === afterSize[0]) {
        const duration = elapsed < 60_000 ? `${Math.max(1, Math.round(elapsed / 1_000))}s` : `${Math.round(elapsed / 60_000)}m`;
        labels.set(before.id, `Rapid size-revert pattern · reverted ${duration} later`);
        labels.set(after.id, `Rapid size-revert pattern · restores the prior size after ${duration}`);
      }
    }
    return labels;
  }
=======
  let boundaryRefresh = $state(0);
  let displayEvents = $derived(events.slice(-50).reverse());
  let revertLabels = $derived.by(() => rapidRevertLabels(events.slice(-50)));
>>>>>>> origin/main

  $effect(() => {
    const generation = ++requestGeneration;
    const source = events;
    // Correlations depend on the current session corpus as well as the event
    // list. Store mutations replace this Map, providing a bounded refresh key.
    void sessionsStore.map;
    void boundaryRefresh;
    if (!active) {
      loading = false;
      return;
    }
    loading = true;
    error = null;
    let boundaryTimer: ReturnType<typeof setTimeout> | null = null;
    const requestTimer = setTimeout(() => {
      const recent = source.slice(-50).reverse();
      (recent.length > 0 ? correlateEvents({ events: recent, before_days: 7, after_days: 7, exclude_confounded: false, include_subagents: true }) : Promise.resolve({ results: [] }))
        .then((result) => {
          if (!active || generation !== requestGeneration) return;
          correlations = result.results;
          const delay = nextCorrelationBoundaryDelay(result.results);
          if (delay !== null) {
            boundaryTimer = setTimeout(() => {
              if (active && generation === requestGeneration) boundaryRefresh += 1;
            }, delay);
          }
        })
        .catch((reason) => { if (generation === requestGeneration) error = String(reason); })
        .finally(() => { if (generation === requestGeneration) loading = false; });
    }, 250);
    return () => {
      if (generation === requestGeneration) requestGeneration += 1;
      clearTimeout(requestTimer);
      if (boundaryTimer !== null) clearTimeout(boundaryTimer);
    };
  });

  function costs(item: EventCorrelation): string {
    if (!$rates) return '';
    const price = (observation: EventCorrelation['before']) => {
      const codex = observation.buckets_by_harness.codex ?? [];
      const claude = observation.buckets_by_harness.claude_code ?? [];
      return {
        credits: creditsFromBuckets(codex, $rates!, 'codex').total,
        codexUsd: apiCostFromBuckets(codex, $rates!, 'codex')?.total ?? 0,
        claudeUsd: creditsFromBuckets(claude, $rates!, 'claude_code').total,
      };
    };
    const before = price(item.before); const after = price(item.after);
    return `credits ${after.credits - before.credits >= 0 ? '+' : ''}${(after.credits - before.credits).toFixed(2)} · Codex ${formatCredits(after.codexUsd - before.codexUsd, 'USD')} · Claude ${formatCredits(after.claudeUsd - before.claudeUsd, 'USD')}`;
  }

<<<<<<< HEAD
  function usageContext(item: EventCorrelation): string {
    const tokensPerTurn = (observation: EventCorrelation['before']) =>
      observation.turn_count > 0 ? observation.tokens.total_tokens / observation.turn_count : null;
    const minutesPerSession = (observation: EventCorrelation['before']) =>
      observation.session_count > 0 ? observation.session_duration_ms / observation.session_count / 60_000 : null;
    const beforeTokens = tokensPerTurn(item.before);
    const afterTokens = tokensPerTurn(item.after);
    const beforeMinutes = minutesPerSession(item.before);
    const afterMinutes = minutesPerSession(item.after);
    const missingSides = (beforeValue: number, afterValue: number, noun: string) => {
      if (beforeValue === 0 && afterValue === 0) return `no qualifying ${noun} on either side`;
      if (beforeValue === 0) return `no qualifying ${noun} before the change`;
      return `no qualifying ${noun} after the change`;
    };
    const tokenText = beforeTokens === null || afterTokens === null
      ? `tokens/turn unavailable (${missingSides(item.before.turn_count, item.after.turn_count, 'turns')})`
      : `tokens/turn ${Math.round(beforeTokens).toLocaleString()} → ${Math.round(afterTokens).toLocaleString()}`;
    const durationText = beforeMinutes === null || afterMinutes === null
      ? `session length unavailable (${missingSides(item.before.session_count, item.after.session_count, 'sessions')})`
      : `avg session ${beforeMinutes.toFixed(0)}m → ${afterMinutes.toFixed(0)}m`;
    return `${tokenText} · ${durationText}`;
  }

  function countContext(item: EventCorrelation): string {
    const sessions = `${item.before.session_count.toLocaleString()} → ${item.after.session_count.toLocaleString()} sessions`;
    const turns = `${item.before.turn_count.toLocaleString()} → ${item.after.turn_count.toLocaleString()} turns`;
    return `${sessions} · ${turns}`;
  }

=======
  function countContext(item: EventCorrelation): string {
    const sessions = `${item.before.session_count.toLocaleString()} → ${item.after.session_count.toLocaleString()} sessions`;
    const turns = `${item.before.turn_count.toLocaleString()} → ${item.after.turn_count.toLocaleString()} turns`;
    return `${sessions} · ${turns}`;
  }

>>>>>>> origin/main
  function collectingContext(item: EventCorrelation): string {
    const end = new Date(item.after_window_end);
    const remainingMs = Math.max(0, end.getTime() - Date.now());
    const remainingDays = Math.ceil(remainingMs / 86_400_000);
    const remaining = remainingDays > 1 ? `about ${remainingDays} days remaining` : remainingDays === 1 ? 'about 1 day remaining' : 'less than a day remaining';
    return `After period still collecting data until ${end.toLocaleString()} (${remaining}).`;
  }

<<<<<<< HEAD
  function comparisonReady(item: EventCorrelation): boolean {
    return item.after_window_complete && item.sample_ready;
  }
=======
>>>>>>> origin/main
</script>

{#if loading || events.length > 0 || error}
  <details class="bg-card border border-edge rounded-lg px-3 py-2">
    <summary class="cursor-pointer text-xs font-semibold text-ink">{title} · {events.length} recent changes</summary>
    {#if loading}<p class="text-xs text-ink-faint py-2">Loading local change history…</p>{/if}
    {#if error}<p class="text-xs text-neg py-2">{error}</p>{/if}
    <div class="mt-2 max-h-56 overflow-y-auto space-y-1.5">
      {#each displayEvents as event (event.id)}
        {@const correlation = correlations.find((item) => item.event.id === event.id)}
        <div class="border-t border-edgerow pt-1.5 text-[11px]">
          <div class="flex gap-2"><span class="font-mono text-ink-faint">{new Date(event.timestamp).toLocaleString()}</span><span class="font-semibold text-ink">{event.kind}</span><span class="text-ink-muted">{event.metadata.harness}</span></div>
          <div class="text-ink-faint">{event.scope ? 'project' : 'global'} · {event.metadata.safe_diff ?? 'redacted content change'}</div>
          {#if revertLabels.has(event.id)}<div class="text-amber-500">{revertLabels.get(event.id)}</div>{/if}
          {#if correlation}
<<<<<<< HEAD
=======
            {@const usage = readyUsageContext(correlation)}
>>>>>>> origin/main
            <div class="text-ink-2">Observed samples · {countContext(correlation)}</div>
            {#if !correlation.after_window_complete}
              <div class="mt-1 rounded border border-edge bg-panel px-2 py-1 text-ink-muted">{collectingContext(correlation)} Comparisons use seven full days on each side.</div>
            {/if}
            {#if comparisonReady(correlation)}
              <div class="text-ink-2">Tokens {correlation.token_delta >= 0 ? '+' : ''}{correlation.token_delta.toLocaleString()} · sessions {correlation.session_delta >= 0 ? '+' : ''}{correlation.session_delta} · {costs(correlation)}</div>
            {:else}
              <div class="text-ink-faint">Outcome deltas hidden until the after period is complete and both sides contain at least {correlation.minimum_session_count} sessions.</div>
            {/if}
<<<<<<< HEAD
            <div class="text-ink-muted">{usageContext(correlation)}</div>
=======
            {#if usage}<div class="text-ink-muted">{usage}</div>{/if}
>>>>>>> origin/main
            {#if correlation.warnings.length > 0}<div class="text-amber-500">{correlation.warnings.join(' · ')}</div>{/if}
          {/if}
        </div>
      {/each}
    </div>
  </details>
{/if}
