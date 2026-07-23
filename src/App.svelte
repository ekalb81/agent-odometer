<script lang="ts">
  import { onMount } from 'svelte';
  import SessionsView from './components/SessionsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import Filters from './components/Filters.svelte';
  import type { FilterState } from './components/Filters.svelte';
  import { defaultFilters, type ViewScope } from './lib/sessionProjection';
  import { listSessions, onSessionUpdated, onSessionRemoved, getRates, getConfig, onRatesUpdated, onConfigUpdated, getScanStatus, onScanProgress, addDefenderExclusions, sessionsInRanges, setTrayTotals, onOpenSettings } from './lib/ipc';
  import { sessionsStore } from './lib/stores/sessions.svelte';
  import { scanStore } from './lib/stores/scan.svelte';
  import { updaterStore } from './lib/stores/updater.svelte';
  import './lib/stores/theme.svelte'; // applies data-theme on import
  import { rates } from './lib/stores/rates';
  import { config } from './lib/stores/config';
  import { getVersion } from '@tauri-apps/api/app';
  import type { SessionSummary } from './lib/types';
  import type { UnlistenFn } from '@tauri-apps/api/event';
  import { apiCostFromBuckets, creditsFromBuckets, formatCredits } from './lib/credits';
  import { configurePerformanceTracking, measureAsync, measureNextPaint, measureSync } from './lib/performance';

  type View = 'all' | 'codex' | 'claude' | 'settings';
  let activeView: View = $state('all');
  let appVersion = $state('');
  const appStarted = performance.now();

  // Filter state lives here per view scope so the toolbar can drive the
  // active tab while every sessions view remains mounted.
  let filtersByScope = $state<Record<ViewScope, FilterState>>({
    all: defaultFilters(),
    codex: defaultFilters(),
    claude_code: defaultFilters(),
  });

  const activeScope = $derived<ViewScope | null>(
    activeView === 'all'
      ? 'all'
      : activeView === 'codex'
        ? 'codex'
        : activeView === 'claude'
          ? 'claude_code'
          : null,
  );

  const toolbarSessions = $derived(
    activeScope
      ? [...sessionsStore.map.values()].filter(
          (session) => activeScope === 'all' || session.harness === activeScope,
        )
      : [],
  );

  let trayRefreshGeneration = $state(0);
  let trayTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    void sessionsStore.map;
    const rateCard = $rates;
    void trayRefreshGeneration;
    if (!rateCard) return;
    if (trayTimer !== null) clearTimeout(trayTimer);
    trayTimer = setTimeout(async () => {
      const start = new Date(); start.setHours(0, 0, 0, 0);
      const end = new Date(start); end.setDate(end.getDate() + 1); end.setMilliseconds(-1);
      try {
        const [ranges] = await measureAsync(
          'frontend.tray_range_fetch',
          () => sessionsInRanges([{ from: start.toISOString(), to: end.toISOString() }]),
          { sessions: sessionsStore.map.size },
        );
        let tokens = 0; let codexCredits = 0; let codexApi = 0; let claudeUsd = 0;
        let unlimited = 0; let missingCredits = false; let missingApi = false; let missingClaude = false;
        for (const session of sessionsStore.map.values()) {
          const range = ranges[session.id]; if (!range) continue;
          tokens += range.tokens.total_tokens;
          const plan = creditsFromBuckets(range.buckets, rateCard, session.harness);
          if (session.harness === 'codex') {
            if (session.credits_unlimited) unlimited++; else codexCredits += plan.total;
            missingCredits ||= plan.missingModels.length > 0;
            const api = apiCostFromBuckets(range.buckets, rateCard, session.harness);
            codexApi += api?.total ?? 0; missingApi ||= !api || api.missingModels.length > 0;
          } else {
            claudeUsd += plan.total; missingClaude ||= plan.missingModels.length > 0;
          }
        }
        const creditText = unlimited > 0 && codexCredits === 0 ? `unlimited (${unlimited})` : `${codexCredits.toFixed(2)}${unlimited ? ` + ${unlimited} unlimited` : ''}${missingCredits ? ' · fallback' : ''}`;
        await setTrayTotals({ tokens: tokens.toLocaleString(), codex_credits: creditText,
          codex_api_usd: missingApi ? 'unavailable · missing direct rate' : formatCredits(codexApi, 'USD'),
          claude_usd: `${formatCredits(claudeUsd, 'USD')}${missingClaude ? ' · fallback' : ''}` });
      } catch (error) { console.error('tray totals refresh failed:', error); }
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
  const isWindows = navigator.userAgent.includes('Windows');
  let defenderDismissed = $state(localStorage.getItem(DEFENDER_DISMISSED_KEY) === '1');
  let defenderRequested = $state(false);
  let defenderError = $state<string | null>(null);

  const showDefenderBanner = $derived(
    isWindows &&
      !defenderDismissed &&
      scanStore.status.complete &&
      (scanStore.status.elapsed_ms ?? 0) > SLOW_SCAN_MS,
  );

  async function requestDefenderExclusion() {
    defenderError = null;
    try {
      await addDefenderExclusions();
      defenderRequested = true;
    } catch (e) {
      defenderError = String(e);
    }
  }

  function dismissDefenderBanner() {
    defenderDismissed = true;
    localStorage.setItem(DEFENDER_DISMISSED_KEY, '1');
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

  function flushMutations() {
    if (!sessionsReady || pendingMutations.size === 0) return;
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
    flushTimer = setTimeout(() => {
      flushTimer = null;
      flushMutations();
    }, 150);
  }

  function queueUpsert(s: SessionSummary) {
    pendingMutations.set(s.id, { kind: 'upsert', session: s });
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
          config.set(newConfig);
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

<div class="flex flex-col h-screen bg-app text-ink text-[13px] {activeView === 'claude' ? 'accent-claude' : 'accent-codex'}">
  <!-- Update banner -->
  {#if updaterStore.available}
    <div class="flex items-center justify-center gap-3 px-4 py-1.5 bg-chrome border-b border-edge text-xs text-ink-2 flex-shrink-0">
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
    <div class="flex flex-wrap items-center justify-center gap-x-3 gap-y-1 px-4 py-1.5 bg-chrome border-b border-edge text-xs text-ink-2 flex-shrink-0">
      {#if defenderRequested}
        <span>Approve the Windows security prompt to finish adding the exclusions. Takes effect next launch.</span>
        <button onclick={dismissDefenderBanner} class="px-2 py-0.5 rounded text-xs text-ink-muted hover:text-ink transition-colors">Done</button>
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
        <button onclick={dismissDefenderBanner} class="px-2 py-0.5 rounded text-xs text-ink-muted hover:text-ink transition-colors">No thanks</button>
        {#if defenderError}
          <span class="text-xs text-red-400">{defenderError}</span>
        {/if}
      {/if}
    </div>
  {/if}

  <!-- Toolbar -->
  <header class="flex items-center gap-5 px-4 h-12 bg-chrome border-b border-edge flex-shrink-0">
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
      <button class={tabClass(activeView === 'all', 'bg-ink !text-app')} onclick={() => (activeView = 'all')}>
        All
      </button>
      <button class={tabClass(activeView === 'codex', 'bg-[#2b58c9]')} onclick={() => (activeView = 'codex')}>
        Codex
      </button>
      <button class={tabClass(activeView === 'claude', 'bg-[#e8935a]')} onclick={() => (activeView = 'claude')}>
        Claude Code
      </button>
      <button class={tabClass(activeView === 'settings', 'bg-ink !text-app')} onclick={() => (activeView = 'settings')}>
        Settings
      </button>
    </nav>

    {#if activeScope}
      <div class="ml-auto">
        {#key activeScope}
          <Filters
            filters={filtersByScope[activeScope]}
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
    <div class="h-full accent-codex {activeView === 'codex' ? '' : 'hidden'}">
      <SessionsView
        harness="codex"
        active={activeView === 'codex'}
        filters={filtersByScope.codex}
        onfilterschange={(f) => (filtersByScope.codex = f)}
      />
    </div>
    <div class="h-full accent-claude {activeView === 'claude' ? '' : 'hidden'}">
      <SessionsView
        harness="claude_code"
        active={activeView === 'claude'}
        filters={filtersByScope.claude_code}
        onfilterschange={(f) => (filtersByScope.claude_code = f)}
      />
    </div>
    {#if activeView === 'settings'}
      <SettingsView />
    {/if}
  </main>

  <!-- Status bar -->
  <footer class="flex items-center gap-4 px-4 h-7 bg-chrome border-t border-edge text-[11px] text-ink-faint flex-shrink-0">
    {#if !scanStore.status.complete}
      <span class="flex items-center gap-1.5" role="status">
        <svg class="w-3 h-3 animate-spin" viewBox="0 0 24 24" fill="none" aria-hidden="true">
          <circle cx="12" cy="12" r="10" stroke="currentColor" stroke-opacity="0.25" stroke-width="4" />
          <path d="M22 12a10 10 0 0 0-10-10" stroke="currentColor" stroke-width="4" stroke-linecap="round" />
        </svg>
        {#if scanStore.status.total > 0}
          Scanning sessions… {scanStore.status.done}/{scanStore.status.total} files
        {:else}
          Scanning sessions…
        {/if}
      </span>
    {:else}
      <span class="flex items-center gap-[5px]">
        <span class="w-1.5 h-1.5 rounded-full bg-pos"></span>
        Watching {watchedRoots} {watchedRoots === 1 ? 'root' : 'roots'}
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
