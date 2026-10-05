<script lang="ts">
  import { onMount } from 'svelte';
  import { getWidgetSettings, getWidgetSnapshot, onWidgetSettingsUpdated, onWidgetDataChanged, onRatesUpdated } from '../lib/ipc';
  import { formatCredits } from '../lib/currency';
  import { themeStore } from '../lib/stores/theme.svelte';
  import type { WidgetSettings, WidgetSnapshot } from '../lib/types';
  void themeStore;
  let settings = $state<WidgetSettings | null>(null);
  let snapshot = $state<WidgetSnapshot | null>(null);
  let error = $state<string | null>(null);
  let pending = $state(false);
  let generation = 0; let alive = true; let busy = false;
  let scheduled: ReturnType<typeof setTimeout> | null = null;
  let interval: ReturnType<typeof setInterval> | null = null;
  let lastRead = 0;
  let settingsGeneration = 0;
  const labels = { codex: 'Codex', claude_code: 'Claude Code', gemini_cli: 'Gemini CLI' };
  function stopTimers() { if (scheduled) clearTimeout(scheduled); if (interval) clearInterval(interval); scheduled = null; interval = null; }
  function configure(next: WidgetSettings) {
    if (!alive) return;
    settingsGeneration++;
    generation++; settings = next; snapshot = null; error = null; pending = false; stopTimers();
    if (!next.preferences.visible) return;
    void refresh(); interval = setInterval(() => invalidate(), 60_000);
  }
  function invalidate(clearPrices = false) {
    if (!alive || !settings?.preferences.visible) return;
    generation++; pending = true;
    if (clearPrices && snapshot?.usage) snapshot = { ...snapshot, usage: { ...snapshot.usage, plan_amount: null, api_amount_usd: null } };
    if (!settings?.preferences.visible || scheduled) return;
    const wait = Math.max(0, Math.min(5_000, 5_000 - (Date.now() - lastRead)));
    scheduled = setTimeout(() => { scheduled = null; void refresh(); }, wait);
  }
  async function refresh() {
    if (!settings?.preferences.visible || !alive) return;
    if (busy) { invalidate(); return; }
    busy = true; lastRead = Date.now(); const request = generation;
    try {
      const next = await getWidgetSnapshot();
      if (alive && request === generation && next.settings.revision === settings.revision) {
        snapshot = next; error = null; pending = false;
      }
    } catch { if (alive && request === generation) { error = 'Local snapshot unavailable. Previous observations remain dated.'; pending = false; } }
    finally { busy = false; if (alive && request !== generation && settings?.preferences.visible && !scheduled) invalidate(); }
  }
  onMount(() => {
    const unsubscribers: (() => void)[] = [];
    const retain = (unsubscribe: () => void) => { if (alive) unsubscribers.push(unsubscribe); else unsubscribe(); };
    const initialSettingsGeneration = settingsGeneration;
    void Promise.all([
      onWidgetSettingsUpdated(configure).then(retain),
      onWidgetDataChanged(() => invalidate()).then(all => all.forEach(retain)),
      onRatesUpdated(() => invalidate(true)).then(retain),
    ]).then(() => alive ? getWidgetSettings() : null).then(value => { if (alive && value && initialSettingsGeneration === settingsGeneration) configure(value); })
      .catch(() => { if (alive && initialSettingsGeneration === settingsGeneration) error = 'Widget settings unavailable. Configure the widget in Odometer Settings.'; });
    return () => { alive = false; generation++; stopTimers(); unsubscribers.forEach(fn => fn()); };
  });
</script>

<main class="widget bg-panel text-ink" aria-label="Local quota and usage widget">
  <header><strong>Odometer</strong><span>Local widget</span></header>
  <p class="text-ink-muted">{settings ? labels[settings.preferences.provider] : 'Loading settings…'} · {settings?.preferences.kind === 'usage' ? 'Local session usage' : 'Quota observations'}</p>
  {#if settings && !settings.preferences.visible}<p>Widget disabled. Enable it in Odometer Settings.</p>{/if}
  {#if error}<p role="alert" class="text-amber-500">{error}</p>{/if}
  {#if pending}<p role="status" class="text-ink-muted">Local updates pending…</p>{/if}
  {#if !snapshot && settings?.preferences.visible && !error}<p role="status">Loading local snapshot…</p>{/if}
  {#if snapshot}
    {#if snapshot.quota}
      <p class="text-ink-faint">{snapshot.quota.provenance === 'live_provider' ? 'Saved consented provider observation' : 'Local transcript observation; account unattributed'}</p>
      {#if snapshot.quota.unavailable}<p>No usable quota observation: {snapshot.quota.unavailable.replaceAll('_', ' ')}.</p>{/if}
      {#each snapshot.quota.windows as window}
        <section class="bg-card border border-edge rounded-md">
          <h2>{window.kind.replaceAll('_', ' ')}</h2>
          {#if window.unavailable}<p>Unavailable · {window.unavailable.replaceAll('_', ' ')}</p>
          {:else if window.unlimited}<p class="value">Unlimited</p>
          {:else}<p class="value">{window.remaining === null ? 'Remaining unavailable' : `Recorded ${window.remaining.toLocaleString(undefined, { maximumFractionDigits: 1 })}${window.unit === 'percent' ? '%' : ' credits'} remaining`}</p><p>{window.used === null ? 'Used unavailable' : `${window.used.toLocaleString(undefined, { maximumFractionDigits: 1 })}${window.unit === 'percent' ? '%' : ' credits'} observed used`}</p>{/if}
          <p class:text-amber-500={window.stale}>{window.stale ? 'Stale observation' : 'Observed'} · {window.observed_at}</p>
          <p>Reset evidence: {window.resets_at ?? 'not recorded'}</p>
        </section>
      {/each}
      {#if snapshot.quota.windows_omitted}<p>{snapshot.quota.windows_omitted} further windows omitted; see the main dashboard.</p>{/if}
    {/if}
    {#if snapshot.usage}
      {@const usage = snapshot.usage}
      <section class="bg-card border border-edge rounded-md">
        <h2>Cumulative local session usage</h2>
        <p class="value">{usage.total_tokens.toLocaleString()} tokens</p>
        <p>{usage.session_count} available-source sessions · retained missing sources excluded</p>
        {#if !usage.scan_complete}<p class="text-amber-500">Partial snapshot: local scan is still preparing or incomplete.</p>{/if}
        <p>Plan estimate: {usage.plan_amount === null ? 'unavailable' : formatCredits(usage.plan_amount, usage.plan_currency)}</p>
        <p>API base reference: {usage.api_amount_usd === null ? 'unavailable' : formatCredits(usage.api_amount_usd, 'USD')}</p>
        {#if usage.estimate_partial}<p class="text-amber-500">Partial or fallback estimate; see model provenance in the main dashboard.</p>{/if}
        <p>Latest source activity: {usage.latest_activity_at ?? 'no recorded activity'}</p>
        <p>Reset evidence: not applicable to cumulative usage.</p>
      </section>
    {/if}
    <p class="text-ink-faint">Snapshot computed: {snapshot.computed_at}</p>
  {/if}
  <footer class="text-ink-faint">Automatic refresh reads local evidence only, at most every 5 seconds. No network source is enabled here. Configure or hide this window in Odometer Settings.</footer>
</main>

<style>
  .widget { min-height: 100vh; padding: 16px; font-size: 12px; overflow-wrap: anywhere; }
  header { display: flex; justify-content: space-between; gap: 8px; } header strong { font-size: 18px; } header span { color: var(--text-muted); }
  p { margin: 10px 0; } section { margin: 12px 0; padding: 12px; } h2 { text-transform: capitalize; font-weight: 600; } .value { font-size: 24px; font-weight: 600; }
  footer { margin-top: 14px; font-size: 11px; }
</style>
