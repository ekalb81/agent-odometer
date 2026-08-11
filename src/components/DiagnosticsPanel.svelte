<script lang="ts">
  import { getConfig, getPerformanceLiveStatus, getProviderDiagnostics, setConfig, writeExport } from '../lib/ipc';
  import { diagnosticsExportJson } from '../lib/diagnosticsExport';
  import Sparkline from './Sparkline.svelte';
  import type {
    Config,
    DiagnosticsReport,
    PerformanceLiveStatus,
    PhaseSampleEvent,
    ProviderDiagnostic,
    ProviderHealthState,
  } from '../lib/types';

  interface Props {
    /** Gate on `active && telemetryOpen`, mirroring `SubscriptionUsage`'s
     *  convention: this panel is mounted for the whole time Settings is open
     *  (see `App.svelte`), so without the disclosure state below the live
     *  poll would run for the lifetime of that tab even if telemetry is
     *  never opened. */
    active?: boolean;
  }
  let { active = true }: Props = $props();

  let report = $state<DiagnosticsReport | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let includePaths = $state(false);
  let copyStatus = $state<string | null>(null);
  let exporting = $state(false);
  let exportError = $state<string | null>(null);

  // Live performance telemetry (issue #163). A one-shot config read (not a
  // poll) tells this section whether tracking is on; the live values
  // themselves only poll while the disclosure below is actually open.
  const LIVE_POLL_MS = 2_000;
  let config = $state<Config | null>(null);
  let configError = $state<string | null>(null);
  let enablingTracking = $state(false);
  let telemetryOpen = $state(false);
  let liveStatus = $state<PerformanceLiveStatus | null>(null);
  let liveError = $state<string | null>(null);

  async function loadConfig(): Promise<void> {
    try {
      config = await getConfig();
      configError = null;
    } catch (e) {
      configError = String(e);
    }
  }

  $effect(() => {
    if (!active) return;
    void loadConfig();
  });

  async function enableTracking(): Promise<void> {
    if (!config) return;
    enablingTracking = true;
    try {
      const updated: Config = { ...config, performance_tracking_enabled: true };
      await setConfig(updated);
      config = updated;
      configError = null;
    } catch (e) {
      configError = String(e);
    } finally {
      enablingTracking = false;
    }
  }

  async function refreshLiveStatus(): Promise<void> {
    try {
      liveStatus = await getPerformanceLiveStatus();
      liveError = null;
    } catch (e) {
      liveError = String(e);
    }
  }

  $effect(() => {
    if (!active || !telemetryOpen || !config?.performance_tracking_enabled) return;
    void refreshLiveStatus();
    const interval = setInterval(() => void refreshLiveStatus(), LIVE_POLL_MS);
    return () => clearInterval(interval);
  });

  /** Recent RSS samples reshaped for `Sparkline`, which plots a generic
   *  `{timestamp, total_tokens}` series — reused as-is rather than adding a
   *  second charting component. `elapsed_ms` (time since the phase started,
   *  not a wall-clock instant) is converted to a real timestamp anchored on
   *  "now" so the sparkline's own tooltip still reads sensibly. */
  function memoryPoints(samples: PhaseSampleEvent[]): { timestamp: string; total_tokens: number }[] {
    const withRss = samples.filter((s) => s.rss_bytes != null);
    if (withRss.length === 0) return [];
    const latestElapsed = withRss[withRss.length - 1].elapsed_ms;
    const now = Date.now();
    return withRss.map((s) => ({
      timestamp: new Date(now - (latestElapsed - s.elapsed_ms)).toISOString(),
      total_tokens: s.rss_bytes as number,
    }));
  }

  function fmtBytes(value: number | null | undefined): string {
    if (value == null) return 'unavailable';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let scaled = value;
    let unit = 0;
    while (scaled >= 1024 && unit < units.length - 1) {
      scaled /= 1024;
      unit++;
    }
    return `${scaled.toFixed(unit === 0 ? 0 : 1)} ${units[unit]}`;
  }

  async function refresh() {
    loading = true;
    error = null;
    try {
      report = await getProviderDiagnostics();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  }

  const stateLabel: Record<ProviderHealthState, string> = {
    ready: 'Ready',
    degraded: 'Degraded',
    unsupported: 'Unsupported',
    not_detected: 'Not detected',
  };

  const stateClass: Record<ProviderHealthState, string> = {
    ready: 'text-pos bg-(--positive-chip-bg) border-(--positive-chip-border)',
    degraded: 'text-amber-500 bg-amber-500/10 border-amber-500/30',
    unsupported: 'text-ink-faint bg-app border-edge',
    not_detected: 'text-ink-faint bg-app border-edge',
  };

  const retentionLabel = { none: 'None', moderate: 'Moderate', high: 'High' };

  async function copyJson() {
    if (!report) return;
    copyStatus = null;
    try {
      await navigator.clipboard.writeText(diagnosticsExportJson(report, { includePaths }));
      copyStatus = 'Copied.';
    } catch (e) {
      copyStatus = `Could not copy: ${e}`;
    }
  }

  async function exportJson() {
    if (!report) return;
    exporting = true;
    exportError = null;
    try {
      const json = diagnosticsExportJson(report, { includePaths });
      await writeExport('odometer-diagnostics', 'json', json);
    } catch (e) {
      exportError = String(e);
    } finally {
      exporting = false;
    }
  }

  function providerRootsMissing(provider: ProviderDiagnostic): number {
    return provider.roots.filter((root) => !root.exists).length;
  }
</script>

<section>
  <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Diagnostics</h2>
  <p class="text-xs text-ink-faint mb-3 max-w-3xl">
    Per-provider health for missing sessions, partial provider support, parse failures, stale
    pricing, and retention risk — without reading logs. Not fetched automatically; refresh below.
  </p>

  <div class="bg-card border border-edge rounded-lg px-4 py-3 max-w-3xl mb-3" data-testid="performance-telemetry-panel">
    <h3 class="text-xs font-semibold text-ink mb-1">Live performance telemetry</h3>
    {#if configError}
      <p class="text-xs text-red-500">{configError}</p>
    {:else if config && !config.performance_tracking_enabled}
      <p class="text-[11px] text-ink-faint">
        Performance tracking is off — no sampler runs and nothing is recorded, so there is
        nothing live to show.
      </p>
      <button
        onclick={enableTracking}
        disabled={enablingTracking}
        class="mt-2 px-3 py-1.5 text-xs rounded-sm bg-app border border-edge hover:bg-(--row-hover) disabled:opacity-40"
      >{enablingTracking ? 'Enabling…' : 'Enable performance tracking'}</button>
    {:else if config}
      <details bind:open={telemetryOpen}>
        <summary class="text-[11px] text-accent-chipfg cursor-pointer select-none">
          {telemetryOpen ? 'Hide live view' : 'Show live view'}
        </summary>
        <div class="mt-2">
          {#if liveError}
            <p class="text-xs text-red-500">{liveError}</p>
          {:else if liveStatus}
            <div class="grid grid-cols-2 sm:grid-cols-4 gap-x-4 gap-y-1 text-[11px] text-ink-faint">
              <div>RSS: {fmtBytes(liveStatus.process?.rss_bytes)}</div>
              <div>Peak RSS: {fmtBytes(liveStatus.process?.peak_rss_bytes)}</div>
              <div>Private bytes: {fmtBytes(liveStatus.process?.private_bytes)}</div>
              <div>Heap: {liveStatus.heap?.current_bytes != null ? fmtBytes(liveStatus.heap.current_bytes) : 'not tracked'}</div>
            </div>

            {#if liveStatus.active_phase}
              <p class="mt-2 text-[11px] text-ink-2" data-testid="active-phase">
                Active phase: <span class="font-mono">{liveStatus.active_phase}</span>
                · {((liveStatus.active_phase_elapsed_ms ?? 0) / 1000).toFixed(1)}s elapsed
                {#if liveStatus.progress_done != null}
                  · {liveStatus.progress_done.toLocaleString()}{liveStatus.progress_total != null ? ` / ${liveStatus.progress_total.toLocaleString()}` : ''} processed
                {/if}
              </p>
            {:else}
              <p class="mt-2 text-[11px] text-ink-faint">No phase currently running.</p>
            {/if}

            {#if liveStatus.recent_samples.length >= 2}
              <div class="mt-2">
                <Sparkline points={memoryPoints(liveStatus.recent_samples)} width={320} height={48} />
              </div>
            {/if}

            {#if liveStatus.database_footprints.length > 0}
              <div class="mt-2 space-y-0.5">
                {#each liveStatus.database_footprints as fp (fp.connection)}
                  <p class="text-[11px] text-ink-faint">
                    {fp.connection}: {fmtBytes(fp.db_bytes)}{fp.wal_bytes != null ? ` (+${fmtBytes(fp.wal_bytes)} WAL)` : ''}
                    {#if fp.volume_free_bytes != null && fp.volume_total_bytes != null}
                      · volume {fmtBytes(fp.volume_free_bytes)} free / {fmtBytes(fp.volume_total_bytes)} total
                    {/if}
                  </p>
                {/each}
              </div>
            {/if}

            {#if liveStatus.recent_operations.length > 0}
              <details class="mt-2">
                <summary class="text-[11px] text-accent-chipfg cursor-pointer">Recent phase timings</summary>
                <ul class="mt-1 space-y-0.5">
                  {#each liveStatus.recent_operations.slice(-15).reverse() as op (op.timestamp + op.operation)}
                    <li class="text-[11px] font-mono text-ink-faint">{op.operation}: {op.duration_ms.toFixed(0)}ms{op.success ? '' : ' (failed)'}</li>
                  {/each}
                </ul>
              </details>
            {/if}
          {:else}
            <p class="text-[11px] text-ink-faint italic">Loading…</p>
          {/if}
        </div>
      </details>
    {/if}
  </div>

  <div class="bg-card border border-edge rounded-lg px-4 py-3 max-w-3xl">
    <div class="flex items-center gap-3 flex-wrap">
      <button
        onclick={refresh}
        disabled={loading}
        class="px-3 py-1.5 text-xs font-medium rounded-sm bg-accent-tab hover:opacity-90 disabled:opacity-40 text-white transition-colors"
      >{loading ? 'Checking…' : report ? 'Refresh' : 'Run diagnostics'}</button>
      {#if report}
        <span class="text-[11px] text-ink-faint">Generated {new Date(report.generated_at).toLocaleString()}</span>
        {#if !report.source_configuration_valid}
          <span class="text-[11px] text-red-500">Session-source configuration is invalid; scanning is disabled for every provider until Watched roots is corrected.</span>
        {/if}
      {/if}
    </div>
    {#if error}
      <p class="text-xs text-red-500 mt-2">{error}</p>
    {/if}
  </div>

  {#if report}
    <div class="mt-3 space-y-3 max-w-3xl">
      {#each report.providers as provider (provider.id)}
        <div class="bg-card border border-edge rounded-lg px-4 py-3">
          <div class="flex items-center gap-2 flex-wrap">
            <h3 class="text-sm font-semibold text-ink">{provider.display_name}</h3>
            <span class="px-2 py-0.5 rounded-full text-[10px] font-medium border {stateClass[provider.state]}">
              {stateLabel[provider.state]}
            </span>
            {#if provider.capabilities.archived_sources}
              <span class="px-1.5 py-0.5 rounded-sm text-[10px] bg-app border border-edge text-ink-faint">archive support</span>
            {/if}
            {#if provider.capabilities.session_index}
              <span class="px-1.5 py-0.5 rounded-sm text-[10px] bg-app border border-edge text-ink-faint">session index</span>
            {/if}
            <span class="px-1.5 py-0.5 rounded-sm text-[10px] bg-app border border-edge text-ink-faint">{provider.capabilities.currency}</span>
            {#if provider.capabilities.deep_link}
              <span class="px-1.5 py-0.5 rounded-sm text-[10px] bg-app border border-edge text-ink-faint">deep link</span>
            {/if}
            {#if provider.capabilities.quota_source}
              <span class="px-1.5 py-0.5 rounded-sm text-[10px] bg-app border border-edge text-ink-faint">quota source</span>
            {/if}
          </div>

          {#if provider.reasons.length > 0}
            <ul class="mt-2 space-y-0.5">
              {#each provider.reasons as r (r.code)}
                <li class="text-[11px] text-ink-2"><span class="text-ink-faint font-mono">[{r.code}]</span> {r.message}</li>
              {/each}
            </ul>
          {/if}

          <div class="mt-3 grid grid-cols-2 sm:grid-cols-4 gap-x-4 gap-y-1 text-[11px] text-ink-faint">
            <div>Roots: {provider.roots.length} ({providerRootsMissing(provider)} missing)</div>
            <div>Discovered: {provider.discovery.discovered_files.toLocaleString()}</div>
            <div>Parse failures: {provider.discovery.parse_failures.toLocaleString()}</div>
            <div>Cache hits: {provider.discovery.cache_hits.toLocaleString()} / misses: {provider.discovery.cache_misses.toLocaleString()}</div>
            <div>Durable sessions: {provider.ledger.durable_sessions.toLocaleString()}</div>
            <div>Available: {provider.ledger.available_sessions.toLocaleString()}</div>
            <div>Collisions: {provider.ledger.collision_sessions.toLocaleString()}</div>
            <div>Ledger: {provider.ledger.history_store_available ? 'available' : 'unavailable'}</div>
            <div>Models priced: {provider.pricing.models_priced} / {provider.pricing.models_observed}</div>
            <div>Fallback pricing: {provider.pricing.fallback_used ? 'yes' : 'no'}</div>
            <div>Rates: {provider.pricing.rates_stale ? 'stale' : 'current'}</div>
            <div>Retention risk: {retentionLabel[provider.retention.level]}</div>
          </div>

          <p class="mt-2 text-[11px] text-ink-faint italic">Quota: {provider.quota.message}</p>

          {#if provider.roots.length > 0}
            <details class="mt-2">
              <summary class="text-[11px] text-accent-chipfg cursor-pointer">Roots</summary>
              <ul class="mt-1 space-y-0.5">
                {#each provider.roots as root (root.kind + root.path)}
                  <li class="text-[11px] font-mono {root.exists ? 'text-ink-faint' : 'text-red-500'}">
                    [{root.kind}] {root.path} — {root.exists ? 'exists' : 'missing'}{root.is_default ? ' (default)' : ''}
                  </li>
                {/each}
              </ul>
            </details>
          {/if}
        </div>
      {/each}
    </div>

    <div class="mt-3 bg-card border border-edge rounded-lg px-4 py-3 max-w-3xl">
      <h3 class="text-xs font-semibold text-ink mb-1">Export</h3>
      <p class="text-[11px] text-ink-faint mb-2">
        Redacted by default — configured/default root paths are replaced with placeholders.
        Nothing else in this report (counts, flags, model names) needs redaction.
      </p>
      <label class="flex items-center gap-2 text-xs text-ink-2 mb-2">
        <input type="checkbox" bind:checked={includePaths} />
        Include exact paths in export (local only — don't paste into a public bug report)
      </label>
      <div class="flex items-center gap-2 flex-wrap">
        <button
          onclick={copyJson}
          class="px-3 py-1.5 text-xs rounded-sm bg-app border border-edge hover:bg-(--row-hover)"
        >Copy JSON</button>
        <button
          onclick={exportJson}
          disabled={exporting}
          class="px-3 py-1.5 text-xs rounded-sm bg-app border border-edge hover:bg-(--row-hover) disabled:opacity-40"
        >{exporting ? 'Exporting…' : 'Export JSON…'}</button>
        {#if copyStatus}
          <span class="text-[11px] text-ink-faint">{copyStatus}</span>
        {/if}
      </div>
      {#if exportError}
        <p class="text-xs text-red-500 mt-2">{exportError}</p>
      {/if}
    </div>
  {/if}
</section>
