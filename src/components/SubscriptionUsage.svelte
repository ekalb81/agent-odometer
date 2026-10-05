<script lang="ts">
  import { historyStore } from '../lib/stores/history.svelte';
  import { sessionsStore } from '../lib/stores/sessions.svelte';
  import { accountingUnavailable } from '../lib/accountingAvailability';
  // Provider-reported subscription quota (Codex rate-limit windows) plus
  // trailing token consumption, shown at the top of the analytics panel.
  // Fetches independently of the rest of SessionsView's range machinery —
  // Observation refreshes use a slow interval. Accounting mutations invalidate
  // trailing numbers immediately, then coalesce a bounded proof refresh.
  import {
    getQuotaSnapshots,
    getSubscriptionUsage,
    sessionsInRanges,
  } from '../lib/ipc';
  import type {
    QuotaAlert,
    QuotaSnapshot,
    RangeTotals,
    RateLimitWindow,
    SubscriptionUsageEntry,
  } from '../lib/types';
  import type { ViewScope } from '../lib/sessionProjection';
  import {
    forecastSummary,
    quotaUnavailableLabel,
    quotaWindowLabel,
    quotaResetEvidence,
    remainingPercent,
    reserveDeficitLabel,
    resetCountdown,
    windowLabel,
  } from '../lib/subscriptionUsage';
  import { providersStore } from '../lib/stores/providers.svelte';
  import { formatCompactTokens } from '../lib/format';
  import QuotaBudgets from './QuotaBudgets.svelte';
  import LiveQuotaAccounts from './LiveQuotaAccounts.svelte';
  import LiveAccountAlerts from './LiveAccountAlerts.svelte';

  interface Props {
    /** Gate on `active && analyticsOpen`: `<details>` keeps collapsed
     *  children mounted, so without the disclosure state this would poll
     *  for the lifetime of the active tab even if Analytics is never
     *  opened. */
    active?: boolean;
    /** The enclosing tab's scope; quota rows and trailing consumption are
     *  filtered to it so per-harness tabs stay internally consistent. */
    harness?: ViewScope;
    /** Session ids in the tab's (non-date-filtered) scope, for the trailing
     *  consumption query. */
    sessionIds?: string[];
  }
  let { active = true, harness = 'all', sessionIds = [] }: Props = $props();

  const REFRESH_INTERVAL_MS = 60_000;
  const TICK_INTERVAL_MS = 30_000;
  const TRAILING_WINDOWS: { label: string; ms: number }[] = [
    { label: '15m', ms: 15 * 60_000 },
    { label: '1h', ms: 60 * 60_000 },
    { label: '24h', ms: 24 * 3_600_000 },
  ];

  let entries = $state<SubscriptionUsageEntry[]>([]);
  let trailingTokens = $state<number[] | null>(null);
  let loaded = $state(false);
  let error = $state<string | null>(null);
  let nowTick = $state(Date.now());

  // Quota windows/budgets/alerts (issue #43). One backend service
  // (src-tauri/src/quota.rs) computes every number here; this component
  // only fetches and formats. Polled on the same 60s cadence and the same
  // active-tab gate as the existing usage refresh above, to avoid adding a
  // second refresh storm.
  let quotaSnapshots = $state<QuotaSnapshot[]>([]);
  let quotaError = $state<string | null>(null);

  function sumTokens(rangeMap: Record<string, RangeTotals>): number {
    let total = 0;
    for (const rt of Object.values(rangeMap)) total += rt.tokens.total_tokens;
    return total;
  }

  let refreshGeneration = 0;
  let trailingError = $state<string | null>(null);
  async function refresh(observations = true): Promise<void> {
    const generation = ++refreshGeneration;
    const scope = harness;
    const ids = [...sessionIds];
    const mutation = sessionsStore.mutationLog.generation;
    const history = historyStore.status;
    trailingTokens = null; trailingError = null;
    const [usage, ranges, snapshots] = await Promise.allSettled([
      observations ? getSubscriptionUsage() : Promise.resolve(entries),
      sessionsInRanges(TRAILING_WINDOWS.map(({ ms }) => ({ from: new Date(Date.now() - ms).toISOString(), to: null })), ids, ids),
      observations ? getQuotaSnapshots() : Promise.resolve(quotaSnapshots),
    ]);
    if (generation !== refreshGeneration || !active || scope !== harness || ids.join('|') !== sessionIds.join('|') || mutation !== sessionsStore.mutationLog.generation || history !== historyStore.status) return;
    if (usage.status === 'fulfilled') {
      entries = scope === 'all' ? usage.value : usage.value.filter(entry => entry.harness === scope); error = null;
    } else { entries = []; error = 'Provider quota observations unavailable.'; }
    if (ranges.status === 'fulfilled') trailingTokens = ranges.value.map(sumTokens);
    else { trailingTokens = null; trailingError = accountingUnavailable(ranges.reason); }
    if (snapshots.status === 'fulfilled') {
      quotaSnapshots = scope === 'all' ? snapshots.value : snapshots.value.filter(s => s.provider === scope); quotaError = null;
    } else { quotaSnapshots = []; quotaError = 'Quota snapshots unavailable.'; }
    loaded = true;
  }

  function handleBudgetAlerts(fired: QuotaAlert[]): void {
    void fired; // Desktop delivery is owned by the shared main-window monitor.
  }

  let lastRefreshScope = '';
  $effect(() => {
    const scope = `${active}|${harness}|${sessionIds.join('|')}`;
    const scopeChanged = scope !== lastRefreshScope; lastRefreshScope = scope;
    void historyStore.status; void harness; void sessionIds; void sessionsStore.mutationLog.generation;
    refreshGeneration++; trailingTokens = null; trailingError = null;
    if (!active) return;
    if (scopeChanged) { entries = []; quotaSnapshots = []; loaded = false; }
    const timer = setTimeout(() => { void refresh(scopeChanged); }, scopeChanged ? 0 : 250);
    const interval = setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => { clearInterval(interval); clearTimeout(timer); refreshGeneration++; };
  });

  // Keeps "as of Xm ago" and reset countdowns fresh without re-fetching.
  $effect(() => {
    if (!active) return;
    const interval = setInterval(() => { nowTick = Date.now(); }, TICK_INTERVAL_MS);
    return () => clearInterval(interval);
  });

  function harnessLabel(harness: string): string {
    return providersStore.displayName(harness);
  }

  function planBadge(entry: SubscriptionUsageEntry): string {
    if (entry.credits_unlimited) return 'unlimited';
    return entry.plan_type ?? 'plan unknown';
  }

  function windowTitle(window: RateLimitWindow): string {
    return window.window_minutes != null ? windowLabel(window.window_minutes) : 'Window';
  }

  function barWidth(usedPercent: number): number {
    return Math.min(100, Math.max(0, usedPercent));
  }

  function windowsFor(entry: SubscriptionUsageEntry): { key: string; window: RateLimitWindow }[] {
    const out: { key: string; window: RateLimitWindow }[] = [];
    if (entry.primary) out.push({ key: 'primary', window: entry.primary });
    if (entry.secondary) out.push({ key: 'secondary', window: entry.secondary });
    return out;
  }

  function freshnessLabel(capturedAt: string): string {
    const capturedMs = Date.parse(capturedAt);
    if (Number.isNaN(capturedMs)) return '';
    const diffMin = Math.max(0, Math.round((nowTick - capturedMs) / 60_000));
    if (diffMin < 1) return 'as of just now';
    if (diffMin < 60) return `as of ${diffMin}m ago`;
    return `as of ${Math.floor(diffMin / 60)}h ago`;
  }

  // Only meaningful on the combined tab: per-harness tabs filter entries to
  // their own provider, so a missing row there is deliberate. Capability-
  // driven (`quota_source`) rather than a hardcoded Claude Code check, so a
  // future provider without local quota telemetry is covered automatically.
  const providersWithoutQuota = $derived(
    harness === 'all' && entries.length > 0
      ? providersStore.descriptors.filter(
          (descriptor) => !descriptor.quota_source && !entries.some((entry) => entry.harness === descriptor.id),
        )
      : [],
  );
</script>

<div class="bg-card border border-edge rounded-lg px-3 py-2" data-testid="subscription-usage-panel">
  <div class="flex items-center justify-between gap-2 flex-wrap">
    <span class="text-xs font-semibold text-ink">Subscription usage</span>
    {#if trailingTokens}
      <span class="text-[11px] text-ink-muted font-mono">
        {#each TRAILING_WINDOWS as w, i (w.label)}{i > 0 ? ' · ' : ''}{w.label} {formatCompactTokens(trailingTokens[i])}{/each}
        &nbsp;tokens
      </span>
    {/if}
  </div>

  {#if trailingError}<p data-testid="accounting-trailing-status" role="status" class="text-[11px] text-neg mt-1.5">{trailingError}</p>{:else if !trailingTokens}<p data-testid="accounting-trailing-status" role="status" class="text-[11px] text-ink-faint mt-1.5">Verifying trailing usage…</p>{/if}
  {#if error}
    <p class="text-[11px] text-neg mt-1.5">{error}</p>
  {:else if loaded && entries.length === 0}
    <p class="text-[11px] text-ink-faint mt-1.5">No provider quota data captured yet</p>
  {:else if entries.length > 0}
    <div class="flex flex-col gap-2 mt-1.5">
      {#each entries as entry (entry.harness)}
        <div class="border-t border-edgerow pt-1.5 first:border-t-0 first:pt-0">
          <div class="flex items-center justify-between gap-2 text-[11px]">
            <span class="font-semibold text-ink">{harnessLabel(entry.harness)}</span>
            <span class="px-1.5 py-0.5 rounded-sm bg-panel border border-edge text-ink-muted">{planBadge(entry)}</span>
            <span class="text-ink-faint ml-auto">{freshnessLabel(entry.captured_at)}</span>
          </div>
          {#each windowsFor(entry) as { key, window } (key)}
            <div class="mt-1.5">
              <div class="flex justify-between text-[11px] text-ink-muted mb-1">
                <span>{windowTitle(window)}</span>
                <span class="font-mono text-ink-2">
                  {remainingPercent(window.used_percent).toFixed(0)}% left
                  {#if resetCountdown(window.resets_at, nowTick)} · resets {resetCountdown(window.resets_at, nowTick)}{/if}
                </span>
              </div>
              <div class="h-[6px] bg-track rounded-[3px] overflow-hidden">
                <div
                  class="h-[6px] rounded-[3px] {window.used_percent > 90 ? 'bg-amber-500' : 'bg-accent'}"
                  style="width: {barWidth(window.used_percent)}%"
                ></div>
              </div>
            </div>
          {/each}
        </div>
      {/each}
      {#each providersWithoutQuota as descriptor (descriptor.id)}
        <p class="text-[11px] text-ink-faint">{descriptor.display_name} does not report quota in transcripts</p>
      {/each}
    </div>
  {/if}

  {#if quotaError}
    <p class="text-[11px] text-neg mt-1.5">{quotaError}</p>
  {:else if quotaSnapshots.length > 0}
    <div class="mt-2 flex flex-col gap-2" data-testid="quota-windows">
      <span class="text-[11px] font-semibold text-ink-muted">Quota observations</span>
      {#each quotaSnapshots as snapshot (snapshot.provider)}
        <div class="border-t border-edgerow pt-1.5 first:border-t-0 first:pt-0">
          <div class="flex items-center justify-between gap-2 text-[11px]">
            <span class="font-semibold text-ink">{harnessLabel(snapshot.provider)}</span>
            <span class="text-ink-faint">{snapshot.provenance === 'transcript_derived' ? 'transcript-derived' : 'live'}</span>
          </div>
          {#if snapshot.unavailable}
            <p class="text-[11px] text-ink-faint mt-1">{quotaUnavailableLabel(snapshot.unavailable)}</p>
          {:else}
            {#each snapshot.windows as window (window.kind + window.unit)}
              <div class="mt-1.5">
                <div class="flex justify-between text-[11px] text-ink-muted mb-1">
                  <span>{quotaWindowLabel(window)}</span>
                  {#if window.unavailable}
                    <span class="text-ink-faint">{quotaUnavailableLabel(window.unavailable)}</span>
                  {:else if window.unlimited}
                    <span class="font-mono text-ink-2">unlimited</span>
                  {:else if window.unit === 'percent' && window.used != null}
                    <span class="font-mono text-ink-2">
                      {remainingPercent(window.used).toFixed(0)}% left{window.stale ? ' · stale' : ''}
                      {#if resetCountdown(window.resets_at, nowTick)} · resets {resetCountdown(window.resets_at, nowTick)}{/if}
                    </span>
                  {:else if window.unit === 'credits' && window.remaining != null}
                    <span class="font-mono text-ink-2">{window.remaining.toFixed(2)} remaining{window.stale ? ' · stale' : ''}</span>
                  {/if}
                </div>
                {#if window.unit === 'percent' && !window.unavailable && window.used != null}
                  <div class="h-[6px] bg-track rounded-[3px] overflow-hidden">
                    <div
                      class="h-[6px] rounded-[3px] {window.used > 90 ? 'bg-amber-500' : 'bg-accent'}"
                      style="width: {barWidth(window.used)}%"
                    ></div>
                  </div>
                {/if}
                {#if window.forecast}
                  <p class="text-[10px] text-ink-faint mt-0.5">
                    Estimated {forecastSummary(window.forecast, nowTick)} · {reserveDeficitLabel(window.forecast.reserve_deficit_percent)}
                    ({window.forecast.evidence_points} samples)
                  </p>
                {/if}
                {#if quotaResetEvidence(window, nowTick)}
                  <p class="text-[10px] text-ink-faint mt-0.5">{quotaResetEvidence(window, nowTick)}</p>
                {/if}
              </div>
            {/each}
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  <LiveQuotaAccounts {active} {harness} />
  <LiveAccountAlerts {active} {harness} />
  <QuotaBudgets {active} {harness} onAlerts={handleBudgetAlerts} />
</div>
