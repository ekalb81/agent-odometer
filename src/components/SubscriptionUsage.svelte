<script lang="ts">
  // Provider-reported subscription quota (Codex rate-limit windows) plus
  // trailing token consumption, shown at the top of the analytics panel.
  // Fetches independently of the rest of SessionsView's range machinery —
  // deliberately NOT re-fetched on every store flush, only on a slow
  // interval, to avoid the refetch storms this repo just eliminated
  // elsewhere (see rangeData.ts).
  import { getSubscriptionUsage, sessionsInRanges } from '../lib/ipc';
  import type { RangeTotals, RateLimitWindow, SubscriptionUsageEntry } from '../lib/types';
  import { remainingPercent, resetCountdown, windowLabel } from '../lib/subscriptionUsage';

  interface Props {
    /** Mirrors SessionsView's own `active` prop — only the currently
     *  selected harness tab polls; inactive tabs stay idle. */
    active?: boolean;
  }
  let { active = true }: Props = $props();

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

  function sumTokens(rangeMap: Record<string, RangeTotals>): number {
    let total = 0;
    for (const rt of Object.values(rangeMap)) total += rt.tokens.total_tokens;
    return total;
  }

  async function refresh(): Promise<void> {
    try {
      const [usage, ranges] = await Promise.all([
        getSubscriptionUsage(),
        sessionsInRanges(
          TRAILING_WINDOWS.map(({ ms }) => ({ from: new Date(Date.now() - ms).toISOString(), to: null })),
        ),
      ]);
      entries = usage;
      trailingTokens = ranges.map(sumTokens);
      error = null;
    } catch (reason) {
      error = String(reason);
    } finally {
      loaded = true;
    }
  }

  $effect(() => {
    if (!active) return;
    void refresh();
    const interval = setInterval(() => void refresh(), REFRESH_INTERVAL_MS);
    return () => clearInterval(interval);
  });

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
    if (harness === 'codex') return 'Codex';
    if (harness === 'claude_code') return 'Claude Code';
    return harness;
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

  const showClaudeCodeNote = $derived(
    entries.length > 0 && !entries.some((entry) => entry.harness === 'claude_code'),
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
      {#if showClaudeCodeNote}
        <p class="text-[11px] text-ink-faint">Claude Code does not report quota in transcripts</p>
      {/if}
    </div>
  {/if}
</div>
