<script lang="ts">
  import { correlateEvents } from '../lib/ipc';
  import { apiCostFromBuckets, creditsFromBuckets, formatCredits } from '../lib/credits';
  import { rates } from '../lib/stores/rates';
  import type { EventCorrelation, ExternalEvent } from '../lib/types';

  interface Props { active?: boolean; events: ExternalEvent[]; }
  let { active = true, events }: Props = $props();
  let correlations = $state<EventCorrelation[]>([]);
  let loading = $state(false);
  let requestGeneration = 0;
  let error = $state<string | null>(null);

  $effect(() => {
    const generation = ++requestGeneration;
    const source = events;
    if (!active) return;
    loading = true;
    error = null;
    const recent = source.slice(-50).reverse();
    (recent.length > 0 ? correlateEvents({ events: recent, before_days: 7, after_days: 7, exclude_confounded: false, include_subagents: true }) : Promise.resolve({ results: [] }))
      .then((result) => { if (active && generation === requestGeneration) correlations = result.results; })
      .catch((reason) => { if (generation === requestGeneration) error = String(reason); })
      .finally(() => { if (generation === requestGeneration) loading = false; });
    return () => { if (generation === requestGeneration) requestGeneration += 1; };
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

  function usageContext(item: EventCorrelation): string {
    const tokensPerTurn = (observation: EventCorrelation['before']) =>
      observation.turn_count > 0 ? observation.tokens.total_tokens / observation.turn_count : null;
    const minutesPerSession = (observation: EventCorrelation['before']) =>
      observation.session_count > 0 ? observation.session_duration_ms / observation.session_count / 60_000 : null;
    const beforeTokens = tokensPerTurn(item.before);
    const afterTokens = tokensPerTurn(item.after);
    const beforeMinutes = minutesPerSession(item.before);
    const afterMinutes = minutesPerSession(item.after);
    const tokenText = beforeTokens === null || afterTokens === null
      ? 'tokens/turn unavailable'
      : `tokens/turn ${Math.round(beforeTokens).toLocaleString()} → ${Math.round(afterTokens).toLocaleString()}`;
    const durationText = beforeMinutes === null || afterMinutes === null
      ? 'session length unavailable'
      : `avg session ${beforeMinutes.toFixed(0)}m → ${afterMinutes.toFixed(0)}m`;
    return `${tokenText} · ${durationText}`;
  }
</script>

{#if loading || events.length > 0 || error}
  <details class="bg-card border border-edge rounded-lg px-3 py-2">
    <summary class="cursor-pointer text-xs font-semibold text-ink">Configuration timeline · {events.length} recent changes</summary>
    {#if loading}<p class="text-xs text-ink-faint py-2">Loading local change history…</p>{/if}
    {#if error}<p class="text-xs text-neg py-2">{error}</p>{/if}
    <div class="mt-2 max-h-56 overflow-y-auto space-y-1.5">
      {#each events.slice(-50).reverse() as event (event.id)}
        {@const correlation = correlations.find((item) => item.event.id === event.id)}
        <div class="border-t border-edgerow pt-1.5 text-[11px]">
          <div class="flex gap-2"><span class="font-mono text-ink-faint">{new Date(event.timestamp).toLocaleString()}</span><span class="font-semibold text-ink">{event.kind}</span><span class="text-ink-muted">{event.metadata.harness}</span></div>
          <div class="text-ink-faint">{event.scope ? 'project' : 'global'} · {event.metadata.safe_diff ?? 'redacted content change'}</div>
          {#if correlation}
            <div class="text-ink-2">Tokens {correlation.token_delta >= 0 ? '+' : ''}{correlation.token_delta.toLocaleString()} · sessions {correlation.session_delta >= 0 ? '+' : ''}{correlation.session_delta} · {costs(correlation)}</div>
            <div class="text-ink-muted">{usageContext(correlation)}</div>
            {#if correlation.warnings.length > 0}<div class="text-amber-500">{correlation.warnings.join(' · ')}</div>{/if}
          {/if}
        </div>
      {/each}
    </div>
  </details>
{/if}
