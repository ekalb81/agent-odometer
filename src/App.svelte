<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import SessionsView from './components/SessionsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import { listSessions, onSessionUpdated, onSessionRemoved, getRates, getConfig, onRatesUpdated, onConfigUpdated } from './lib/ipc';
  import { sessionsStore } from './lib/stores/sessions.svelte';
  import { rates } from './lib/stores/rates';
  import { config } from './lib/stores/config';
  import { check, type Update } from '@tauri-apps/plugin-updater';
  import { relaunch } from '@tauri-apps/plugin-process';
  import type { UnlistenFn } from '@tauri-apps/api/event';

  type View = 'codex' | 'claude' | 'settings';
  let activeView: View = $state('codex');

  // ---------------------------------------------------------------------------
  // Auto-update: check once at startup; failures (offline, endpoint not yet
  // public, dev build) are silent — updating is never load-bearing.
  // ---------------------------------------------------------------------------
  let availableUpdate = $state<Update | null>(null);
  let updateState = $state<'idle' | 'installing' | 'error'>('idle');
  let updateProgress = $state(0);
  let updateTotal = $state(0);

  async function checkForUpdate() {
    try {
      const update = await check();
      if (update) availableUpdate = update;
    } catch (e) {
      console.debug('update check skipped:', e);
    }
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

  let unlistenUpdated: UnlistenFn | null = null;
  let unlistenRemoved: UnlistenFn | null = null;
  let unlistenRates: UnlistenFn | null = null;
  let unlistenConfig: UnlistenFn | null = null;

  onMount(async () => {
    try {
      const sessions = await listSessions();
      sessionsStore.replaceAll(sessions);
    } catch (e) {
      console.error('listSessions failed:', e);
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

    unlistenUpdated = await onSessionUpdated((s) => sessionsStore.upsert(s));
    unlistenRemoved = await onSessionRemoved((id) => sessionsStore.remove(id));
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
  });

  onDestroy(() => {
    unlistenUpdated?.();
    unlistenRemoved?.();
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

  <!-- Top bar -->
  <header class="flex items-center justify-between px-4 py-3 border-b-2 border-slate-700 bg-slate-800 shadow-md flex-shrink-0">
    <span class="font-semibold text-lg tracking-tight text-white">Agent Activity Viewer</span>

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
      <SessionsView harness="codex" />
    </div>
    <div class="h-full {activeView === 'claude' ? '' : 'hidden'}">
      <SessionsView harness="claude_code" />
    </div>
    {#if activeView === 'settings'}
      <SettingsView />
    {/if}
  </main>
</div>
