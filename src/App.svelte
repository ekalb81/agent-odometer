<script lang="ts">
  import SessionsView from './components/SessionsView.svelte';
  import SettingsView from './components/SettingsView.svelte';

  type View = 'sessions' | 'settings';
  let activeView: View = $state('sessions');
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
