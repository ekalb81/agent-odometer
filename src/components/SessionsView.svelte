<script lang="ts">
  import { sessionsStore } from '../lib/stores/sessions';
  import Filters from './Filters.svelte';
  import type { FilterState } from './Filters.svelte';
  import RowDrawer from './RowDrawer.svelte';
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

  // ---------------------------------------------------------------------------
  // Filter state
  // ---------------------------------------------------------------------------
  let filters = $state<FilterState>({
    search: '',
    dateFrom: '',
    dateTo: '',
    model: '',
    showActive: true,
    showArchived: true,
  });

  function defaultFilters(): FilterState {
    return { search: '', dateFrom: '', dateTo: '', model: '', showActive: true, showArchived: true };
  }

  // ---------------------------------------------------------------------------
  // Sort state
  // ---------------------------------------------------------------------------
  type SortKey = 'name' | 'started' | 'model' | 'input' | 'cached' | 'output' | 'reasoning' | 'total' | null;
  type SortDir = 'asc' | 'desc';

  let sortKey = $state<SortKey>(null);
  let sortDir = $state<SortDir>('asc');

  function toggleSort(key: SortKey) {
    if (sortKey === key) {
      if (sortDir === 'asc') {
        sortDir = 'desc';
      } else {
        // Third click clears the sort.
        sortKey = null;
        sortDir = 'asc';
      }
    } else {
      sortKey = key;
      sortDir = 'asc';
    }
  }

  // ---------------------------------------------------------------------------
  // Filtered + sorted list
  // ---------------------------------------------------------------------------
  const allSessions = $derived([...sessionsStore.map.values()]);

  const filtered = $derived((() => {
    const lc = filters.search.toLowerCase();
    return allSessions.filter((s) => {
      // Status filter.
      if (s.archived && !filters.showArchived) return false;
      if (!s.archived && !filters.showActive) return false;

      // Date range — compare against local date portion of started_at.
      const startedDate = s.started_at.slice(0, 10);
      if (filters.dateFrom && startedDate < filters.dateFrom) return false;
      if (filters.dateTo && startedDate > filters.dateTo) return false;

      // Model filter.
      if (filters.model && s.model !== filters.model) return false;

      // Free-text search.
      if (lc) {
        const haystack = [
          s.thread_name ?? '',
          s.id,
          s.first_user_message ?? '',
          s.working_directory ?? '',
        ].join('\0').toLowerCase();
        if (!haystack.includes(lc)) return false;
      }

      return true;
    });
  })());

  function compareSession(a: (typeof allSessions)[number], b: (typeof allSessions)[number]): number {
    if (sortKey === null) {
      // Default: last_event_at desc.
      return new Date(b.last_event_at).getTime() - new Date(a.last_event_at).getTime();
    }
    let cmp = 0;
    switch (sortKey) {
      case 'name':
        cmp = sessionName(a).localeCompare(sessionName(b));
        break;
      case 'started':
        cmp = new Date(a.started_at).getTime() - new Date(b.started_at).getTime();
        break;
      case 'model':
        cmp = (a.model ?? '').localeCompare(b.model ?? '');
        break;
      case 'input':
        cmp = a.tokens_total.input_tokens - b.tokens_total.input_tokens;
        break;
      case 'cached':
        cmp = a.tokens_total.cached_input_tokens - b.tokens_total.cached_input_tokens;
        break;
      case 'output':
        cmp = a.tokens_total.output_tokens - b.tokens_total.output_tokens;
        break;
      case 'reasoning':
        cmp = a.tokens_total.reasoning_output_tokens - b.tokens_total.reasoning_output_tokens;
        break;
      case 'total':
        cmp = a.tokens_total.total_tokens - b.tokens_total.total_tokens;
        break;
    }
    return sortDir === 'asc' ? cmp : -cmp;
  }

  const displayed = $derived([...filtered].sort(compareSession));

  const filteredTotal = $derived(
    filtered.reduce((sum, s) => sum + s.tokens_total.total_tokens, 0),
  );

  // ---------------------------------------------------------------------------
  // Drawer state — tracked by session id only (lookup from the map).
  // ---------------------------------------------------------------------------
  let openSessionId = $state<string | null>(null);

  const openSession = $derived(
    openSessionId !== null ? (sessionsStore.map.get(openSessionId) ?? null) : null,
  );

  function openDrawer(id: string) {
    openSessionId = id;
  }

  function closeDrawer() {
    openSessionId = null;
  }

  // ---------------------------------------------------------------------------
  // Sort header helper
  // ---------------------------------------------------------------------------
  function ariaSortAttr(key: SortKey): 'ascending' | 'descending' | 'none' {
    if (sortKey !== key) return 'none';
    return sortDir === 'asc' ? 'ascending' : 'descending';
  }

  function caretFor(key: SortKey): string {
    if (sortKey !== key) return '';
    return sortDir === 'asc' ? ' ▲' : ' ▼';
  }
</script>

<div class="flex flex-col h-full overflow-hidden">
  <!-- Filters toolbar -->
  <Filters
    {filters}
    sessions={allSessions}
    onchange={(f) => { filters = f; }}
  />

  <!-- Summary header -->
  <div class="flex items-center gap-6 px-4 py-2 bg-slate-800 border-b border-slate-700 flex-shrink-0 text-sm text-slate-400">
    <span>
      Showing
      <span class="font-semibold text-slate-200">{displayed.length}</span>
      of
      <span class="font-semibold text-slate-200">{allSessions.length}</span>
      {allSessions.length === 1 ? 'session' : 'sessions'}
    </span>
    <span>
      <span class="font-semibold text-slate-200">{fmtTokens(filteredTotal)}</span>
      total tokens
    </span>
  </div>

  <!-- Table -->
  <div class="flex-1 overflow-auto">
    {#if allSessions.length === 0}
      <div class="flex flex-col items-center justify-center h-full gap-3 text-slate-500">
        <p class="text-lg">No sessions found</p>
        <p class="text-sm">Start a Codex session or check your config roots.</p>
      </div>
    {:else if displayed.length === 0}
      <div class="flex flex-col items-center justify-center h-full gap-3 text-slate-500">
        <p class="text-lg">No sessions match the current filters.</p>
        <button
          onclick={() => { filters = defaultFilters(); }}
          class="text-sm text-blue-400 hover:text-blue-300 underline underline-offset-2 transition-colors"
        >
          Clear filters
        </button>
      </div>
    {:else}
      <table class="w-full text-sm text-left border-collapse">
        <thead class="sticky top-0 bg-slate-800 z-10">
          <tr class="border-b border-slate-700">
            <!-- Sortable: Name — aria-sort belongs on <th> (columnheader role) -->
            <th class="px-3 py-2 font-medium text-slate-300 whitespace-nowrap" aria-sort={ariaSortAttr('name')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('name')}>
                Name{caretFor('name')}
              </button>
            </th>

            <!-- ID: not sortable -->
            <th class="px-3 py-2 font-medium text-slate-300 whitespace-nowrap">ID</th>

            <!-- Sortable: Started -->
            <th class="px-3 py-2 font-medium text-slate-300 whitespace-nowrap" aria-sort={ariaSortAttr('started')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('started')}>
                Started{caretFor('started')}
              </button>
            </th>

            <!-- Sortable: Model -->
            <th class="px-3 py-2 font-medium text-slate-300 whitespace-nowrap" aria-sort={ariaSortAttr('model')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('model')}>
                Model{caretFor('model')}
              </button>
            </th>

            <!-- Sortable: Input -->
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap" aria-sort={ariaSortAttr('input')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('input')}>
                Input{caretFor('input')}
              </button>
            </th>

            <!-- Sortable: Cached -->
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap" aria-sort={ariaSortAttr('cached')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('cached')}>
                Cached{caretFor('cached')}
              </button>
            </th>

            <!-- Sortable: Output -->
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap" aria-sort={ariaSortAttr('output')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('output')}>
                Output{caretFor('output')}
              </button>
            </th>

            <!-- Sortable: Reasoning -->
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap" aria-sort={ariaSortAttr('reasoning')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('reasoning')}>
                Reasoning{caretFor('reasoning')}
              </button>
            </th>

            <!-- Sortable: Total -->
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap" aria-sort={ariaSortAttr('total')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('total')}>
                Total{caretFor('total')}
              </button>
            </th>
          </tr>
        </thead>
        <tbody>
          {#each displayed as session (session.id)}
            {@const name = sessionName(session)}
            <!-- svelte-ignore a11y_interactive_supports_focus -->
            <tr
              role="button"
              class="border-b border-slate-700/50 hover:bg-slate-700/40 transition-colors cursor-pointer
                     {isPulsing(session.lastUpdatedAt) ? 'bg-blue-900/20 animate-pulse' : ''}
                     {openSessionId === session.id ? 'bg-slate-700/60' : ''}"
              onclick={() => openDrawer(session.id)}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') openDrawer(session.id); }}
              tabindex="0"
              aria-label="Open session {name}"
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

<!-- Detail drawer (rendered outside the scrollable area so it overlays correctly) -->
<RowDrawer session={openSession} onclose={closeDrawer} />
