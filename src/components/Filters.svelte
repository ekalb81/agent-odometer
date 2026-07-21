<script lang="ts">
  import { onDestroy } from 'svelte';
  import type { SessionSummary } from '../lib/types';

  export type FilterState = {
    search: string;
    dateFrom: string;
    dateTo: string;
    model: string;
    showActive: boolean;
    showArchived: boolean;
    showSubagents: boolean;
  };

  interface Props {
    filters: FilterState;
    sessions: SessionSummary[];
    onchange: (f: FilterState) => void;
  }

  let { filters, sessions, onchange }: Props = $props();

  const defaultFilters = (): FilterState => ({
    search: '',
    dateFrom: '',
    dateTo: '',
    model: '',
    showActive: true,
    showArchived: true,
    showSubagents: true,
  });

  // Debounce timer for the search input.
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  // Initialise to '' and let the effect below immediately set the real value.
  let localSearch = $state('');

  // Keep localSearch in sync whenever the parent-provided filters.search changes
  // (e.g. when "Clear filters" resets the state from the parent).
  $effect(() => {
    localSearch = filters.search;
  });

  function handleSearchInput(e: Event) {
    const value = (e.target as HTMLInputElement).value;
    localSearch = value;
    if (searchTimer !== null) clearTimeout(searchTimer);
    searchTimer = setTimeout(() => {
      emit({ search: value });
    }, 150);
  }

  function emit(patch: Partial<FilterState>) {
    onchange({ ...filters, ...patch });
  }

  function clearAll() {
    localSearch = '';
    onchange(defaultFilters());
  }

  onDestroy(() => {
    if (searchTimer !== null) clearTimeout(searchTimer);
  });

  // Collect distinct model values reactively from the full session list.
  const distinctModels = $derived(
    [...new Set(sessions.map((s) => s.model).filter((m): m is string => m !== null))].sort(),
  );

  // ---------------------------------------------------------------------------
  // Date-range presets — stored as `datetime-local` strings (local time).
  // Empty bound = open-ended.
  // ---------------------------------------------------------------------------
  function pad(n: number): string { return n.toString().padStart(2, '0'); }
  function toLocalInputValue(d: Date): string {
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
  }
  function startOfToday(): Date {
    const d = new Date();
    d.setHours(0, 0, 0, 0);
    return d;
  }

  type Preset = { label: string; from: () => Date | null };
  const presets: Preset[] = [
    { label: 'All time', from: () => null },
    { label: 'Today', from: startOfToday },
    { label: 'Last 24h', from: () => new Date(Date.now() - 24 * 3600 * 1000) },
    { label: 'Last 7 days', from: () => new Date(Date.now() - 7 * 24 * 3600 * 1000) },
    { label: 'Last 30 days', from: () => new Date(Date.now() - 30 * 24 * 3600 * 1000) },
  ];

  function applyPreset(p: Preset) {
    const from = p.from();
    emit({ dateFrom: from ? toLocalInputValue(from) : '', dateTo: '' });
    rangeOpen = false;
  }

  // The pill label: recognise the preset that produced the current bounds
  // (within tolerance — presets are relative to "now"), else show the range.
  const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  function fmtBound(local: string): string {
    const d = new Date(local);
    if (isNaN(d.getTime())) return '…';
    return `${MONTHS[d.getMonth()]} ${d.getDate()}`;
  }
  const rangeLabel = $derived((() => {
    if (!filters.dateFrom && !filters.dateTo) return 'All time';
    if (filters.dateFrom && !filters.dateTo) {
      const fromMs = new Date(filters.dateFrom).getTime();
      const tolerance = 90 * 1000; // presets round to the minute; allow drift
      for (const p of presets) {
        const d = p.from();
        if (d && Math.abs(fromMs - d.getTime()) < tolerance) return p.label;
      }
      return `${fmtBound(filters.dateFrom)} – now`;
    }
    return `${filters.dateFrom ? fmtBound(filters.dateFrom) : '…'} – ${filters.dateTo ? fmtBound(filters.dateTo) : '…'}`;
  })());

  const dateScoped = $derived(Boolean(filters.dateFrom || filters.dateTo));

  // Count of active non-default filters behind the "Filters" pill.
  const filterCount = $derived(
    (filters.model ? 1 : 0) +
      (filters.showActive && filters.showArchived ? 0 : 1) +
      (filters.showSubagents ? 0 : 1),
  );

  const isDefault = $derived(
    filters.search === '' && !dateScoped && filterCount === 0,
  );

  // ---------------------------------------------------------------------------
  // Popover state + click-outside handling
  // ---------------------------------------------------------------------------
  let rangeOpen = $state(false);
  let filtersOpen = $state(false);
  let rangeEl = $state<HTMLElement | null>(null);
  let filtersEl = $state<HTMLElement | null>(null);

  function onWindowPointerDown(e: PointerEvent) {
    const t = e.target as Node;
    if (rangeOpen && rangeEl && !rangeEl.contains(t)) rangeOpen = false;
    if (filtersOpen && filtersEl && !filtersEl.contains(t)) filtersOpen = false;
  }

  function onPopoverKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      rangeOpen = false;
      filtersOpen = false;
      e.stopPropagation();
    }
  }

  $effect(() => {
    if (rangeOpen || filtersOpen) {
      window.addEventListener('pointerdown', onWindowPointerDown);
      return () => window.removeEventListener('pointerdown', onWindowPointerDown);
    }
  });

  const pillClass =
    'flex items-center gap-1.5 bg-app border border-edge rounded-full px-3.5 py-1.5 text-xs text-ink-2 hover:text-ink transition-colors whitespace-nowrap';
</script>

<div class="flex items-center gap-2">
  <!-- Search -->
  <div class="relative">
    <svg
      class="absolute left-3 top-1/2 -translate-y-1/2 w-3 h-3 text-ink-faint pointer-events-none"
      fill="none"
      stroke="currentColor"
      viewBox="0 0 24 24"
      aria-hidden="true"
    >
      <path stroke-linecap="round" stroke-width="2" d="M21 21l-4.35-4.35M17 11A6 6 0 1 1 5 11a6 6 0 0 1 12 0z" />
    </svg>
    <input
      type="search"
      placeholder="Search sessions"
      value={localSearch}
      oninput={handleSearchInput}
      class="w-[220px] pl-8 pr-3 py-1.5 text-xs bg-app border border-edge rounded-lg text-ink placeholder-ink-faint focus:outline-none focus:ring-1 focus:ring-accent focus:border-accent"
      aria-label="Search sessions"
    />
  </div>

  <!-- Date-range pill -->
  <div class="relative" bind:this={rangeEl}>
    <button
      type="button"
      class="{pillClass} {dateScoped ? 'font-medium text-ink' : 'font-medium'}"
      onclick={() => { rangeOpen = !rangeOpen; filtersOpen = false; }}
      aria-haspopup="true"
      aria-expanded={rangeOpen}
    >
      {rangeLabel} <span class="text-ink-faint">▾</span>
    </button>
    {#if rangeOpen}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="absolute right-0 top-full mt-2 w-64 bg-card border border-edge rounded-xl shadow-xl z-50 p-3 flex flex-col gap-3"
        onkeydown={onPopoverKeydown}
      >
        <div class="flex flex-wrap gap-1.5">
          {#each presets as p}
            <button
              type="button"
              onclick={() => applyPreset(p)}
              class="text-xs px-2.5 py-1 rounded-full border transition-colors
                     {rangeLabel === p.label
                       ? 'bg-accent-tab border-transparent text-white font-semibold'
                       : 'bg-app border-edge text-ink-2 hover:text-ink'}"
            >{p.label}</button>
          {/each}
        </div>
        <div class="border-t border-edge pt-3 flex flex-col gap-2">
          <label class="flex items-center justify-between gap-2 text-xs text-ink-muted">
            <span>From</span>
            <input
              type="datetime-local"
              value={filters.dateFrom}
              onchange={(e) => emit({ dateFrom: (e.target as HTMLInputElement).value })}
              class="py-1 px-2 text-xs bg-app border border-edge rounded-lg text-ink focus:outline-none focus:ring-1 focus:ring-accent"
              aria-label="Start datetime from"
            />
          </label>
          <label class="flex items-center justify-between gap-2 text-xs text-ink-muted">
            <span>To</span>
            <input
              type="datetime-local"
              value={filters.dateTo}
              onchange={(e) => emit({ dateTo: (e.target as HTMLInputElement).value })}
              class="py-1 px-2 text-xs bg-app border border-edge rounded-lg text-ink focus:outline-none focus:ring-1 focus:ring-accent"
              aria-label="Start datetime to"
            />
          </label>
        </div>
      </div>
    {/if}
  </div>

  <!-- Filters pill -->
  <div class="relative" bind:this={filtersEl}>
    <button
      type="button"
      class="{pillClass} {filterCount > 0 ? 'text-ink font-medium' : ''}"
      onclick={() => { filtersOpen = !filtersOpen; rangeOpen = false; }}
      aria-haspopup="true"
      aria-expanded={filtersOpen}
    >
      Filters{#if filterCount > 0}<span class="text-ink-faint">·</span>{filterCount}{/if}
    </button>
    {#if filtersOpen}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="absolute right-0 top-full mt-2 w-60 bg-card border border-edge rounded-xl shadow-xl z-50 p-3 flex flex-col gap-3"
        onkeydown={onPopoverKeydown}
      >
        <label class="flex flex-col gap-1 text-xs text-ink-muted">
          <span class="section-label">Model</span>
          <select
            value={filters.model}
            onchange={(e) => emit({ model: (e.target as HTMLSelectElement).value })}
            class="py-1.5 px-2 text-xs bg-app border border-edge rounded-lg text-ink font-mono focus:outline-none focus:ring-1 focus:ring-accent"
            aria-label="Filter by model"
          >
            <option value="">All models</option>
            {#each distinctModels as m}
              <option value={m}>{m}</option>
            {/each}
          </select>
        </label>

        <fieldset class="flex flex-col gap-1.5 text-xs text-ink-2">
          <legend class="section-label mb-1">Status</legend>
          <label class="flex items-center gap-2 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={filters.showActive}
              onchange={(e) => {
                const next = (e.target as HTMLInputElement).checked;
                // Must keep at least one status on.
                if (!next && !filters.showArchived) return;
                emit({ showActive: next });
              }}
              disabled={filters.showActive && !filters.showArchived}
              class="accent-[var(--accent)]"
              aria-label="Show active sessions"
            />
            Active
          </label>
          <label class="flex items-center gap-2 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={filters.showArchived}
              onchange={(e) => {
                const next = (e.target as HTMLInputElement).checked;
                if (!next && !filters.showActive) return;
                emit({ showArchived: next });
              }}
              disabled={filters.showArchived && !filters.showActive}
              class="accent-[var(--accent)]"
              aria-label="Show archived sessions"
            />
            Archived
          </label>
          <label class="flex items-center gap-2 cursor-pointer select-none">
            <input
              type="checkbox"
              checked={filters.showSubagents}
              onchange={(e) => emit({ showSubagents: (e.target as HTMLInputElement).checked })}
              class="accent-[var(--accent)]"
              aria-label="Show subagent tasks"
            />
            Subagents
          </label>
        </fieldset>

        {#if !isDefault}
          <button
            onclick={clearAll}
            class="self-start text-xs px-2.5 py-1 rounded-lg bg-app border border-edge text-ink-2 hover:text-ink transition-colors"
            aria-label="Clear all filters"
          >
            Clear all filters
          </button>
        {/if}
      </div>
    {/if}
  </div>
</div>
