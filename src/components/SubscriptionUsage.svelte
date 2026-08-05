<script lang="ts">
  // Provider-reported subscription quota (Codex rate-limit windows) plus
  // trailing token consumption, shown at the top of the analytics panel.
  // Fetches independently of the rest of SessionsView's range machinery —
  // deliberately NOT re-fetched on every store flush, only on a slow
  // interval, to avoid the refetch storms this repo just eliminated
  // elsewhere (see rangeData.ts).
  import {
    checkQuotaAlerts,
    getQuotaConfig,
    getQuotaSnapshots,
    getSubscriptionUsage,
    sessionsInRanges,
    setQuotaConfig,
  } from '../lib/ipc';
  import type {
    QuotaAlert,
    QuotaBudget,
    QuotaConfigWire,
    QuotaSnapshot,
    QuotaWindow,
    RangeTotals,
    RateLimitWindow,
    SubscriptionUsageEntry,
  } from '../lib/types';
  import type { ViewScope } from '../lib/sessionProjection';
  import {
    budgetStatus,
    forecastSummary,
    quotaUnavailableLabel,
    quotaWindowLabel,
    remainingPercent,
    reserveDeficitLabel,
    resetCountdown,
    windowLabel,
  } from '../lib/subscriptionUsage';
  import { providersStore } from '../lib/stores/providers.svelte';

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
  let quotaConfig = $state<QuotaConfigWire | null>(null);
  let quotaError = $state<string | null>(null);
  let alerts = $state<QuotaAlert[]>([]);
  let budgetsOpen = $state(false);
  let newBudgetProvider = $state('');
  let newBudgetWindowKind = $state('burst');
  let newBudgetThreshold = $state(80);
  let budgetSaveError = $state<string | null>(null);

  function sumTokens(rangeMap: Record<string, RangeTotals>): number {
    let total = 0;
    for (const rt of Object.values(rangeMap)) total += rt.tokens.total_tokens;
    return total;
  }

  function notifyIfPermitted(alert: QuotaAlert): void {
    if (typeof Notification === 'undefined') return;
    if (Notification.permission !== 'granted') return;
    try {
      new Notification('Odometer quota alert', { body: alert.message });
    } catch {
      // Notification construction can throw in restricted webview contexts;
      // the in-panel alert list below still shows it either way.
    }
  }

  async function refresh(): Promise<void> {
    try {
      const [usage, ranges, snapshots, config] = await Promise.all([
        getSubscriptionUsage(),
        // Scoped to the tab's sessions so a per-harness tab's trailing
        // figures match the analytics beside them ('all' passes every id).
        sessionsInRanges(
          TRAILING_WINDOWS.map(({ ms }) => ({ from: new Date(Date.now() - ms).toISOString(), to: null })),
          sessionIds,
        ),
        getQuotaSnapshots(),
        getQuotaConfig(),
      ]);
      entries = harness === 'all' ? usage : usage.filter((entry) => entry.harness === harness);
      trailingTokens = ranges.map(sumTokens);
      quotaSnapshots = harness === 'all' ? snapshots : snapshots.filter((s) => s.provider === harness);
      quotaConfig = config;
      error = null;
      quotaError = null;
    } catch (reason) {
      error = String(reason);
      quotaError = String(reason);
    } finally {
      loaded = true;
    }

    // Alert check is independent of the above Promise.all so a failure
    // fetching it never blocks the usage panel from rendering.
    try {
      const fired = await checkQuotaAlerts();
      if (fired.length > 0) {
        alerts = [...fired, ...alerts].slice(0, 20);
        for (const alert of fired) notifyIfPermitted(alert);
      }
    } catch {
      // Alerts are best-effort; the panel still shows quota/budget state.
    }
  }

  $effect(() => {
    if (!active) return;
    void refresh();
    const interval = setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => clearInterval(interval);
  });

  async function toggleNotifications(): Promise<void> {
    if (!quotaConfig) return;
    const enabling = !quotaConfig.notifications.enabled;
    if (enabling && typeof Notification !== 'undefined' && Notification.permission === 'default') {
      try {
        await Notification.requestPermission();
      } catch {
        // Permission prompt can fail/no-op in restricted contexts; the
        // opt-in setting still saves below, alerts just stay panel-only.
      }
    }
    const updated: QuotaConfigWire = {
      ...quotaConfig,
      notifications: { ...quotaConfig.notifications, enabled: enabling },
    };
    try {
      quotaConfig = await setQuotaConfig(updated);
      budgetSaveError = null;
    } catch (reason) {
      budgetSaveError = String(reason);
    }
  }

  async function addBudget(): Promise<void> {
    if (!quotaConfig || !newBudgetProvider) return;
    const budget: QuotaBudget = {
      id: `${newBudgetProvider}-${newBudgetWindowKind}-${Date.now()}`,
      provider: newBudgetProvider,
      project_key: null,
      unit: 'percent_of_window',
      window_kind: newBudgetWindowKind,
      period_hours: null,
      threshold: newBudgetThreshold,
      enabled: true,
    };
    const updated: QuotaConfigWire = { ...quotaConfig, budgets: [...quotaConfig.budgets, budget] };
    try {
      quotaConfig = await setQuotaConfig(updated);
      budgetSaveError = null;
    } catch (reason) {
      budgetSaveError = String(reason);
    }
  }

  async function removeBudget(id: string): Promise<void> {
    if (!quotaConfig) return;
    const updated: QuotaConfigWire = {
      ...quotaConfig,
      budgets: quotaConfig.budgets.filter((b) => b.id !== id),
    };
    try {
      quotaConfig = await setQuotaConfig(updated);
      budgetSaveError = null;
    } catch (reason) {
      budgetSaveError = String(reason);
    }
  }

  /** Live value for a percent_of_window budget, derived from the already-
   *  fetched quota snapshots (no extra IPC round-trip). null when the
   *  matching window isn't currently available. */
  function currentBudgetValue(budget: QuotaBudget): number | null {
    if (budget.unit !== 'percent_of_window') return null;
    const snapshot = quotaSnapshots.find((s) => s.provider === budget.provider);
    const window = snapshot?.windows.find(
      (w: QuotaWindow) => w.kind === budget.window_kind && w.unavailable === null && w.used != null,
    );
    return window?.used ?? null;
  }

  // Keeps "as of Xm ago" and reset countdowns fresh without re-fetching.
  $effect(() => {
    if (!active) return;
    const interval = setInterval(() => { nowTick = Date.now(); }, TICK_INTERVAL_MS);
    return () => clearInterval(interval);
  });

  function fmtCompact(n: number): string {
    if (n >= 1e9) return `${(n / 1e9).toFixed(2)}B`;
    if (n >= 1e6) return `${(n / 1e6).toFixed(2)}M`;
    if (n >= 1e3) return `${(n / 1e3).toFixed(1)}K`;
    return String(n);
  }

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
        {#each TRAILING_WINDOWS as w, i (w.label)}{i > 0 ? ' · ' : ''}{w.label} {fmtCompact(trailingTokens[i])}{/each}
        &nbsp;tokens
      </span>
    {/if}
  </div>

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

  {#if alerts.length > 0}
    <div class="mt-2 flex flex-col gap-1" data-testid="quota-alerts">
      {#each alerts as alert (alert.budget_id + alert.fired_at)}
        <p class="text-[11px] text-amber-600 bg-amber-500/10 border border-amber-500/30 rounded-sm px-1.5 py-1">
          {alert.message}
        </p>
      {/each}
    </div>
  {/if}

  {#if quotaError}
    <p class="text-[11px] text-neg mt-1.5">{quotaError}</p>
  {:else if quotaSnapshots.length > 0}
    <div class="mt-2 flex flex-col gap-2" data-testid="quota-windows">
      <span class="text-[11px] font-semibold text-ink-muted">Live quota windows</span>
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
                    {forecastSummary(window.forecast, nowTick)} · {reserveDeficitLabel(window.forecast.reserve_deficit_percent)}
                    ({window.forecast.evidence_points} samples)
                  </p>
                {/if}
              </div>
            {/each}
          {/if}
        </div>
      {/each}
    </div>
  {/if}

  <details class="mt-2" bind:open={budgetsOpen} data-testid="quota-budgets-panel">
    <summary class="text-[11px] font-semibold text-ink-muted cursor-pointer select-none">Soft budgets &amp; alerts</summary>
    <div class="mt-1.5 flex flex-col gap-1.5">
      {#if quotaConfig}
        <label class="flex items-center gap-1.5 text-[11px] text-ink-muted">
          <input type="checkbox" checked={quotaConfig.notifications.enabled} onchange={toggleNotifications} />
          Notify me when a budget is crossed
        </label>

        {#if quotaConfig.budgets.length > 0}
          <div class="flex flex-col gap-1">
            {#each quotaConfig.budgets as budget (budget.id)}
              {@const current = currentBudgetValue(budget)}
              {@const status = current != null ? budgetStatus(current, budget) : null}
              <div class="flex items-center gap-1.5 text-[11px]">
                <span
                  class="px-1 py-0.5 rounded-sm border {status === 'exceeded'
                    ? 'bg-neg/10 border-neg/30 text-neg'
                    : status === 'warning'
                      ? 'bg-amber-500/10 border-amber-500/30 text-amber-600'
                      : 'bg-panel border-edge text-ink-muted'}"
                >
                  {status ?? 'no data'}
                </span>
                <span class="text-ink-muted">
                  {harnessLabel(budget.provider)} · {budget.unit === 'percent_of_window' ? (budget.window_kind ?? 'window') : 'tokens'} ≥ {budget.threshold}
                  {budget.unit === 'percent_of_window' ? '%' : ''}
                  {#if current != null}<span class="font-mono">({current.toFixed(0)}{budget.unit === 'percent_of_window' ? '%' : ''} now)</span>{/if}
                </span>
                <button
                  type="button"
                  class="ml-auto text-ink-faint hover:text-neg"
                  onclick={() => removeBudget(budget.id)}
                  aria-label="Remove budget"
                >
                  ×
                </button>
              </div>
            {/each}
          </div>
        {/if}

        <div class="flex items-center gap-1.5 text-[11px] mt-1">
          <select bind:value={newBudgetProvider} class="bg-panel border border-edge rounded-sm px-1 py-0.5 text-ink-muted">
            <option value="">Provider…</option>
            {#each providersStore.descriptors.filter((d) => d.quota_source) as descriptor (descriptor.id)}
              <option value={descriptor.id}>{descriptor.display_name}</option>
            {/each}
          </select>
          <select bind:value={newBudgetWindowKind} class="bg-panel border border-edge rounded-sm px-1 py-0.5 text-ink-muted">
            <option value="burst">Burst</option>
            <option value="daily">Daily</option>
            <option value="weekly">Weekly</option>
            <option value="monthly">Monthly</option>
          </select>
          <input
            type="number"
            min="1"
            max="100"
            bind:value={newBudgetThreshold}
            class="w-14 bg-panel border border-edge rounded-sm px-1 py-0.5 text-ink-muted"
          />
          <span class="text-ink-faint">%</span>
          <button
            type="button"
            class="ml-auto px-1.5 py-0.5 rounded-sm border border-edge text-ink-muted hover:text-ink disabled:opacity-50"
            disabled={!newBudgetProvider}
            onclick={addBudget}
          >
            Add
          </button>
        </div>
        <p class="text-[10px] text-ink-faint">
          Per-project token budgets are supported by the backend (see Settings) but not yet editable from this panel.
        </p>
        {#if budgetSaveError}
          <p class="text-[11px] text-neg">{budgetSaveError}</p>
        {/if}
      {/if}
    </div>
  </details>
</div>
