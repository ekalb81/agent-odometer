<script lang="ts">
  import { sessionsStore } from '../lib/stores/sessions.svelte';
  import { rates } from '../lib/stores/rates';
  import { computeSummaryCredits, creditsFromBuckets, formatCredits, harnessCurrency } from '../lib/credits';
  import { getSessionDetails, sessionsInRange } from '../lib/ipc';
  import type { Harness, RangeTotals, Session, TokenTotals } from '../lib/types';
  import Filters from './Filters.svelte';
  import type { FilterState } from './Filters.svelte';
  import RowDrawer from './RowDrawer.svelte';

  let { harness = 'codex' as Harness }: { harness?: Harness } = $props();

  const fmt = new Intl.NumberFormat();
  const fmtCredit = (amount: number) =>
    formatCredits(amount, $rates ? harnessCurrency($rates, harness) : 'credits');
  // Codex bills in plan credits; other harnesses show an API-equivalent cost.
  const costNoun = $derived(harness === 'codex' ? 'credits' : 'est. cost');

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
    showSubagents: true,
  });

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

  // ---------------------------------------------------------------------------
  // Sort state
  // ---------------------------------------------------------------------------
  type SortKey = 'name' | 'started' | 'model' | 'input' | 'cached' | 'output' | 'reasoning' | 'total' | 'credit' | null;
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
  const allSessions = $derived(
    [...sessionsStore.map.values()].filter((s) => s.harness === harness),
  );

  // Convert datetime-local strings (local time) to UTC ISO once, so the rest
  // of the pipeline can do lexical comparison against the UTC ISO timestamps
  // we store on sessions and history points.
  function toUtcIso(local: string): string | null {
    if (!local) return null;
    const d = new Date(local);
    return isNaN(d.getTime()) ? null : d.toISOString();
  }
  const fromUtc = $derived(toUtcIso(filters.dateFrom));
  const toUtc = $derived(toUtcIso(filters.dateTo));

  const filtered = $derived((() => {
    const lc = filters.search.toLowerCase();
    return allSessions.filter((s) => {
      // Status filter.
      if (s.archived && !filters.showArchived) return false;
      if (!s.archived && !filters.showActive) return false;
      if (!filters.showSubagents && (s.parent_thread_id || s.agent_path || s.source === 'subagent')) {
        return false;
      }

      // Datetime range — overlap semantics: include any session whose
      // [started_at, last_event_at] window intersects the filter range.
      // Comparison is lexical on UTC ISO strings, which sorts chronologically.
      if (fromUtc && s.last_event_at < fromUtc) return false;
      if (toUtc && s.started_at > toUtc) return false;

      // Model filter.
      if (filters.model && s.model !== filters.model) return false;

      // Free-text search.
      if (lc) {
        const haystack = [
          s.thread_name ?? '',
          s.id,
          s.first_user_message ?? '',
          s.working_directory ?? '',
          s.agent_path ?? '',
          s.agent_nickname ?? '',
        ].join('\0').toLowerCase();
        if (!haystack.includes(lc)) return false;
      }

      return true;
    });
  })());

  // True when the user has narrowed by date — drives whether per-session
  // tokens and credits are "all-time" or scoped to the visible window.
  const dateScoped = $derived(Boolean(filters.dateFrom || filters.dateTo));

  // ---------------------------------------------------------------------------
  // Date-scoped rollups come from the backend (summaries don't carry event
  // histories). Refetched, debounced, when the range or the session set changes.
  // ---------------------------------------------------------------------------
  let rangeTotals = $state<Record<string, RangeTotals>>({});
  let rangeFetchTimer: ReturnType<typeof setTimeout> | null = null;

  $effect(() => {
    const from = fromUtc;
    const to = toUtc;
    // Depend on the session map so live session updates refresh range data.
    void sessionsStore.map;
    if (!from && !to) {
      rangeTotals = {};
      return;
    }
    if (rangeFetchTimer !== null) clearTimeout(rangeFetchTimer);
    rangeFetchTimer = setTimeout(() => {
      sessionsInRange(from, to)
        .then((result) => {
          // Drop stale responses if the filter moved on meanwhile.
          if (fromUtc === from && toUtc === to) rangeTotals = result;
        })
        .catch((e) => console.error('sessions_in_range failed:', e));
    }, 250);
    return () => {
      if (rangeFetchTimer !== null) clearTimeout(rangeFetchTimer);
    };
  });

  function zeroTotals(): TokenTotals {
    return {
      input_tokens: 0,
      cached_input_tokens: 0,
      output_tokens: 0,
      reasoning_output_tokens: 0,
      total_tokens: 0,
    };
  }

  // Per-session display values: tokens AND credits, both scoped to the date
  // filter when one is active so the row numbers add up to the header.
  const sessionDisplayMap = $derived((() => {
    const r = $rates;
    const out = new Map<
      string,
      { tokens: TokenTotals; total: number; refCost: number; missingModels: string[] }
    >();
    for (const s of filtered) {
      const rt = dateScoped ? rangeTotals[s.id] : undefined;
      const tokens = dateScoped ? (rt?.tokens ?? zeroTotals()) : s.tokens_total;
      if (!r) {
        out.set(s.id, { tokens, total: 0, refCost: 0, missingModels: [] });
        continue;
      }
      // Reference cost (à-la-carte equivalent) is always all-time for the
      // unlimited-plan tooltip — that's the figure people compare against.
      const allTime = computeSummaryCredits(s, r);
      const credits = dateScoped ? creditsFromBuckets(rt?.buckets ?? [], r, s.harness) : allTime;
      out.set(s.id, {
        tokens,
        total: credits.total,
        refCost: allTime.total,
        missingModels: credits.missingModels,
      });
    }
    return out;
  })());

  function compareSession(a: (typeof allSessions)[number], b: (typeof allSessions)[number]): number {
    if (sortKey === null) {
      // Default: last_event_at desc.
      return new Date(b.last_event_at).getTime() - new Date(a.last_event_at).getTime();
    }
    const aTokens = sessionDisplayMap.get(a.id)?.tokens ?? a.tokens_total;
    const bTokens = sessionDisplayMap.get(b.id)?.tokens ?? b.tokens_total;
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
        cmp = aTokens.input_tokens - bTokens.input_tokens;
        break;
      case 'cached':
        cmp = aTokens.cached_input_tokens - bTokens.cached_input_tokens;
        break;
      case 'output':
        cmp = aTokens.output_tokens - bTokens.output_tokens;
        break;
      case 'reasoning':
        cmp = aTokens.reasoning_output_tokens - bTokens.reasoning_output_tokens;
        break;
      case 'total':
        cmp = aTokens.total_tokens - bTokens.total_tokens;
        break;
      case 'credit': {
        const aCredits = sessionDisplayMap.get(a.id)?.total ?? 0;
        const bCredits = sessionDisplayMap.get(b.id)?.total ?? 0;
        cmp = aCredits - bCredits;
        break;
      }
    }
    return sortDir === 'asc' ? cmp : -cmp;
  }

  const displayed = $derived([...filtered].sort(compareSession));

  const filteredTotal = $derived(
    filtered.reduce((sum, s) => sum + (sessionDisplayMap.get(s.id)?.tokens.total_tokens ?? 0), 0),
  );

  // Credit totals: only count sessions that are NOT on unlimited plans.
  const creditSummary = $derived((() => {
    let billedTotal = 0;
    let unlimitedCount = 0;
    for (const s of filtered) {
      const c = sessionDisplayMap.get(s.id);
      if (!c) continue;
      if (s.credits_unlimited === true) {
        unlimitedCount++;
      } else {
        billedTotal += c.total;
      }
    }
    return { billedTotal, unlimitedCount };
  })());

  // ---------------------------------------------------------------------------
  // Drawer state — the table only holds summaries, so the full session
  // (turns, token history) is fetched on open and refreshed when the
  // session's summary is upserted by a live event.
  // ---------------------------------------------------------------------------
  let openSessionId = $state<string | null>(null);
  let openSession = $state<Session | null>(null);

  $effect(() => {
    const id = openSessionId;
    // Reactive dep: refetch details when this session's summary updates.
    void (id !== null ? sessionsStore.map.get(id)?.lastUpdatedAt : undefined);
    if (id === null) {
      openSession = null;
      return;
    }
    let cancelled = false;
    getSessionDetails(id)
      .then((s) => {
        if (!cancelled && openSessionId === id) openSession = s;
      })
      .catch((e) => console.error('get_session_details failed:', e));
    return () => {
      cancelled = true;
    };
  });

  function openDrawer(id: string) {
    openSessionId = id;
  }

  function closeDrawer() {
    openSessionId = null;
    openSession = null;
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
  <div class="flex flex-wrap items-center gap-x-6 gap-y-1 px-4 py-2 bg-slate-800 border-b border-slate-700 flex-shrink-0 text-sm text-slate-400">
    <span>
      Showing
      <span class="font-semibold text-slate-200">{displayed.length}</span>
      of
      <span class="font-semibold text-slate-200">{allSessions.length}</span>
      {allSessions.length === 1 ? 'task' : 'tasks'}
    </span>
    <span>
      <span class="font-semibold text-slate-200">{fmtTokens(filteredTotal)}</span>
      {dateScoped ? 'tokens in range' : 'total tokens'}
    </span>
    <span>
      <span class="font-semibold text-slate-200">{fmtCredit(creditSummary.billedTotal)}</span>
      {dateScoped ? `${costNoun} in range` : `total ${costNoun}`}
      {#if creditSummary.unlimitedCount > 0}
        <span class="text-slate-500 text-xs ml-1">({creditSummary.unlimitedCount} unlimited excluded)</span>
      {/if}
    </span>
    {#if dateScoped}
      <span class="text-xs text-slate-500">
        Token & credit columns scoped to
        {filters.dateFrom || '…'} – {filters.dateTo || '…'}
      </span>
    {/if}
  </div>

  <!-- Table -->
  <div class="flex-1 overflow-auto">
    {#if allSessions.length === 0}
      <div class="flex flex-col items-center justify-center h-full gap-3 text-slate-500">
        <p class="text-lg">No tasks found</p>
        {#if harness === 'claude_code'}
          <p class="text-sm">Start a Claude Code session or check your Claude session roots in Settings.</p>
        {:else}
          <p class="text-sm">Start a Codex task in ChatGPT or check your config roots.</p>
        {/if}
      </div>
    {:else if displayed.length === 0}
      <div class="flex flex-col items-center justify-center h-full gap-3 text-slate-500">
        <p class="text-lg">No tasks match the current filters.</p>
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
            <!-- Sortable: Name -->
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

            <!-- Sortable: Credit -->
            <th class="px-3 py-2 font-medium text-slate-300 text-right whitespace-nowrap" aria-sort={ariaSortAttr('credit')}>
              <button class="hover:text-white transition-colors" onclick={() => toggleSort('credit')}>
                {harness === 'codex' ? 'Credit' : 'Cost'}{caretFor('credit')}
              </button>
            </th>
          </tr>
        </thead>
        <tbody>
          {#each displayed as session (session.id)}
            {@const name = sessionName(session)}
            {@const sessionCredit = sessionDisplayMap.get(session.id)}
            {@const rowTokens = sessionCredit?.tokens ?? session.tokens_total}
            <!-- svelte-ignore a11y_interactive_supports_focus -->
            <tr
              role="button"
              class="border-b border-slate-700/50 hover:bg-slate-700/40 transition-colors cursor-pointer
                     {isPulsing(session.lastUpdatedAt) ? 'bg-blue-900/20 animate-pulse' : ''}
                     {openSessionId === session.id ? 'bg-slate-700/60' : ''}"
              onclick={() => openDrawer(session.id)}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') openDrawer(session.id); }}
              tabindex="0"
              aria-label="Open task {name}"
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
                  {#if session.parent_thread_id || session.agent_path || session.source === 'subagent'}
                    <span class="flex-shrink-0 text-xs px-1.5 py-0.5 rounded bg-violet-900/60 text-violet-300">
                      subagent
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

              <!-- Token columns (scoped to date range when set, else all-time) -->
              <td class="px-3 py-2 text-right tabular-nums text-slate-300">
                {fmtTokens(rowTokens.input_tokens)}
              </td>
              <td class="px-3 py-2 text-right tabular-nums text-slate-300">
                {fmtTokens(rowTokens.cached_input_tokens)}
              </td>
              <td class="px-3 py-2 text-right tabular-nums text-slate-300">
                {fmtTokens(rowTokens.output_tokens)}
              </td>
              <td class="px-3 py-2 text-right tabular-nums text-slate-300">
                {fmtTokens(rowTokens.reasoning_output_tokens)}
              </td>
              <td class="px-3 py-2 text-right tabular-nums font-medium text-slate-100">
                {fmtTokens(rowTokens.total_tokens)}
              </td>

              <!-- Credit column -->
              <td class="px-3 py-2 text-right tabular-nums whitespace-nowrap">
                {#if session.credits_unlimited === true}
                  <span
                    class="text-slate-400"
                    title="Reference cost on à-la-carte: {fmtCredit(sessionCredit?.refCost ?? 0)} ({session.plan_type ?? 'unlimited'} · unlimited)"
                  >—</span>
                {:else}
                  <span class="text-emerald-400 font-medium">
                    {fmtCredit(sessionCredit?.total ?? 0)}
                  </span>
                {/if}
                {#if sessionCredit && sessionCredit.missingModels.length > 0}
                  <span
                    class="ml-1 text-amber-400 cursor-help"
                    title="Fallback rate used for: {sessionCredit.missingModels.join(', ')}"
                    aria-label="Warning: fallback rate used"
                  >⚠</span>
                {/if}
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
