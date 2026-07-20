<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import SessionsView from './components/SessionsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import { listSessions, onSessionUpdated, onSessionRemoved, getRates, getConfig, onRatesUpdated, onConfigUpdated, getScanStatus, onScanProgress, addDefenderExclusions } from './lib/ipc';
  import { sessionsStore } from './lib/stores/sessions.svelte';
  import { scanStore } from './lib/stores/scan.svelte';
  import { rates } from './lib/stores/rates';
  import { config } from './lib/stores/config';
  import { check, type Update } from '@tauri-apps/plugin-updater';
  import { relaunch } from '@tauri-apps/plugin-process';
  import type { UnlistenFn } from '@tauri-apps/api/event';
  import type { SessionSummary } from './lib/types';

  type View = 'codex' | 'claude' | 'settings';
  let activeView: View = $state('codex');

  // ---------------------------------------------------------------------------
  // Auto-update: check at startup, then hourly and whenever the window
  // regains focus — the app tends to stay open for days. Failures (offline,
  // dev build) are silent; updating is never load-bearing.
  // ---------------------------------------------------------------------------
  let availableUpdate = $state<Update | null>(null);
  let updateState = $state<'idle' | 'installing' | 'error'>('idle');
  let updateProgress = $state(0);
  let updateTotal = $state(0);

  const UPDATE_CHECK_INTERVAL_MS = 60 * 60 * 1000;
  let updateCheckTimer: ReturnType<typeof setInterval> | null = null;

  async function checkForUpdate() {
    // Already found one (banner is showing) or mid-install: nothing to gain.
    if (availableUpdate || updateState === 'installing') return;
    try {
      const update = await check();
      if (update) availableUpdate = update;
    } catch (e) {
      console.debug('update check skipped:', e);
    }
  }

  function onFocusCheck() {
    void checkForUpdate();
  }

  async function installUpdate() {
    if (!availableUpdate || updateState === 'installing') return;
    updateState = 'installing';
    updateProgress = 0;
    updateTotal = 0;
    try {
      await availableUpdate.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          updateTotal = event.data.contentLength ?? 0;
        } else if (event.event === 'Progress') {
          updateProgress += event.data.chunkLength;
        }
      });
      // Windows exits into the installer on its own; elsewhere relaunch now.
      await relaunch();
    } catch (e) {
      console.error('update install failed:', e);
      updateState = 'error';
    }
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

    void checkForUpdate();
    updateCheckTimer = setInterval(() => void checkForUpdate(), UPDATE_CHECK_INTERVAL_MS);
    window.addEventListener('focus', onFocusCheck);
  });

  onDestroy(() => {
    if (flushTimer !== null) clearTimeout(flushTimer);
    if (updateCheckTimer !== null) clearInterval(updateCheckTimer);
    window.removeEventListener('focus', onFocusCheck);
    unlistenUpdated?.();
    unlistenRemoved?.();
    unlistenScan?.();
    unlistenRates?.();
    unlistenConfig?.();
  });
</script>

<div class="flex flex-col h-screen bg-slate-900 text-slate-100">
  <!-- Update banner -->
  {#if availableUpdate}
    <div class="flex items-center justify-center gap-3 px-4 py-1.5 bg-sky-900/60 border-b border-sky-700/60 text-sm text-sky-100 flex-shrink-0">
      {#if updateState === 'installing'}
        <span>
          Downloading v{availableUpdate.version}…
          {#if updateTotal > 0}
            {Math.min(100, Math.round((updateProgress / updateTotal) * 100))}%
          {/if}
        </span>
      {:else}
        <span>Version {availableUpdate.version} is available.</span>
        <button
          onclick={installUpdate}
          class="px-2.5 py-0.5 rounded bg-sky-600 hover:bg-sky-500 text-white text-xs font-medium transition-colors"
        >
          Update &amp; restart
        </button>
        {#if updateState === 'error'}
          <span class="text-xs text-red-300">Install failed — see console; you can retry.</span>
        {/if}
      {/if}
    </div>
  {/if}

  <!-- Defender-exclusion suggestion (Windows, slow scan only) -->
  {#if showDefenderBanner}
    <div class="flex flex-wrap items-center justify-center gap-x-3 gap-y-1 px-4 py-1.5 bg-slate-800 border-b border-slate-700 text-sm text-slate-300 flex-shrink-0">
      {#if defenderRequested}
        <span>Approve the Windows security prompt to finish adding the exclusions. Takes effect next launch.</span>
        <button onclick={dismissDefenderBanner} class="px-2 py-0.5 rounded text-xs text-slate-400 hover:text-slate-200 transition-colors">Done</button>
      {:else}
        <span>
          That scan took {Math.round((scanStore.status.elapsed_ms ?? 0) / 1000)}s — antivirus scanning of session files is usually the biggest cost.
          You can exclude your session folders (Codex + Claude Code data only) from Windows Defender.
        </span>
        <button
          onclick={requestDefenderExclusion}
          class="px-2.5 py-0.5 rounded bg-slate-600 hover:bg-slate-500 text-white text-xs font-medium transition-colors"
          title="Opens a Windows administrator prompt; excluded folders are no longer scanned for threats"
        >
          Add exclusions…
        </button>
        <button onclick={dismissDefenderBanner} class="px-2 py-0.5 rounded text-xs text-slate-400 hover:text-slate-200 transition-colors">No thanks</button>
        {#if defenderError}
          <span class="text-xs text-red-300">{defenderError}</span>
        {/if}
      {/if}
    </div>
  {/if}

  <!-- Top bar -->
  <header class="flex items-center justify-between px-4 py-3 border-b-2 border-slate-700 bg-slate-800 shadow-md flex-shrink-0">
    <span class="font-semibold text-lg tracking-tight text-white">Odometer</span>

    <nav class="flex gap-1 bg-slate-900 rounded-lg p-1 border border-slate-700">
      <button
        class="px-4 py-1.5 rounded-md text-sm font-medium transition-colors {activeView === 'codex'
          ? 'bg-blue-600 text-white shadow'
          : 'text-slate-400 hover:text-white hover:bg-slate-700'}"
        onclick={() => (activeView = 'codex')}
      >
        Codex
      </button>
      <button
        class="px-4 py-1.5 rounded-md text-sm font-medium transition-colors {activeView === 'claude'
          ? 'bg-orange-600 text-white shadow'
          : 'text-slate-400 hover:text-white hover:bg-slate-700'}"
        onclick={() => (activeView = 'claude')}
      >
        Claude Code
      </button>
      <button
        class="px-4 py-1.5 rounded-md text-sm font-medium transition-colors {activeView === 'settings'
          ? 'bg-blue-600 text-white shadow'
          : 'text-slate-400 hover:text-white hover:bg-slate-700'}"
        onclick={() => (activeView = 'settings')}
      >
        Settings
      </button>
    </nav>
  </header>

  <!-- Main content. Harness views stay mounted so filters/sort survive tab switches. -->
  <main class="flex-1 overflow-hidden">
    <div class="h-full {activeView === 'codex' ? '' : 'hidden'}">
      <SessionsView harness="codex" active={activeView === 'codex'} />
    </div>
    <div class="h-full {activeView === 'claude' ? '' : 'hidden'}">
      <SessionsView harness="claude_code" active={activeView === 'claude'} />
    </div>
    {#if activeView === 'settings'}
      <SettingsView />
    {/if}
  </main>
</div>
