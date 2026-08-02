<script lang="ts">
  import { onMount } from 'svelte';
  import { marked } from 'marked';
  import DOMPurify from 'dompurify';
  import ConfigTimeline from './ConfigTimeline.svelte';
  import {
    cancelInstructionScan,
    listExternalEvents,
    listInstructionFiles,
    onConfigEvent,
    onInstructionInventoryUpdated,
    openInstructionFile,
    readInstructionFile,
    revealInFileManager,
  } from '../lib/ipc';
  import { instructionScanStore } from '../lib/stores/instructionScan.svelte';
  import type { ExternalEvent, InstructionFile, InstructionInventory } from '../lib/types';

  interface Props { onhide: () => void | Promise<void>; }
  let { onhide }: Props = $props();

  let inventory = $state<InstructionInventory | null>(null);
  let selectedId = $state<string | null>(null);
  let content = $state<string | null>(null);
  let loading = $state(false);
  let cancelRequested = $state(false);
  let contentLoading = $state(false);
  let error = $state<string | null>(null);
  let contentError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let query = $state('');
  let harnessFilter = $state('all');
  let warningsOnly = $state(false);
  let previewMode = $state<'preview' | 'raw'>('preview');
  let configEvents = $state<ExternalEvent[]>([]);
  let contentGeneration = 0;

  const scanProgress = $derived(instructionScanStore.status);

  const selected = $derived(
    inventory?.files.find((file) => file.id === selectedId) ?? null,
  );

  const harnesses = $derived((() => {
    const values = new Set<string>();
    for (const file of inventory?.files ?? []) {
      for (const harness of file.harnesses) values.add(harness);
    }
    return [...values].sort();
  })());

  const filteredFiles = $derived((() => {
    const needle = query.trim().toLocaleLowerCase();
    return (inventory?.files ?? []).filter((file) => {
      if (harnessFilter !== 'all' && !file.harnesses.includes(harnessFilter)) return false;
      if (warningsOnly && file.warnings.length === 0) return false;
      return !needle || `${file.path} ${file.file_name}`.toLocaleLowerCase().includes(needle);
    });
  })());

  const groups = $derived((() => {
    const grouped = new Map<string, InstructionFile[]>();
    for (const file of filteredFiles) {
      const key = file.project_path ?? `global:${file.root_path}`;
      const group = grouped.get(key) ?? [];
      group.push(file);
      grouped.set(key, group);
    }
    return [...grouped.entries()].map(([key, files]) => ({ key, files }));
  })());

  const effectiveFiles = $derived(
    selected
      ? selected.effective_ids
          .map((id) => inventory?.files.find((file) => file.id === id))
          .filter((file): file is InstructionFile => Boolean(file))
      : [],
  );

  const selectedEvents = $derived(
    selected
      ? configEvents.filter(
          (event) => event.source === 'config' && event.metadata.path_id === selected.path_id,
        )
      : [],
  );

  const renderedMarkdown = $derived((() => {
    if (content === null) return '';
    const html = marked.parse(content, { gfm: true, breaks: false }) as string;
    return DOMPurify.sanitize(html, {
      FORBID_TAGS: [
        'script', 'style', 'iframe', 'object', 'embed', 'form', 'input', 'button',
        'textarea', 'select', 'option', 'img', 'picture', 'video', 'audio', 'source',
        'svg', 'math',
      ],
      FORBID_ATTR: ['style', 'href', 'src', 'srcset', 'formaction', 'xlink:href'],
    });
  })());

  function harnessLabel(harness: string): string {
    if (harness === 'codex') return 'Codex';
    if (harness === 'claude_code') return 'Claude Code';
    return harness;
  }

  function groupLabel(key: string, files: InstructionFile[]): string {
    if (key.startsWith('global:')) return `Global · ${files[0]?.root_path ?? key.slice(7)}`;
    const normalized = key.replaceAll('\\', '/').replace(/\/$/, '');
    return normalized.split('/').at(-1) || key;
  }

  function hierarchyDepth(file: InstructionFile): number {
    let depth = 0;
    let current = file;
    const seen = new Set<string>();
    while (current.parent_id && !seen.has(current.parent_id)) {
      seen.add(current.parent_id);
      const parent = inventory?.files.find((candidate) => candidate.id === current.parent_id);
      if (!parent) break;
      depth += 1;
      current = parent;
    }
    return depth;
  }

  function formatBytes(value: number): string {
    if (value < 1024) return `${value} B`;
    if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KiB`;
    return `${(value / 1024 / 1024).toFixed(1)} MiB`;
  }

  function formatDuration(value: number): string {
    if (value < 1_000) return `${value} ms`;
    if (value < 60_000) return `${(value / 1_000).toFixed(value < 10_000 ? 1 : 0)} s`;
    return `${Math.floor(value / 60_000)}m ${Math.round((value % 60_000) / 1_000)}s`;
  }

  function scanStatusText(): string {
    if (!scanProgress || scanProgress.phase === 'preparing') return 'Preparing project roots…';
    if (scanProgress.phase === 'analyzing') {
      return `Analyzing ${scanProgress.files_found.toLocaleString()} instruction files…`;
    }
    const root = scanProgress.roots_total === 0
      ? 'No scan roots'
      : `Root ${Math.min(scanProgress.roots_done + 1, scanProgress.roots_total)} of ${scanProgress.roots_total}`;
    return `${root} · ${scanProgress.entries_visited.toLocaleString()} entries · ${scanProgress.files_found.toLocaleString()} files · ${formatDuration(scanProgress.elapsed_ms)}`;
  }

  function truncationMessage(inventory: InstructionInventory): string {
    if (inventory.truncation_reason === 'entry_limit') {
      return `Scan stopped after ${inventory.entries_visited.toLocaleString()} filesystem entries. Narrow or split large recursive roots.`;
    }
    return 'Results stopped at the 10,000-instruction-file safety limit.';
  }

  function adoptInventory(next: InstructionInventory) {
    inventory = next;
    if (!selectedId || !next.files.some((file) => file.id === selectedId)) {
      selectedId = next.files[0]?.id ?? null;
    }
  }

  async function refresh() {
    loading = true;
    cancelRequested = false;
    instructionScanStore.clearCurrent();
    error = null;
    try {
      const [nextInventory, events] = await Promise.all([
        listInstructionFiles(),
        listExternalEvents(),
      ]);
      adoptInventory(nextInventory);
      configEvents = events;
    } catch (reason) {
      if (!String(reason).toLocaleLowerCase().includes('instruction scan cancelled')) {
        error = String(reason);
      }
    } finally {
      loading = false;
      cancelRequested = false;
      // A stale answer means a background rescan is still running; its
      // progress keeps flowing to the status bar until the fresh inventory
      // arrives via instruction-inventory-updated.
      if (!inventory?.stale) instructionScanStore.clearCurrent();
    }
  }

  async function cancelScan() {
    cancelRequested = true;
    try {
      const scanId = await cancelInstructionScan();
      instructionScanStore.clearThrough(scanId);
    }
    catch (reason) {
      cancelRequested = false;
      error = String(reason);
    }
  }

  $effect(() => {
    const file = selected;
    const generation = ++contentGeneration;
    content = null;
    contentError = null;
    actionError = null;
    if (!file) return;
    contentLoading = true;
    readInstructionFile(file.path)
      .then((result) => {
        if (generation === contentGeneration) content = result.content;
      })
      .catch((reason) => {
        if (generation === contentGeneration) contentError = String(reason);
      })
      .finally(() => {
        if (generation === contentGeneration) contentLoading = false;
      });
    return () => {
      if (generation === contentGeneration) contentGeneration += 1;
    };
  });

  async function revealSelected() {
    if (!selected) return;
    actionError = null;
    try { await revealInFileManager(selected.path); }
    catch (reason) { actionError = String(reason); }
  }

  async function openSelected() {
    if (!selected) return;
    actionError = null;
    try { await openInstructionFile(selected.path); }
    catch (reason) { actionError = String(reason); }
  }

  async function hideTab() {
    actionError = null;
    try { await onhide(); }
    catch (reason) { actionError = String(reason); }
  }

  onMount(() => {
    void refresh();
    let disposed = false;
    let configUnlisten: (() => void) | undefined;
    let inventoryUnlisten: (() => void) | undefined;
    onConfigEvent((event) => {
      if (!disposed && event.source === 'config') {
        configEvents = [...configEvents.filter((item) => item.id !== event.id), event];
      }
    }).then((dispose) => {
      if (disposed) dispose();
      else configUnlisten = dispose;
    }).catch(() => {});
    onInstructionInventoryUpdated((fresh) => {
      if (disposed) return;
      adoptInventory(fresh);
      error = null;
      instructionScanStore.clearCurrent();
    }).then((dispose) => {
      if (disposed) dispose();
      else inventoryUnlisten = dispose;
    }).catch(() => {});
    return () => {
      disposed = true;
      configUnlisten?.();
      inventoryUnlisten?.();
      if (loading) {
        void cancelInstructionScan()
          .then((scanId) => instructionScanStore.clearThrough(scanId))
          .catch(() => instructionScanStore.clearCurrent());
      } else {
        instructionScanStore.clearCurrent();
      }
    };
  });
</script>

<div class="flex h-full min-h-0 bg-tablebg">
  <aside class="w-[380px] min-w-[300px] max-w-[42vw] flex flex-col border-r border-edge bg-chrome">
    <div class="px-3 py-3 border-b border-edge space-y-2">
      <div class="flex items-center gap-2">
        <div>
          <h1 class="text-sm font-semibold text-ink">Instructions</h1>
          <p class="text-[11px] text-ink-faint">
            {inventory?.files.length ?? 0} files · read-only
          </p>
        </div>
        {#if loading}
          <button
            type="button"
            onclick={() => void cancelScan()}
            disabled={cancelRequested}
            class="ml-auto px-2.5 py-1 text-xs rounded-sm border border-edge bg-card hover:bg-(--row-hover) disabled:opacity-40"
          >{cancelRequested ? 'Cancelling…' : 'Cancel'}</button>
        {:else}
          <button
            type="button"
            onclick={() => void refresh()}
            class="ml-auto px-2.5 py-1 text-xs rounded-sm border border-edge bg-card hover:bg-(--row-hover)"
          >Refresh</button>
        {/if}
        <button
          type="button"
          onclick={() => void hideTab()}
          class="px-2 py-1 text-xs text-ink-muted hover:text-ink"
          title="Hide this tab; discovery remains enabled"
        >Hide tab</button>
      </div>
      {#if loading}
        <p class="text-[11px] text-ink-faint" role="status">{scanStatusText()}</p>
      {:else if inventory?.stale}
        <p class="text-[11px] text-ink-faint" role="status">
          Showing last results · refreshing in background…
        </p>
      {:else if inventory}
        <p class="text-[10px] text-ink-faint">
          Scanned {inventory.entries_visited.toLocaleString()} entries in {formatDuration(inventory.elapsed_ms)}
        </p>
      {/if}
      <input
        type="search"
        bind:value={query}
        placeholder="Filter paths…"
        class="w-full bg-app border border-edge rounded-sm px-2.5 py-1.5 text-xs text-ink placeholder-ink-faint focus:outline-hidden focus:ring-1 focus:ring-(--accent)"
      />
      <div class="flex items-center gap-2">
        <select
          bind:value={harnessFilter}
          class="bg-app border border-edge rounded-sm px-2 py-1 text-xs text-ink"
          aria-label="Filter by harness"
        >
          <option value="all">All harnesses</option>
          {#each harnesses as harness}
            <option value={harness}>{harnessLabel(harness)}</option>
          {/each}
        </select>
        <label class="flex items-center gap-1.5 text-xs text-ink-muted">
          <input type="checkbox" bind:checked={warningsOnly} /> Warnings only
        </label>
      </div>
      {#if inventory?.truncated}
        <p class="text-[11px] text-amber-500">{truncationMessage(inventory)}</p>
      {/if}
      {#if error}<p class="text-xs text-neg" role="alert">{error}</p>{/if}
    </div>

    <div class="flex-1 overflow-y-auto">
      {#if !loading && filteredFiles.length === 0}
        <div class="p-5 text-xs text-ink-faint text-center">
          No matching instruction files. Add a discovery root in Settings or clear the filters.
        </div>
      {/if}
      {#each groups as group (group.key)}
        <section class="border-b border-edge">
          <div class="sticky top-0 z-10 px-3 py-1.5 bg-panel border-b border-edgerow">
            <div class="text-[11px] font-semibold text-ink truncate" title={group.key}>
              {groupLabel(group.key, group.files)}
            </div>
            <div class="text-[10px] font-mono text-ink-faint truncate">{group.key.replace(/^global:/, '')}</div>
          </div>
          {#each group.files as file (file.id)}
            <button
              type="button"
              onclick={() => (selectedId = file.id)}
              class="w-full flex items-start gap-2 pr-3 py-2 text-left border-b border-edgerow hover:bg-(--row-hover) {selectedId === file.id ? 'bg-(--row-selected)' : ''}"
              style={`padding-left: ${10 + Math.min(6, hierarchyDepth(file)) * 14}px`}
              title={file.path}
            >
              <span class="mt-[5px] w-1.5 h-1.5 rounded-full shrink-0 {file.file_name === 'AGENTS.md' ? 'bg-[#2b58c9]' : 'bg-[#e8935a]'}"></span>
              <span class="min-w-0 flex-1">
                <span class="flex items-center gap-1.5">
                  <span class="font-mono text-xs text-ink">{file.file_name}</span>
                  {#if file.warnings.length > 0}
                    <span class="px-1 rounded-sm bg-amber-500/15 text-amber-500 text-[10px]">{file.warnings.length}</span>
                  {/if}
                </span>
                <span class="block text-[10px] font-mono text-ink-faint truncate">{file.relative_path}</span>
              </span>
            </button>
          {/each}
        </section>
      {/each}
    </div>
  </aside>

  <section class="flex-1 min-w-0 flex flex-col bg-app">
    {#if selected}
      <header class="px-4 py-3 border-b border-edge bg-chrome">
        <div class="flex items-start gap-3">
          <div class="min-w-0">
            <div class="flex items-center gap-2 flex-wrap">
              <h2 class="font-mono text-sm font-semibold text-ink">{selected.file_name}</h2>
              {#each selected.harnesses as harness}
                <span class="px-1.5 py-0.5 rounded-sm bg-chip text-[10px] text-ink-muted">{harnessLabel(harness)}</span>
              {/each}
              <span class="px-1.5 py-0.5 rounded-sm bg-chip text-[10px] text-ink-muted">{selected.root_source}</span>
            </div>
            <p class="mt-1 font-mono text-[11px] text-ink-faint break-all">{selected.path}</p>
          </div>
          <div class="ml-auto flex items-center gap-2 shrink-0">
            <button onclick={() => void revealSelected()} class="px-2.5 py-1 text-xs rounded-sm border border-edge bg-card hover:bg-(--row-hover)">Reveal</button>
            <button onclick={() => void openSelected()} class="px-2.5 py-1 text-xs rounded-sm bg-accent-tab text-white hover:opacity-90">Open</button>
          </div>
        </div>
        <div class="mt-2 flex items-center gap-3 text-[10px] text-ink-faint flex-wrap">
          <span>{formatBytes(selected.size)}</span>
          {#if selected.line_count !== null}<span>{selected.line_count.toLocaleString()} lines</span>{/if}
          {#if selected.modified_at}<span>modified {new Date(selected.modified_at).toLocaleString()}</span>{/if}
          <span>{selected.root_recursive ? 'recursive root' : 'folder only'}</span>
        </div>
        {#if actionError}<p class="mt-2 text-xs text-neg">{actionError}</p>{/if}
      </header>

      <div class="flex-1 min-h-0 overflow-y-auto p-4 space-y-3">
        <section class="grid grid-cols-1 xl:grid-cols-2 gap-3">
          <div class="border border-edge rounded-lg bg-card px-3 py-2">
            <h3 class="text-xs font-semibold text-ink">Effective here</h3>
            <p class="text-[11px] text-ink-faint mt-0.5">Applied from global and broad scopes toward this directory.</p>
            <div class="mt-2 space-y-1">
              {#each effectiveFiles as file, index (file.id)}
                <button
                  type="button"
                  onclick={() => (selectedId = file.id)}
                  class="w-full flex items-center gap-2 text-left text-[11px] hover:text-ink"
                >
                  <span class="w-4 text-center text-ink-faint">{index + 1}</span>
                  <span class="font-mono text-ink-2">{file.file_name}</span>
                  <span class="truncate text-ink-faint">{file.directory}</span>
                </button>
              {/each}
            </div>
          </div>

          <div class="border border-edge rounded-lg bg-card px-3 py-2">
            <h3 class="text-xs font-semibold text-ink">Review signals</h3>
            {#if selected.warnings.length === 0}
              <p class="text-[11px] text-ink-faint mt-1">No deterministic warnings for this file.</p>
            {:else}
              <div class="mt-1.5 space-y-1.5">
                {#each selected.warnings as warning}
                  <div class="text-[11px]">
                    <span class="font-semibold {warning.severity === 'warning' ? 'text-amber-500' : 'text-ink-2'}">{warning.kind.replaceAll('_', ' ')}</span>
                    <span class="text-ink-muted"> · {warning.message}</span>
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        </section>

        {#if selectedEvents.length > 0}
          <ConfigTimeline events={selectedEvents} title="Instruction impact" />
        {:else}
          <div class="border border-edge rounded-lg bg-card px-3 py-2">
            <h3 class="text-xs font-semibold text-ink">Instruction impact</h3>
            <p class="text-[11px] text-ink-faint mt-1">No recorded change for this file yet. Future watched changes will link here by local path identity and use the existing before/after correlation.</p>
          </div>
        {/if}

        <section class="border border-edge rounded-lg bg-card overflow-hidden">
          <div class="flex items-center gap-2 px-3 py-2 border-b border-edge">
            <h3 class="text-xs font-semibold text-ink">Content</h3>
            <div class="ml-auto flex bg-app rounded-sm p-[2px] border border-edge">
              <button
                onclick={() => (previewMode = 'preview')}
                class="px-2.5 py-0.5 rounded-sm text-[11px] {previewMode === 'preview' ? 'bg-ink text-app font-semibold' : 'text-ink-muted'}"
              >Preview</button>
              <button
                onclick={() => (previewMode = 'raw')}
                class="px-2.5 py-0.5 rounded-sm text-[11px] {previewMode === 'raw' ? 'bg-ink text-app font-semibold' : 'text-ink-muted'}"
              >Raw</button>
            </div>
          </div>
          {#if contentLoading}
            <p class="p-4 text-xs text-ink-faint">Loading preview…</p>
          {:else if contentError}
            <p class="p-4 text-xs text-neg">{contentError}</p>
          {:else if content !== null && previewMode === 'preview'}
            <article class="markdown-preview p-5 text-sm text-ink">{@html renderedMarkdown}</article>
          {:else if content !== null}
            <pre class="p-4 overflow-x-auto whitespace-pre-wrap wrap-break-word text-xs leading-5 font-mono text-ink-2">{content}</pre>
          {/if}
        </section>
      </div>
    {:else}
      <div class="flex h-full items-center justify-center p-8 text-sm text-ink-faint">
        Select an instruction file to inspect its effective chain and preview.
      </div>
    {/if}
  </section>
</div>
