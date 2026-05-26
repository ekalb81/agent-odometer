<script lang="ts">
  import { sessionsStore } from '../lib/stores/sessions';

  const fmt = new Intl.NumberFormat();

  function fmtTokens(n: number): string {
    return fmt.format(n);
  }

  function fmtDate(iso: string): string {
    const d = new Date(iso);
    const y = d.getFullYear();
    const mo = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    const h = String(d.getHours()).padStart(2, '0');
    const min = String(d.getMinutes()).padStart(2, '0');
    return `${y}-${mo}-${day} ${h}:${min}`;
  }

  function sessionName(s: {
    thread_name: string | null;
    first_user_message: string | null;
    working_directory: string | null;
    id: string;
  }): string {
    if (s.thread_name) return s.thread_name;
    if (s.first_user_message) return s.first_user_message;
    if (s.working_directory) {
      const parts = s.working_directory.replace(/\\/g, '/').split('/');
      const base = parts[parts.length - 1];
      if (base) return base;
    }
    return s.id.slice(0, 8);
  }

  function truncate(str: string, max: number): string {
    return str.length > max ? str.slice(0, max) + '…' : str;
  }

  function isPulsing(lastUpdatedAt: number): boolean {
    return Date.now() - lastUpdatedAt < 2000;
  }

  const totalTokens = $derived(
    sessionsStore.sorted.reduce((sum, s) => sum + s.tokens_total.total_tokens, 0),
  );
</script>

<div class="flex flex-col h-full overflow-hidden">
  <!-- Summary header -->
  <div class="flex items-center gap-6 px-4 py-2 bg-slate-800 border-b border-slate-700 flex-shrink-0 text-sm text-slate-400">
    <span>
      <span class="font-semibold text-slate-200">{sessionsStore.sorted.length}</span>
      {sessionsStore.sorted.length === 1 ? 'session' : 'sessions'}
    </span>
    <span>
      <span class="font-semibold text-slate-200">{fmtTokens(totalTokens)}</span>
      total tokens
    </span>
  </div>

  <!-- Table -->
  <div class="flex-1 overflow-auto">
    {#if sessionsStore.sorted.length === 0}
      <div class="flex flex-col items-center justify-center h-full gap-3 text-slate-500">
        <p class="text-lg">No sessions found</p>
        <p class="text-sm">Start a Codex session or check your config roots.</p>
      </div>
    {:else}
      <table class="w-full text-sm text-left border-collapse">
        <thead class="sticky top-0 bg-slate-800 z-10">
          <tr class="border-b border-slate-700">
            <th class="px-3 py-2 font-medium text-slate-300 whitespace-nowrap">Name</th>
            <th class="px-3 py-2 font-medium text-slate-300 whitespace-nowrap">ID</th>
            <th class="px-3 py-2 font-medium text-slate-300 whitespace-nowrap">Started</th>
            <th class="px-3 py-2 font-medium text-slate-300 whitespace-nowrap">Model</th>
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap">Input</th>
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap">Cached</th>
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap">Output</th>
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap">Reasoning</th>
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap">Total</th>
          </tr>
        </thead>
        <tbody>
          {#each sessionsStore.sorted as session (session.id)}
            {@const name = sessionName(session)}
            <tr
              class="border-b border-slate-700/50 hover:bg-slate-700/40 transition-colors
                     {isPulsing(session.lastUpdatedAt) ? 'bg-blue-900/20 animate-pulse' : ''}"
            >
              <!-- Name -->
              <td class="px-3 py-2 max-w-xs" title={name}>
                <div class="flex items-center gap-1.5 min-w-0">
                  <span class="truncate text-slate-100">{truncate(name, 80)}</span>
                  {#if session.archived}
                    <span class="flex-shrink-0 text-xs px-1.5 py-0.5 rounded bg-slate-600 text-slate-300">
                      archived
                    </span>
                  {/if}
                </div>
              </td>

              <!-- ID (short) -->
              <td class="px-3 py-2 font-mono text-slate-400 whitespace-nowrap">
                {session.id.slice(0, 8)}
              </td>

              <!-- Started -->
              <td class="px-3 py-2 text-slate-400 whitespace-nowrap">
                {fmtDate(session.started_at)}
              </td>

              <!-- Model -->
              <td class="px-3 py-2 text-slate-400 whitespace-nowrap max-w-[12rem]">
                <span class="truncate block" title={session.model ?? ''}>
                  {session.model ?? '—'}
                </span>
              </td>

              <!-- Token columns -->
              <td class="px-3 py-2 text-right tabular-nums text-slate-300">
                {fmtTokens(session.tokens_total.input_tokens)}
              </td>
              <td class="px-3 py-2 text-right tabular-nums text-slate-300">
                {fmtTokens(session.tokens_total.cached_input_tokens)}
              </td>
              <td class="px-3 py-2 text-right tabular-nums text-slate-300">
                {fmtTokens(session.tokens_total.output_tokens)}
              </td>
              <td class="px-3 py-2 text-right tabular-nums text-slate-300">
                {fmtTokens(session.tokens_total.reasoning_output_tokens)}
              </td>
              <td class="px-3 py-2 text-right tabular-nums font-medium text-slate-100">
                {fmtTokens(session.tokens_total.total_tokens)}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>
