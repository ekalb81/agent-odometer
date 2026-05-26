<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import SessionsView from './components/SessionsView.svelte';
  import SettingsView from './components/SettingsView.svelte';
  import { listSessions, onSessionUpdated, onSessionRemoved, getRates, onRatesUpdated } from './lib/ipc';
  import { sessionsStore } from './lib/stores/sessions';
  import { rates } from './lib/stores/rates';
  import type { UnlistenFn } from '@tauri-apps/api/event';

  type View = 'sessions' | 'settings';
  let activeView: View = $state('sessions');

  let unlistenUpdated: UnlistenFn | null = null;
  let unlistenRemoved: UnlistenFn | null = null;
  let unlistenRates: UnlistenFn | null = null;

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

    unlistenUpdated = await onSessionUpdated((s) => sessionsStore.upsert(s));
    unlistenRemoved = await onSessionRemoved((id) => sessionsStore.remove(id));
    unlistenRates = await onRatesUpdated((card) => rates.set(card));
  });

  onDestroy(() => {
    unlistenUpdated?.();
    unlistenRemoved?.();
    unlistenRates?.();
  });
</script>

<div class="flex flex-col h-screen bg-slate-900 text-slate-100">
  <!-- Top bar -->
  <header class="flex items-center justify-between px-4 py-3 border-b-2 border-slate-700 bg-slate-800 shadow-md flex-shrink-0">
    <span class="font-semibold text-lg tracking-tight text-white">Codex Data Viewer</span>

    <nav class="flex gap-1 bg-slate-900 rounded-lg p-1 border border-slate-700">
      <button
        class="px-4 py-1.5 rounded-md text-sm font-medium transition-colors {activeView === 'sessions'
          ? 'bg-blue-600 text-white shadow'
          : 'text-slate-400 hover:text-white hover:bg-slate-700'}"
        onclick={() => (activeView = 'sessions')}
      >
        Sessions
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

  <!-- Main content -->
  <main class="flex-1 overflow-hidden">
    {#if activeView === 'sessions'}
      <SessionsView />
    {:else}
      <SettingsView />
    {/if}
  </main>
</div>
