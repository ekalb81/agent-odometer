<script lang="ts">
  import { listExternalEvents, correlateEvents } from '../lib/ipc';
  import { apiCostFromBuckets, creditsFromBuckets, formatCredits } from '../lib/credits';
  import { rates } from '../lib/stores/rates';
  import type { EventCorrelation, ExternalEvent } from '../lib/types';

  interface Props { active?: boolean; }
  let { active = true }: Props = $props();
  let events = $state<ExternalEvent[]>([]);
  let correlations = $state<EventCorrelation[]>([]);
  let loading = $state(false);
  let loaded = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!active || loading || loaded) return;
    loading = true;
    loaded = true;
    listExternalEvents().then((all) => {
      events = all.filter((event) => event.source === 'config').slice(-50).reverse();
      return events.length > 0 ? correlateEvents({ events, before_days: 7, after_days: 7, exclude_confounded: false, include_subagents: true }) : { results: [] };
    }).then((result) => { correlations = result.results; })
      .catch((reason) => { error = String(reason); })
      .finally(() => { loading = false; });
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
</script>

{#if loading || events.length > 0 || error}
  <details class="bg-card border border-edge rounded-lg px-3 py-2">
    <summary class="cursor-pointer text-xs font-semibold text-ink">Configuration timeline · {events.length} recent changes</summary>
    {#if loading}<p class="text-xs text-ink-faint py-2">Loading local change history…</p>{/if}
    {#if error}<p class="text-xs text-neg py-2">{error}</p>{/if}
    <div class="mt-2 max-h-56 overflow-y-auto space-y-1.5">
      {#each events as event (event.id)}
        {@const correlation = correlations.find((item) => item.event.id === event.id)}
        <div class="border-t border-edgerow pt-1.5 text-[11px]">
          <div class="flex gap-2"><span class="font-mono text-ink-faint">{new Date(event.timestamp).toLocaleString()}</span><span class="font-semibold text-ink">{event.kind}</span><span class="text-ink-muted">{event.metadata.harness}</span></div>
          {#if correlation}
            <div class="text-ink-2">Tokens {correlation.token_delta >= 0 ? '+' : ''}{correlation.token_delta.toLocaleString()} · sessions {correlation.session_delta >= 0 ? '+' : ''}{correlation.session_delta} · {costs(correlation)}</div>
            {#if correlation.warnings.length > 0}<div class="text-amber-500">{correlation.warnings.join(' · ')}</div>{/if}
          {/if}
        </div>
      {/each}
    </div>
  </details>
{/if}
