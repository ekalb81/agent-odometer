<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import SessionsView from './components/SessionsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import Filters from './components/Filters.svelte';
  import type { FilterState } from './components/Filters.svelte';
  import { listSessions, onSessionUpdated, onSessionRemoved, getRates, getConfig, onRatesUpdated, onConfigUpdated, getScanStatus, onScanProgress, addDefenderExclusions } from './lib/ipc';
  import { sessionsStore } from './lib/stores/sessions.svelte';
  import { scanStore } from './lib/stores/scan.svelte';
  import { updaterStore } from './lib/stores/updater.svelte';
  import './lib/stores/theme.svelte'; // applies data-theme on import
  import { rates } from './lib/stores/rates';
  import { config } from './lib/stores/config';
  import { getVersion } from '@tauri-apps/api/app';
  import type { Harness, SessionSummary } from './lib/types';
  import type { UnlistenFn } from '@tauri-apps/api/event';

  type View = 'codex' | 'claude' | 'settings';
  let activeView: View = $state('codex');
  let appVersion = $state('');

  // ---------------------------------------------------------------------------
  // Filter state lives here (per harness) so the toolbar cluster can drive
  // whichever tab is active while both harness views stay mounted.
  // ---------------------------------------------------------------------------
  function defaultFilters(): FilterState {
    return {
      search: '',
      dateFrom: '',
      dateTo: '',
      model: '',
      showActive: true,
      showArchived: true,
      showSubagents: true,
    };
  }
  let filtersByHarness = $state<Record<Harness, FilterState>>({
    codex: defaultFilters(),
    claude_code: defaultFilters(),
  });

  const activeHarness = $derived<Harness | null>(
    activeView === 'codex' ? 'codex' : activeView === 'claude' ? 'claude_code' : null,
  );

  const toolbarSessions = $derived(
    activeHarness
      ? [...sessionsStore.map.values()].filter((s) => s.harness === activeHarness)
      : [],
  );

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

  let unlistenUpdated: UnlistenFn | null = null;
  let unlistenRemoved: UnlistenFn | null = null;
  let unlistenRates: UnlistenFn | null = null;
  let unlistenConfig: UnlistenFn | null = null;
  let unlistenScan: UnlistenFn | null = null;

  // During the initial scan, session-updated events arrive by the hundred.
  // Applying each one individually clones the store map and re-derives every
  // view per event; batching into ~150ms flushes makes the flood cheap.
  let pendingUpserts: SessionSummary[] = [];
  let flushTimer: ReturnType<typeof setTimeout> | null = null;

  function queueUpsert(s: SessionSummary) {
    pendingUpserts.push(s);
    if (flushTimer === null) {
      flushTimer = setTimeout(() => {
        const batch = pendingUpserts;
        pendingUpserts = [];
        flushTimer = null;
        sessionsStore.upsertMany(batch);
      }, 150);
    }
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

  onMount(async () => {
    try {
      const sessions = await listSessions();
      sessionsStore.replaceAll(sessions);
    } catch (e) {
      console.error('listSessions failed:', e);
    }

    try {
      scanStore.set(await getScanStatus());
    } catch (e) {
      console.error('getScanStatus failed:', e);
    }

    try {
      const card = await getRates();
      rates.set(card);
    } catch (e) {
      console.error('getRates failed:', e);
    }

    try {
      const cfg = await getConfig();
      config.set(cfg);
    } catch (e) {
      console.error('getConfig failed:', e);
    }

    unlistenUpdated = await onSessionUpdated((s) => queueUpsert(s));
    unlistenRemoved = await onSessionRemoved((id) => sessionsStore.remove(id));
    unlistenScan = await onScanProgress((status) => scanStore.set(status));
    unlistenRates = await onRatesUpdated((card) => rates.set(card));
    unlistenConfig = await onConfigUpdated(async (newConfig) => {
      config.set(newConfig);
      try {
        const sessions = await listSessions();
        sessionsStore.replaceAll(sessions);
      } catch (e) {
        console.error('listSessions after config-updated failed:', e);
      }
    });

    void getVersion().then((v) => (appVersion = v)).catch(() => {});
    void updaterStore.checkNow();
    updateCheckTimer = setInterval(() => void updaterStore.checkNow(), UPDATE_CHECK_INTERVAL_MS);
    window.addEventListener('focus', onFocusCheck);
    tickTimer = setInterval(() => (nowTick = Date.now()), 5000);
  });

  onDestroy(() => {
    if (flushTimer !== null) clearTimeout(flushTimer);
    if (updateCheckTimer !== null) clearInterval(updateCheckTimer);
    if (tickTimer !== null) clearInterval(tickTimer);
    window.removeEventListener('focus', onFocusCheck);
    unlistenUpdated?.();
    unlistenRemoved?.();
    unlistenScan?.();
    unlistenRates?.();
    unlistenConfig?.();
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

    {#if activeHarness}
      <div class="ml-auto">
        <Filters
          filters={filtersByHarness[activeHarness]}
          sessions={toolbarSessions}
          onchange={(f) => { if (activeHarness) filtersByHarness[activeHarness] = f; }}
        />
      </div>
    {/if}
  </header>

  <!-- Main content. Harness views stay mounted so filters/sort survive tab switches. -->
  <main class="flex-1 overflow-hidden">
    <div class="h-full accent-codex {activeView === 'codex' ? '' : 'hidden'}">
      <SessionsView
        harness="codex"
        active={activeView === 'codex'}
        filters={filtersByHarness.codex}
        onfilterschange={(f) => (filtersByHarness.codex = f)}
      />
    </div>
    <div class="h-full accent-claude {activeView === 'claude' ? '' : 'hidden'}">
      <SessionsView
        harness="claude_code"
        active={activeView === 'claude'}
        filters={filtersByHarness.claude_code}
        onfilterschange={(f) => (filtersByHarness.claude_code = f)}
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
