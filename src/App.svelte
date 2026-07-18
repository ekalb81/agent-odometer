<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import SessionsView from './components/SessionsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import { listSessions, onSessionUpdated, onSessionRemoved, getRates, getConfig, onRatesUpdated, onConfigUpdated } from './lib/ipc';
  import { sessionsStore } from './lib/stores/sessions.svelte';
  import { rates } from './lib/stores/rates';
  import { config } from './lib/stores/config';
  import type { UnlistenFn } from '@tauri-apps/api/event';

  type View = 'codex' | 'claude' | 'settings';
  let activeView: View = $state('codex');

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
  });

  onDestroy(() => {
    unlistenUpdated?.();
    unlistenRemoved?.();
    unlistenRates?.();
    unlistenConfig?.();
  });
</script>

<div class="flex flex-col h-screen bg-slate-900 text-slate-100">
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
