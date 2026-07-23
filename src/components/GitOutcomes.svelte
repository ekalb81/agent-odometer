<script lang="ts">
  import { correlateEvents, listExternalEvents, scanGitOutcomes } from '../lib/ipc';
  import type { EventCorrelation, GitOutcome, GitOutcomeKind } from '../lib/types';

  let outcomes = $state<GitOutcome[]>([]);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let correlations = $state<Record<string, EventCorrelation>>({});
  let postWindowHours = $state(24);
  const kinds: GitOutcomeKind[] = ['kept', 'reverted', 'abandoned', 'ambiguous', 'not_evaluated'];
  const correlationBatchSize = 2_000;

  async function scan() {
    busy = true; error = null;
    try {
      outcomes = await scanGitOutcomes(postWindowHours);
      const sessionIds = new Set(outcomes.map((outcome) => outcome.session_id));
      const events = (await listExternalEvents()).filter((event) => event.source === 'git' && sessionIds.has(event.metadata.session_id));
      const results: EventCorrelation[] = [];
      for (let offset = 0; offset < events.length; offset += correlationBatchSize) {
        const batch = await correlateEvents({
          events: events.slice(offset, offset + correlationBatchSize),
          before_days: 7,
          after_days: 7,
          exclude_confounded: false,
          include_subagents: true,
        });
        results.push(...batch.results);
      }
      correlations = Object.fromEntries(results.map((item) => [item.event.metadata.session_id, item]));
    }
    catch (reason) { error = String(reason); }
    finally { busy = false; }
  }
</script>

<details class="bg-card border border-edge rounded-lg px-3 py-2">
  <summary class="cursor-pointer text-xs font-semibold text-ink">Local git outcomes</summary>
  <div class="mt-2 flex items-center gap-2">
    <button class="px-3 py-1.5 rounded-md border border-edge bg-panel hover:bg-app text-xs disabled:opacity-50" disabled={busy} onclick={scan}>{busy ? 'Scanning…' : 'Evaluate local repositories'}</button>
    <label class="text-[11px] text-ink-muted">Post-session window
      <input class="ml-1 w-16 rounded border border-edge bg-app px-1.5 py-1 font-mono" type="number" min="0" max="8760" step="1" bind:value={postWindowHours} disabled={busy} /> h
    </label>
    <span class="text-[11px] text-ink-faint">HEAD-reachable commits · no remotes or worktree changes</span>
  </div>
  {#if error}<p class="text-xs text-neg mt-2">{error}</p>{/if}
  {#if outcomes.length > 0}
    <div class="grid grid-cols-5 gap-2 mt-2">
      {#each kinds as kind}
        <div class="border border-edgerow rounded px-2 py-1 text-[11px]"><span class="text-ink-muted">{kind.replace('_', ' ')}</span><div class="font-mono font-semibold">{outcomes.filter((outcome) => outcome.kind === kind).length}</div></div>
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
