<script lang="ts">
  import { SESSION_GRID_COLUMNS, sessionGridStore } from '../lib/stores/sessionGrid.svelte';

  const controlColumns = $derived([
    ...sessionGridStore.columns,
    ...SESSION_GRID_COLUMNS.filter((column) => !sessionGridStore.columnIds.includes(column.id)),
  ]);
</script>

<div class="px-4 py-2 border-t border-edge flex items-center gap-3 text-xs bg-panel">
  <label class="flex items-center gap-1.5 text-ink-muted">
    <input
      type="checkbox"
      checked={sessionGridStore.groupByRepository}
      onchange={(event) => sessionGridStore.setGroupByRepository(event.currentTarget.checked)}
    />
    Group by repository
  </label>
  <label class="flex items-center gap-1.5 text-ink-muted">
    <input
      type="checkbox"
      checked={sessionGridStore.colorByModelProvider}
      onchange={(event) => sessionGridStore.setColorByModelProvider(event.currentTarget.checked)}
    />
    Color by model provider
  </label>
  <label
    class="flex items-center gap-1.5 text-ink-muted"
    title="Rank subagent runs against every other thread instead of nesting them under their parent"
  >
    <input
      type="checkbox"
      checked={sessionGridStore.flattenSubagents}
      onchange={(event) => sessionGridStore.setFlattenSubagents(event.currentTarget.checked)}
    />
    Flat list
  </label>
  <details class="relative">
    <summary class="cursor-pointer rounded-md border border-edge bg-card px-2 py-1 text-ink-muted hover:text-ink">Columns</summary>
    <div class="absolute z-20 right-0 mt-1 w-64 rounded-lg border border-edge bg-card shadow-lg p-2 space-y-1">
      <div class="flex items-center justify-between pb-1 border-b border-edgerow">
        <span class="section-label">Session grid</span>
        <button class="text-ink-faint hover:text-ink" onclick={() => sessionGridStore.reset()} title="Reset grid columns to defaults" aria-label="Reset grid columns to defaults">↺ Reset</button>
      </div>
      {#each controlColumns as column (column.id)}
        {@const visible = sessionGridStore.columnIds.includes(column.id)}
        <div class="flex items-center gap-1.5 py-0.5" data-column-id={column.id}>
          <input
            id={`column-${column.id}`}
            type="checkbox"
            checked={visible}
            disabled={column.id === 'name'}
            onchange={(event) => sessionGridStore.setVisible(column.id, event.currentTarget.checked)}
          />
          <label class="flex-1 text-ink-muted" for={`column-${column.id}`}>{column.label}</label>
          {#if visible}
            {@const index = sessionGridStore.columnIds.indexOf(column.id)}
            <button class="px-1 text-ink-faint hover:text-ink disabled:opacity-30" disabled={index === 0} onclick={() => sessionGridStore.move(column.id, -1)} aria-label={`Move ${column.label} left`}>←</button>
            <button class="px-1 text-ink-faint hover:text-ink disabled:opacity-30" disabled={index === sessionGridStore.columnIds.length - 1} onclick={() => sessionGridStore.move(column.id, 1)} aria-label={`Move ${column.label} right`}>→</button>
          {/if}
        </div>
      {/each}
      <p class="pt-1 text-[10px] text-ink-faint">Shown repository names use only the recorded project-folder label; full paths stay out of the grid.</p>
    </div>
  </details>
</div>
