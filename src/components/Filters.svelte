<script lang="ts">
  import type { Session } from '../lib/types';

  export type FilterState = {
    search: string;
    dateFrom: string;
    dateTo: string;
    model: string;
    showActive: boolean;
    showArchived: boolean;
  };

  interface Props {
    filters: FilterState;
    sessions: Session[];
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
  });

  // Debounce timer for the search input.
  let searchTimer: ReturnType<typeof setTimeout> | null = null;
  // Initialise to '' and let the effect below immediately set the real value.
  let localSearch = $state('');

  // Keep localSearch in sync whenever the parent-provided filters.search changes
  // (e.g. when "Clear filters" resets the state from the parent).
  $effect(() => {
    // Reading filters.search inside the effect body creates a reactive dependency.
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

  // Collect distinct model values reactively from the full session list.
  const distinctModels = $derived(
    [...new Set(sessions.map((s) => s.model).filter((m): m is string => m !== null))].sort(),
  );

  const isDefault = $derived(
    filters.search === '' &&
      filters.dateFrom === '' &&
      filters.dateTo === '' &&
      filters.model === '' &&
      filters.showActive &&
      filters.showArchived,
  );
</script>

<div class="flex flex-wrap items-center gap-3 px-4 py-2 bg-slate-800/80 border-b border-slate-700 flex-shrink-0">
  <!-- Search -->
  <div class="relative flex-1 min-w-[160px] max-w-xs">
    <svg
      class="absolute left-2.5 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-slate-400 pointer-events-none"
      fill="none"
      stroke="currentColor"
      viewBox="0 0 24 24"
      aria-hidden="true"
    >
      <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M21 21l-4.35-4.35M17 11A6 6 0 1 1 5 11a6 6 0 0 1 12 0z" />
    </svg>
    <input
      type="search"
      placeholder="Search…"
      value={localSearch}
      oninput={handleSearchInput}
      class="w-full pl-8 pr-3 py-1.5 text-sm bg-slate-700 border border-slate-600 rounded-md text-slate-100 placeholder-slate-400 focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500"
      aria-label="Search sessions"
    />
  </div>

  <!-- Date from -->
  <label class="flex items-center gap-1.5 text-xs text-slate-400 whitespace-nowrap">
    <span>From</span>
    <input
      type="date"
      value={filters.dateFrom}
      onchange={(e) => emit({ dateFrom: (e.target as HTMLInputElement).value })}
      class="py-1.5 px-2 text-sm bg-slate-700 border border-slate-600 rounded-md text-slate-100 focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 [color-scheme:dark]"
      aria-label="Start date from"
    />
  </label>

  <!-- Date to -->
  <label class="flex items-center gap-1.5 text-xs text-slate-400 whitespace-nowrap">
    <span>To</span>
    <input
      type="date"
      value={filters.dateTo}
      onchange={(e) => emit({ dateTo: (e.target as HTMLInputElement).value })}
      class="py-1.5 px-2 text-sm bg-slate-700 border border-slate-600 rounded-md text-slate-100 focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500 [color-scheme:dark]"
      aria-label="Start date to"
    />
  </label>

  <!-- Model select -->
  <select
    value={filters.model}
    onchange={(e) => emit({ model: (e.target as HTMLSelectElement).value })}
    class="py-1.5 px-2 text-sm bg-slate-700 border border-slate-600 rounded-md text-slate-100 focus:outline-none focus:ring-1 focus:ring-blue-500 focus:border-blue-500"
    aria-label="Filter by model"
  >
    <option value="">All models</option>
    {#each distinctModels as m}
      <option value={m}>{m}</option>
    {/each}
  </select>

  <!-- Status toggles -->
  <fieldset class="flex items-center gap-3 text-sm text-slate-300">
    <legend class="sr-only">Session status filter</legend>

    <label class="flex items-center gap-1.5 cursor-pointer select-none">
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
        class="accent-blue-500"
        aria-label="Show active sessions"
      />
      Active
    </label>

    <label class="flex items-center gap-1.5 cursor-pointer select-none">
      <input
        type="checkbox"
        checked={filters.showArchived}
        onchange={(e) => {
          const next = (e.target as HTMLInputElement).checked;
          if (!next && !filters.showActive) return;
          emit({ showArchived: next });
        }}
        disabled={filters.showArchived && !filters.showActive}
        class="accent-blue-500"
        aria-label="Show archived sessions"
      />
      Archived
    </label>
  </fieldset>

  <!-- Clear button — only visible when filters differ from defaults -->
  {#if !isDefault}
    <button
      onclick={clearAll}
      class="ml-auto flex-shrink-0 text-xs px-2.5 py-1.5 rounded-md bg-slate-600 hover:bg-slate-500 text-slate-200 transition-colors"
      aria-label="Clear all filters"
    >
      Clear filters
    </button>
  {/if}
</div>
