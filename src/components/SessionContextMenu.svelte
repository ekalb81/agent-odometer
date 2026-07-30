<script lang="ts">
  import { onMount } from 'svelte';
  import type { SessionExportFormat } from '../lib/sessionExport';

  interface Props {
    sessionName: string;
    x: number;
    y: number;
    descendantCount: number;
    includeDescendants: boolean;
    busy: boolean;
    error: string | null;
    onincludechange: (include: boolean) => void;
    onexport: (format: SessionExportFormat) => void;
    onclose: () => void;
  }

  let {
    sessionName,
    x,
    y,
    descendantCount,
    includeDescendants,
    busy,
    error,
    onincludechange,
    onexport,
    onclose,
  }: Props = $props();

  let menu: HTMLDivElement;
  onMount(() => menu.focus());
</script>

<div
  class="fixed inset-0 z-40"
  data-testid="session-context-backdrop"
  role="presentation"
  onclick={onclose}
  oncontextmenu={(event) => { event.preventDefault(); onclose(); }}
>
  <div
    bind:this={menu}
    class="fixed z-50 w-60 rounded-lg border border-edge bg-card p-1.5 shadow-2xl text-xs text-ink"
    style:left={`${x}px`}
    style:top={`${y}px`}
    role="menu"
    aria-label={`Export ${sessionName}`}
    tabindex="-1"
    onclick={(event) => event.stopPropagation()}
    onkeydown={(event) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        event.stopPropagation();
        onclose();
      }
    }}
  >
    <div class="truncate px-2 py-1.5 font-semibold" title={sessionName}>{sessionName}</div>
    {#if descendantCount > 0}
      <button
        class="flex w-full items-center gap-2 rounded-sm px-2 py-1.5 text-left hover:bg-(--row-hover) disabled:opacity-50"
        role="menuitemcheckbox"
        aria-checked={includeDescendants}
        disabled={busy}
        onclick={() => onincludechange(!includeDescendants)}
      >
        <span class="w-3 text-center" aria-hidden="true">{includeDescendants ? '✓' : ''}</span>
        Include {descendantCount} child {descendantCount === 1 ? 'session' : 'sessions'}
      </button>
      <div class="my-1 border-t border-edge"></div>
    {/if}
    <button
      class="w-full rounded-sm px-2 py-1.5 text-left hover:bg-(--row-hover) disabled:opacity-50"
      role="menuitem"
      disabled={busy}
      onclick={() => onexport('json')}
    >Export as JSON</button>
    <button
      class="w-full rounded-sm px-2 py-1.5 text-left hover:bg-(--row-hover) disabled:opacity-50"
      role="menuitem"
      disabled={busy}
      onclick={() => onexport('csv')}
    >Export as CSV</button>
    {#if busy}<div class="px-2 py-1.5 text-ink-muted">Preparing export…</div>{/if}
    {#if error}<div class="px-2 py-1.5 text-neg" role="alert">{error}</div>{/if}
  </div>
</div>
