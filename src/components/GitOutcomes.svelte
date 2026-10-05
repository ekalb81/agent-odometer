<script lang="ts">
  import { correlateEvents, listExternalEvents, scanGitOutcomes } from '../lib/ipc';
  import { accountingUnavailable } from '../lib/accountingAvailability';
  import { historyStore } from '../lib/stores/history.svelte';
  import { sessionsStore } from '../lib/stores/sessions.svelte';
  import { rates } from '../lib/stores/rates';
  import type { EventCorrelation, GitOutcome, GitOutcomeKind } from '../lib/types';

  let outcomes = $state<GitOutcome[]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let comparisonError = $state<string | null>(null);
  let correlations = $state<Record<string, EventCorrelation>>({});
  let postWindowHours = $state(24);
  const kinds: GitOutcomeKind[] = ['kept', 'reverted', 'abandoned', 'ambiguous', 'not_evaluated'];
  const correlationBatchSize = 2_000;
  let generation = 0;

  $effect(() => {
    void sessionsStore.mutationLog.generation;
    void historyStore.status;
    void $rates;
    void postWindowHours;
    generation++;
    correlations = {};
    comparisonError = null;
    error = null;
    busy = false;
    return () => { generation++; };
  });

  async function scan() {
    const request = ++generation;
    const mutation = sessionsStore.mutationLog.generation;
    const history = historyStore.status;
    const rateCard = $rates;
    const hours = postWindowHours;
    const current = () => request === generation
      && mutation === sessionsStore.mutationLog.generation
      && history === historyStore.status && rateCard === $rates && hours === postWindowHours;
    busy = true; error = null; comparisonError = null; correlations = {};
    let comparing = false;
    try {
      const scanned = await scanGitOutcomes(hours);
      if (!current()) return;
      outcomes = scanned;
      const sessionIds = new Set(outcomes.map((outcome) => outcome.session_id));
      comparing = true;
      const events = (await listExternalEvents()).filter((event) => event.source === 'git' && sessionIds.has(event.metadata.session_id));
      if (!current()) return;
      const results: EventCorrelation[] = [];
      for (let offset = 0; offset < events.length; offset += correlationBatchSize) {
        const batch = await correlateEvents({
          events: events.slice(offset, offset + correlationBatchSize),
          before_days: 7,
          after_days: 7,
          exclude_confounded: false,
          include_subagents: true,
        });
        if (!current()) return;
        results.push(...batch.results);
      }
      correlations = Object.fromEntries(results.map((item) => [item.event.metadata.session_id, item]));
    }
    catch (reason) {
      if (!current()) return;
      correlations = {};
      if (comparing) comparisonError = accountingUnavailable(reason);
      else error = String(reason);
    }
    finally { if (current()) busy = false; }
  }
</script>

<details class="bg-card border border-edge rounded-lg px-3 py-2">
  <summary class="cursor-pointer text-xs font-semibold text-ink">Local git outcomes</summary>
  <div class="mt-2 flex items-center gap-2">
    <button class="px-3 py-1.5 rounded-md border border-edge bg-panel hover:bg-app text-xs disabled:opacity-50" disabled={busy} onclick={scan}>{busy ? 'Scanning…' : 'Evaluate local repositories'}</button>
    <label class="text-[11px] text-ink-muted">Post-session window
      <input class="ml-1 w-16 rounded-sm border border-edge bg-app px-1.5 py-1 font-mono" type="number" min="0" max="8760" step="1" bind:value={postWindowHours} disabled={busy} /> h
    </label>
    <span class="text-[11px] text-ink-faint">HEAD-reachable commits · no remotes or worktree changes</span>
  </div>
  {#if error}<p class="text-xs text-neg mt-2">{error}</p>{/if}
  {#if comparisonError}<p data-testid="accounting-git-outcomes-status" role="status" class="text-xs text-neg mt-2">{comparisonError}</p>{/if}
  {#if outcomes.length > 0}
    <div class="grid grid-cols-5 gap-2 mt-2">
      {#each kinds as kind}
        <div class="border border-edgerow rounded-sm px-2 py-1 text-[11px]"><span class="text-ink-muted">{kind.replace('_', ' ')}</span><div class="font-mono font-semibold">{outcomes.filter((outcome) => outcome.kind === kind).length}</div></div>
      {/each}
    </div>
    <details class="mt-2"><summary class="text-[11px] cursor-pointer text-ink-muted">Session evidence</summary>
      <div class="max-h-40 overflow-y-auto mt-1">
        {#each outcomes as outcome (outcome.session_id)}
          {@const correlation = correlations[outcome.session_id]}
          <div class="text-[11px] border-t border-edgerow py-1"><span class="font-mono">{outcome.session_id.slice(0, 10)}</span> · <span class="font-semibold">{outcome.kind}</span> · <span class="text-ink-muted">{outcome.evidence}</span>{#if correlation}<div class="text-ink-2">7d token delta {correlation.token_delta >= 0 ? '+' : ''}{correlation.token_delta.toLocaleString()} · session delta {correlation.session_delta >= 0 ? '+' : ''}{correlation.session_delta}{#if correlation.warnings.length} · <span class="text-amber-500">{correlation.warnings.join(' · ')}</span>{/if}</div>{/if}</div>
        {/each}
      </div>
    </details>
  {/if}
</details>
