<script lang="ts">
  import { onMount } from 'svelte';
  import SessionsView from './components/SessionsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import InstructionsView from './components/InstructionsView.svelte';
  import Filters from './components/Filters.svelte';
  import type { FilterState } from './components/Filters.svelte';
  import { defaultFilters, type ViewScope } from './lib/sessionProjection';
  import { listSessions, onSessionUpdated, onSessionRemoved, getRates, getConfig, onRatesUpdated, onConfigUpdated, getScanStatus, onScanProgress, onInstructionScanProgress, sessionsInRanges, setTrayTotals, onOpenSettings, setConfig } from './lib/ipc';
  import { sessionsStore } from './lib/stores/sessions.svelte';
  import { scanStore } from './lib/stores/scan.svelte';
  import { instructionScanStore } from './lib/stores/instructionScan.svelte';
  import { updaterStore } from './lib/stores/updater.svelte';
  import './lib/stores/theme.svelte'; // applies data-theme on import
  import { rates } from './lib/stores/rates';
  import { config } from './lib/stores/config';
  import { defenderActionStore } from './lib/stores/defender.svelte';
  import { defenderReceiptStatus, isWindowsDefenderSurface } from './lib/defenderStatus';
  import { getVersion } from '@tauri-apps/api/app';
  import type { InstructionScanProgress, RateCard, SessionSummary } from './lib/types';
  import type { UnlistenFn } from '@tauri-apps/api/event';
  import { computeTrayTotals } from './lib/trayTotals';
  import { MutationAccumulator, RangeDataCache } from './lib/rangeData';
  import { computeFlushDelay, recordFlush } from './lib/flushCadence';
  import { configurePerformanceTracking, measureAsync, measureNextPaint, measureSync } from './lib/performance';
  import { appViews, providerIdForTab, type AppView } from './lib/appViews';
  import { providersStore } from './lib/stores/providers.svelte';
  import { providerAccent } from './lib/providerAccents';

  let activeView: AppView = $state('all');
  let appVersion = $state('');
  const appStarted = performance.now();

  // Filter state lives here per view scope so the toolbar can drive the
  // active tab while every sessions view remains mounted. 'all' is always
  // present; a provider's entry is added the first time its descriptor is
  // observed and then persists for the life of the app.
  let filtersByScope = $state<Record<string, FilterState>>({
    all: defaultFilters(),
  });

  $effect(() => {
    for (const descriptor of providersStore.descriptors) {
      if (!(descriptor.id in filtersByScope)) filtersByScope[descriptor.id] = defaultFilters();
    }
  });

  // Tabs are generated from descriptors, so the active scope is just the
  // active view's provider id (identity for every provider except the one
  // legacy short tab id) — except for the two static, non-provider views.
  const activeScope = $derived<ViewScope | null>(
    activeView === 'instructions' || activeView === 'settings' ? null : providerIdForTab(activeView),
  );

  const toolbarSessions = $derived(
    activeScope
      ? [...sessionsStore.map.values()].filter(
          (session) => activeScope === 'all' || session.harness === activeScope,
        )
      : [],
  );

  $effect(() => {
    const current = $config;
    if (activeView === 'instructions' && (!current.instructions_enabled || !current.instructions_tab_visible)) {
      activeView = 'settings';
    }
  });

  async function hideInstructionsTab() {
    const updated = { ...$config, instructions_tab_visible: false };
    await setConfig(updated);
    config.set(updated);
    activeView = 'all';
  }

  let trayRefreshGeneration = $state(0);
  let trayTimer: ReturnType<typeof setTimeout> | null = null;
  let trayJobGeneration = 0;
  let trayQueue: Promise<void> = Promise.resolve();
  const trayCache = new RangeDataCache();
  const trayMutations = new MutationAccumulator();

  // The queue serializes refresh jobs so a drain/plan never runs against a
  // cache that an in-flight fetch hasn't updated yet. A superseded job skips
  // before draining, leaving its pending mutations to the newer job.
  async function runTrayRefresh(generation: number, rateCard: RateCard): Promise<void> {
    if (generation !== trayJobGeneration) return;
    const start = new Date(); start.setHours(0, 0, 0, 0);
    const end = new Date(start); end.setDate(end.getDate() + 1); end.setMilliseconds(-1);
    const trayRange = [{ from: start.toISOString(), to: end.toISOString() }];
    // The day is the only variable in the range, so rollover forces a full fetch.
    const rangesKey = `tray:${trayRange[0].from}`;
    const ids = [...sessionsStore.map.keys()];
    const drained = trayMutations.drain();
    const plan = trayCache.plan({
      rangesKey,
      ids,
      changedIds: drained.changedIds,
      removedIds: drained.removedIds,
    });
    try {
      let results = trayCache.current();
      if (plan.mode === 'full') {
        const fetched = await measureAsync(
          'frontend.tray_range_fetch',
          () => sessionsInRanges(trayRange),
          { sessions: ids.length, fetched: ids.length, mode: 'full' },
        );
        results = trayCache.applyFull(rangesKey, ids, fetched);
      } else if (plan.mode === 'delta') {
        const fetched = plan.fetchIds.length > 0
          ? await measureAsync(
              'frontend.tray_range_fetch',
              () => sessionsInRanges(trayRange, plan.fetchIds),
              { sessions: ids.length, fetched: plan.fetchIds.length, mode: 'delta' },
            )
          : null;
        results = trayCache.applyDelta(plan.fetchIds, drained.removedIds, fetched);
      }
      if (!results) return;
      await setTrayTotals(computeTrayTotals(sessionsStore.map.values(), results[0], rateCard));
    } catch (error) {
      trayCache.invalidate();
      console.error('tray totals refresh failed:', error);
    }
  }

  $effect(() => {
    trayMutations.observe(sessionsStore.mutationLog);
    const rateCard = $rates;
    void trayRefreshGeneration;
    if (!rateCard) return;
    if (trayTimer !== null) clearTimeout(trayTimer);
    trayTimer = setTimeout(() => {
      trayTimer = null;
      const generation = ++trayJobGeneration;
      trayQueue = trayQueue
        .then(() => runTrayRefresh(generation, rateCard))
        .catch(() => {});
    }, 250);
    const now = new Date(); const next = new Date(now); next.setDate(next.getDate() + 1); next.setHours(0, 0, 1, 0);
    const boundary = setTimeout(() => { trayRefreshGeneration += 1; }, next.getTime() - now.getTime());
    return () => { clearTimeout(boundary); if (trayTimer !== null) clearTimeout(trayTimer); };
  });

  // ---------------------------------------------------------------------------
  // Auto-update: check at startup, then hourly and whenever the window
  // regains focus — the app tends to stay open for days. State lives in
  // updaterStore so Settings can offer a manual check against the same
  // update object.
  // ---------------------------------------------------------------------------
  const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;
  let updateCheckTimer: ReturnType<typeof setInterval> | null = null;

  function onFocusCheck() {
    void updaterStore.checkNow();
  }

  // ---------------------------------------------------------------------------
  // Defender-exclusion suggestion: Windows scans every session file on read,
  // which usually dominates a slow first load. Offer a one-click, UAC-gated
  // exclusion of the session folders when a scan was slow; fully dismissible.
  // ---------------------------------------------------------------------------
  const SLOW_SCAN_MS = 20_000;
  const DEFENDER_DISMISSED_KEY = 'defenderPromptDismissed';
  const visualScenario = document.documentElement.dataset.visualScenario ?? null;
  const isWindows = isWindowsDefenderSurface(navigator.userAgent, visualScenario);
  let defenderDismissed = $state(localStorage.getItem(DEFENDER_DISMISSED_KEY) === '1');
  const defenderStatus = $derived(defenderReceiptStatus($config));
  const showDefenderConfirmation = $derived(
    defenderActionStore.phase === 'success' && defenderActionStore.origin === 'banner',
  );

  const showDefenderBanner = $derived(
    isWindows &&
      !defenderDismissed &&
      scanStore.status.complete &&
      (scanStore.status.elapsed_ms ?? 0) > SLOW_SCAN_MS &&
      (defenderStatus !== 'current' || showDefenderConfirmation),
  );

  function requestDefenderExclusion() {
    void defenderActionStore.request('banner').catch(() => {});
  }

  function dismissDefenderBanner() {
    defenderDismissed = true;
    localStorage.setItem(DEFENDER_DISMISSED_KEY, '1');
    defenderActionStore.clearFeedback();
  }

  // During the initial scan, session-updated events arrive by the hundred.
  // Coalesce every id to its last ordered mutation. Removals share the same
  // batch so a pending stale upsert cannot resurrect a removed session.
  type PendingMutation =
    | { kind: 'upsert'; session: SessionSummary }
    | { kind: 'remove' };
  let pendingMutations = new Map<string, PendingMutation>();
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  let sessionsReady = false;
  const flushHistory: number[] = [];

  function flushMutations() {
    if (!sessionsReady || pendingMutations.size === 0) return;
    recordFlush(flushHistory, Date.now());
    const batch = pendingMutations;
    pendingMutations = new Map();
    const upserts: SessionSummary[] = [];
    const removals: string[] = [];
    for (const [id, mutation] of batch) {
      if (mutation.kind === 'upsert') upserts.push(mutation.session);
      else removals.push(id);
    }
    const started = performance.now();
    measureSync(
      'frontend.session_batch_apply',
      () => sessionsStore.applyMutations(upserts, removals),
      { sessions: batch.size },
    );
    measureNextPaint('frontend.session_batch_paint', started, { sessions: batch.size });
  }

  function scheduleMutationFlush() {
    if (!sessionsReady || flushTimer !== null || pendingMutations.size === 0) return;
    // The window widens under sustained streaming so continuous agent output
    // coalesces into ~1Hz-or-slower paints instead of one per parse.
    flushTimer = setTimeout(() => {
      flushTimer = null;
      flushMutations();
    }, computeFlushDelay(flushHistory, Date.now()));
  }

  function queueUpsert(s: SessionSummary) {
    pendingMutations.set(s.storage_id, { kind: 'upsert', session: s });
    scheduleMutationFlush();
  }

  function queueRemove(id: string) {
    pendingMutations.set(id, { kind: 'remove' });
    scheduleMutationFlush();
  }

  // ---------------------------------------------------------------------------
  // Status bar: watched-root count, freshness of the newest event, rate card.
  // ---------------------------------------------------------------------------
  const watchedRoots = $derived(
    $config.session_roots.length +
      $config.archive_roots.length +
      ($config.claude_session_roots?.length ?? 0),
  );

  // A parser-version bump forces one cold rescan of every transcript; say so
  // explicitly rather than leaving the user staring at an unexplained delay.
  const scanningLabel = $derived(
    scanStore.status.cold_reason === 'parse_version_changed'
      ? 'Re-indexing after update'
      : 'Scanning sessions',
  );

  const instructionScanStatus = $derived(instructionScanStore.status);
  const instructionScanActive = $derived(
    instructionScanStatus !== null &&
      instructionScanStatus.phase !== 'complete' &&
      instructionScanStatus.phase !== 'cancelled',
  );

  function instructionScanLabel(status: InstructionScanProgress): string {
    if (status.phase === 'preparing') return 'Scanning instructions… preparing roots';
    if (status.phase === 'analyzing') {
      return `Scanning instructions… analyzing ${status.files_found.toLocaleString()} files`;
    }
    if (status.roots_total === 0) return 'Scanning instructions…';
    const root = Math.min(status.roots_done + 1, status.roots_total);
    return `Scanning instructions… root ${root}/${status.roots_total} · ${status.entries_visited.toLocaleString()} entries · ${status.files_found.toLocaleString()} files`;
  }

  // Ticks every 5s so "Last event 12s ago" stays fresh without any events.
  let nowTick = $state(Date.now());
  let tickTimer: ReturnType<typeof setInterval> | null = null;

  const lastEventMs = $derived((() => {
    let max = 0;
    for (const s of sessionsStore.map.values()) {
      if (s.lastEventMs > max) max = s.lastEventMs;
    }
    return max;
  })());

  const lastEventLabel = $derived((() => {
    if (lastEventMs === 0) return null;
    const secs = Math.max(0, Math.round((nowTick - lastEventMs) / 1000));
    if (secs < 60) return `${secs}s ago`;
    const mins = Math.floor(secs / 60);
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    return new Date(lastEventMs).toLocaleDateString();
  })());

  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  const rateCardLabel = $derived((() => {
    const r = $rates;
    if (!r) return null;
    let label = `Rate card v${r.version}`;
    if (r.fetched_at) {
      const d = new Date(r.fetched_at);
      if (!isNaN(d.getTime())) label += ` · fetched ${MONTHS[d.getMonth()]} ${d.getDate()}`;
    }
    return label;
  })());

  onMount(() => {
    let disposed = false;
    let reloadGeneration = 0;
    let configEventRevision = 0;
    let scanEventRevision = 0;
    let ratesEventRevision = 0;
    const unlisteners: UnlistenFn[] = [];
    sessionsReady = false;

    async function attach(label: string, listener: Promise<UnlistenFn>): Promise<void> {
      try {
        const unlisten = await listener;
        if (disposed) unlisten();
        else unlisteners.push(unlisten);
      } catch (error) {
        console.error(`${label} listener failed:`, error);
      }
    }

    async function reloadSessions(operation: string): Promise<void> {
      const generation = ++reloadGeneration;
      if (flushTimer !== null) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      // Finish mutations from the prior source before taking a replacement
      // snapshot; only events arriving during this request are replayed onto it.
      if (sessionsReady) flushMutations();
      sessionsReady = false;
      try {
        const sessions = await measureAsync(operation, listSessions);
        if (!disposed && generation === reloadGeneration) sessionsStore.replaceAll(sessions);
      } catch (error) {
        if (!disposed && generation === reloadGeneration) {
          console.error(`${operation} failed:`, error);
        }
      } finally {
        if (!disposed && generation === reloadGeneration) {
          sessionsReady = true;
          scheduleMutationFlush();
        }
      }
    }

    void (async () => {
      // Establish event delivery before taking snapshots. Session mutations
      // remain buffered until the latest snapshot is installed.
      await Promise.all([
        attach('session-updated', onSessionUpdated((session) => {
          if (!disposed) queueUpsert(session);
        })),
        attach('session-removed', onSessionRemoved((id) => {
          if (!disposed) queueRemove(id);
        })),
        attach('scan-progress', onScanProgress((status) => {
          if (disposed) return;
          scanEventRevision += 1;
          scanStore.set(status);
        })),
        attach('instruction-scan-progress', onInstructionScanProgress((status) => {
          if (!disposed) instructionScanStore.set(status);
        })),
        attach('rates-updated', onRatesUpdated((card) => {
          if (disposed) return;
          ratesEventRevision += 1;
          rates.set(card);
        })),
        attach('config-updated', onConfigUpdated((newConfig) => {
          if (disposed) return;
          configEventRevision += 1;
          const previous = $config;
          const sourcesChanged = JSON.stringify([
            previous.session_roots,
            previous.archive_roots,
            previous.session_index_path,
            previous.claude_session_roots,
          ]) !== JSON.stringify([
            newConfig.session_roots,
            newConfig.archive_roots,
            newConfig.session_index_path,
            newConfig.claude_session_roots,
          ]);
          const instructionSourcesChanged =
            previous.instructions_enabled !== newConfig.instructions_enabled ||
            JSON.stringify(previous.instruction_roots) !== JSON.stringify(newConfig.instruction_roots);
          config.set(newConfig);
          if (instructionSourcesChanged) instructionScanStore.clearCurrent();
          configurePerformanceTracking(newConfig.performance_tracking_enabled);
          if (sourcesChanged) void reloadSessions('frontend.config_list_sessions');
        })),
        attach('open-settings', onOpenSettings(() => {
          if (!disposed) activeView = 'settings';
        })),
      ]);
      if (disposed) return;

      const configRevision = configEventRevision;
      try {
        const cfg = await getConfig();
        if (!disposed && configRevision === configEventRevision) {
          config.set(cfg);
          configurePerformanceTracking(cfg.performance_tracking_enabled);
        }
      } catch (error) {
        console.error('getConfig failed:', error);
      }
      if (disposed) return;

      const scanRevision = scanEventRevision;
      const rateRevision = ratesEventRevision;
      await Promise.allSettled([
        reloadSessions('frontend.initial_list_sessions'),
        providersStore.init(),
        measureAsync('frontend.initial_scan_status', getScanStatus).then((status) => {
          if (!disposed && scanRevision === scanEventRevision) scanStore.set(status);
        }).catch((error) => console.error('getScanStatus failed:', error)),
        measureAsync('frontend.initial_rates', getRates).then((card) => {
          if (!disposed && rateRevision === ratesEventRevision) rates.set(card);
        }).catch((error) => console.error('getRates failed:', error)),
      ]);
      if (disposed) return;

      void getVersion().then((version) => {
        if (!disposed) appVersion = version;
      }).catch(() => {});
      void updaterStore.checkNow();
      updateCheckTimer = setInterval(() => void updaterStore.checkNow(), UPDATE_CHECK_INTERVAL_MS);
      window.addEventListener('focus', onFocusCheck);
      tickTimer = setInterval(() => (nowTick = Date.now()), 5000);
      measureNextPaint('frontend.app_ready', appStarted, { sessions: sessionsStore.map.size });
    })();

    return () => {
      disposed = true;
      reloadGeneration += 1;
      sessionsReady = false;
      pendingMutations.clear();
      if (flushTimer !== null) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      if (updateCheckTimer !== null) clearInterval(updateCheckTimer);
      if (tickTimer !== null) clearInterval(tickTimer);
      window.removeEventListener('focus', onFocusCheck);
      for (const unlisten of unlisteners) unlisten();
    };
  });

  const tabClass = (isActive: boolean, fill: string) =>
    `px-4 py-[5px] rounded-md text-xs transition-colors ${
      isActive ? `${fill} text-white font-semibold` : 'text-ink-muted hover:text-ink font-normal'
    }`;
</script>

<div
  class="flex flex-col h-screen bg-app text-ink text-[13px] {activeScope === 'claude_code' ? 'accent-claude' : 'accent-codex'}"
  data-visual-scenario={visualScenario ?? undefined}
>
  <!-- Update banner -->
  {#if updaterStore.available}
    <div class="flex items-center justify-center gap-3 px-4 py-1.5 bg-chrome border-b border-edge text-xs text-ink-2 shrink-0">
      {#if updaterStore.phase === 'installing'}
        <span>
          Downloading v{updaterStore.available.version}…
          {#if updaterStore.total > 0}
            {Math.min(100, Math.round((updaterStore.progress / updaterStore.total) * 100))}%
          {/if}
        </span>
      {:else}
        <span>Version {updaterStore.available.version} is available.</span>
        <button
          onclick={() => void updaterStore.install()}
          class="px-2.5 py-0.5 rounded-lg bg-accent-tab text-white text-xs font-medium hover:opacity-90 transition-opacity"
        >
          Update &amp; restart
        </button>
        {#if updaterStore.phase === 'error'}
          <span class="text-xs text-red-400">Install failed — see console; you can retry.</span>
        {/if}
      {/if}
    </div>
  {/if}

  <!-- Defender-exclusion suggestion (Windows, slow scan only) -->
  {#if showDefenderBanner}
    <div class="flex flex-wrap items-center justify-center gap-x-3 gap-y-1 px-4 py-1.5 bg-chrome border-b border-edge text-xs text-ink-2 shrink-0">
      {#if defenderActionStore.phase === 'pending'}
        <span role="status" aria-live="polite">Waiting for approval and Windows Defender verification…</span>
      {:else if showDefenderConfirmation}
        <span role="status" aria-live="polite">Verified — the existing session folders are excluded. Future scans can use the change.</span>
        <button onclick={dismissDefenderBanner} class="px-2 py-0.5 rounded-sm text-xs text-ink-muted hover:text-ink transition-colors">Done</button>
      {:else}
        <span>
          That scan took {Math.round((scanStore.status.elapsed_ms ?? 0) / 1000)}s — antivirus scanning of session files is usually the biggest cost.
          You can exclude your session folders (Codex + Claude Code data only) from Windows Defender.
        </span>
        <button
          onclick={requestDefenderExclusion}
          class="px-2.5 py-0.5 rounded-lg bg-card border border-edge text-ink text-xs font-medium hover:bg-app transition-colors"
          title="Opens a Windows administrator prompt; excluded folders are no longer scanned for threats"
        >
          Add exclusions…
        </button>
        <button onclick={dismissDefenderBanner} class="px-2 py-0.5 rounded-sm text-xs text-ink-muted hover:text-ink transition-colors">No thanks</button>
        {#if defenderActionStore.phase === 'error' && defenderActionStore.origin === 'banner'}
          <span class="text-xs text-red-400" role="alert">{defenderActionStore.error}</span>
        {/if}
      {/if}
    </div>
  {/if}

  <!-- Toolbar -->
  <header class="flex items-center gap-5 px-4 h-12 bg-chrome border-b border-edge shrink-0">
    <!-- Gauge-O wordmark. The ring/hub follow the text color; the needle is
         always brand orange (#e8935a). -->
    <span class="font-bold text-[15px] tracking-[-0.015em] leading-none text-ink whitespace-nowrap">
      <svg width="12.5" height="12.5" viewBox="0 0 96 96" class="align-[-1px] mr-[0.5px] inline" aria-hidden="true">
        <circle cx="48" cy="48" r="38" fill="none" stroke="currentColor" stroke-width="14" stroke-dasharray="4.6 5.8"/>
        <line x1="41" y1="55" x2="80.9" y2="15.1" stroke="#e8935a" stroke-width="10" stroke-linecap="round"/>
        <circle cx="48" cy="48" r="10" fill="currentColor"/>
      </svg><span class="sr-only">O</span>dometer
      {#if appVersion}
        <span class="ml-1 text-[10px] font-mono font-normal text-ink-faint align-middle">v{appVersion}</span>
      {/if}
    </span>

    <nav class="flex bg-app rounded-lg p-[2px] gap-[2px] border border-edge" aria-label="Views">
      {#each appViews(providersStore.descriptors) as view (view.id)}
        {#if view.id !== 'instructions' || ($config.instructions_enabled && $config.instructions_tab_visible)}
          <button class={tabClass(activeView === view.id, providerAccent(providerIdForTab(view.id)).tabFill)} onclick={() => (activeView = view.id)}>
            {view.label}
          </button>
        {/if}
      {/each}
    </nav>

    {#if activeScope}
      <div class="ml-auto">
        {#key activeScope}
          <Filters
            filters={filtersByScope[activeScope] ?? defaultFilters()}
            sessions={toolbarSessions}
            onchange={(f) => { if (activeScope) filtersByScope[activeScope] = f; }}
          />
        {/key}
      </div>
    {/if}
  </header>

  <!-- Main content. Harness views stay mounted so filters/sort survive tab switches. -->
  <main class="flex-1 overflow-hidden">
    <div class="h-full {activeView === 'all' ? '' : 'hidden'}">
      <SessionsView
        harness="all"
        active={activeView === 'all'}
        filters={filtersByScope.all}
        onfilterschange={(f) => (filtersByScope.all = f)}
      />
    </div>
    {#each providersStore.descriptors as descriptor (descriptor.id)}
      <div class="h-full {providerAccent(descriptor.id).accentClass ?? ''} {activeScope === descriptor.id ? '' : 'hidden'}">
        <SessionsView
          harness={descriptor.id}
          active={activeScope === descriptor.id}
          filters={filtersByScope[descriptor.id] ?? defaultFilters()}
          onfilterschange={(f) => (filtersByScope[descriptor.id] = f)}
        />
      </div>
    {/each}
    {#if activeView === 'instructions'}
      <InstructionsView onhide={hideInstructionsTab} />
    {/if}
    {#if activeView === 'settings'}
      <SettingsView onopeninstructions={() => (activeView = 'instructions')} />
    {/if}
  </main>

  <!-- Status bar -->
  <footer class="flex items-center gap-4 px-4 h-7 bg-chrome border-t border-edge text-[11px] text-ink-faint shrink-0">
    {#if !scanStore.status.complete}
      <span class="flex items-center gap-1.5" role="status">
        <svg class="w-3 h-3 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <circle cx="12" cy="12" r="10" stroke="currentColor" stroke-opacity="0.25" stroke-width="4" />
          <path d="M22 12a10 10 0 0 0-10-10" stroke="currentColor" stroke-width="4" stroke-linecap="round" />
        </svg>
        {#if scanStore.status.total > 0}
          {scanningLabel}… {scanStore.status.done}/{scanStore.status.total} files
        {:else}
          {scanningLabel}…
        {/if}
      </span>
    {:else}
      <span class="flex items-center gap-[5px]">
        <span class="w-1.5 h-1.5 rounded-full bg-pos"></span>
        Watching {watchedRoots} {watchedRoots === 1 ? 'root' : 'roots'}
      </span>
    {/if}
    {#if instructionScanActive && instructionScanStatus}
      <span class="flex items-center gap-1.5 min-w-0" role="status">
        <svg class="w-3 h-3 animate-spin shrink-0" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <circle cx="12" cy="12" r="10" stroke="currentColor" stroke-opacity="0.25" stroke-width="4" />
          <path d="M22 12a10 10 0 0 0-10-10" stroke="currentColor" stroke-width="4" stroke-linecap="round" />
        </svg>
        <span class="truncate">{instructionScanLabel(instructionScanStatus)}</span>
      </span>
    {/if}
    {#if lastEventLabel}
      <span>Last event {lastEventLabel}</span>
    {/if}
    {#if rateCardLabel}
      <span class="ml-auto font-mono">{rateCardLabel}</span>
    {/if}
  </footer>
</div>
