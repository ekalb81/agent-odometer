<script lang="ts">
  import { onMount } from 'svelte';
  import { listSavedSearches, listOrganizationTags, saveSearch, deleteSavedSearch, changeOrganizationTag, getOrganizationRecoveryState } from '../lib/ipc';
  import { savedSummarySearch, restoredSummaryFilters } from '../lib/organization';
  import { organizationStore } from '../lib/stores/organization.svelte';
  import type { SavedSearch } from '../lib/types';
  import type { SessionFilterState, ViewScope } from '../lib/sessionProjection';

  let { scope, filters, pinnedOnly = false, selectedTags = [], scopes,
    onchange, onrestore }: {
    scope: ViewScope; filters: SessionFilterState; pinnedOnly?: boolean;
    selectedTags?: string[]; scopes: string[];
    onchange: (pinned: boolean, tags: string[]) => void;
    onrestore: (scope: string, filters: SessionFilterState, pinned: boolean, tags: string[]) => void;
  } = $props();
  let searches = $state<SavedSearch[]>([]);
  let tags = $state<string[]>([]);
  let name = $state('');
  let chosen = $state('');
  let rename = $state('');
  let tag = $state('');
  let tagName = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let message = $state('');
  let recoveryPending = $state(false);
  let popover: HTMLDetailsElement;
  const selected = $derived(searches.find(search => String(search.id) === chosen));
  onMount(() => { void load(); });

  async function load() {
    busy = true; error = null;
    try { [searches, tags, recoveryPending] = await Promise.all([listSavedSearches(), listOrganizationTags(), getOrganizationRecoveryState()]); }
    catch (cause) { error = String(cause); }
    finally { busy = false; }
  }
  async function action(work: () => Promise<unknown>) {
    busy = true; error = null; message = '';
    try { await work(); await load(); message = 'Local organization saved.'; }
    catch (cause) { error = String(cause); }
    finally { busy = false; }
  }
  function restore() {
    if (!selected) return;
    const definition = selected.definition;
    const restored = restoredSummaryFilters(definition);
    if (!restored) { error = 'This saved search requires explicit session-content search. It is unavailable in the summary view.'; return; }
    if (!scopes.includes(definition.scope)) { error = 'This saved search requires an unavailable provider.'; return; }
    if (definition.tags.some(label => !tags.includes(label))) { error = 'This saved search contains a deleted or renamed tag. Edit its filters before running it.'; return; }
    onrestore(definition.scope, restored, definition.pinned_only, definition.tags);
    popover.open = false;
  }
  async function refreshMetadata() {
    await organizationStore.load(Object.keys(organizationStore.summaries));
  }
</script>

<svelte:window onkeydown={(event) => { if (event.key === 'Escape' && popover?.open) { popover.open = false; popover.querySelector('summary')?.focus(); } }} />
<details bind:this={popover} class="relative text-xs" ontoggle={() => { if (popover.open) void load(); }}>
  <summary class="cursor-pointer text-ink-muted hover:text-ink px-2 py-1 list-none">Organize{pinnedOnly || selectedTags.length ? ' · filtered' : ''}</summary>
  <div class="absolute right-0 top-full mt-2 z-40 w-[min(28rem,calc(100vw-2rem))] max-h-[75vh] overflow-y-auto bg-panel border border-edge rounded-md shadow-xl p-4 space-y-4" aria-label="Local session organization">
    <p class="text-ink-muted">Pins and tags filter sessions. Text search uses summary fields. Private notes remain local and are excluded from exports and MCP.</p>
    {#if recoveryPending}<p class="text-amber-500">Earlier organization and saved searches were preserved in the recovery backup. They were not reconstructed from source transcripts.</p>{/if}
    <label class="flex items-center gap-2"><input type="checkbox" checked={pinnedOnly} onchange={(event) => onchange(event.currentTarget.checked, selectedTags)} /> Pinned sessions only</label>
    <label class="block">Filter by tags (all selected tags)
      <select multiple class="block w-full mt-1 bg-card border border-edge p-1" value={selectedTags} onchange={(event) => onchange(pinnedOnly, [...event.currentTarget.selectedOptions].map(option => option.value))} disabled={busy}>
        {#each tags as label}<option value={label}>{label}</option>{/each}
      </select>
    </label>
    {#if pinnedOnly || selectedTags.length}<button class="text-accent underline" onclick={() => onchange(false, [])}>Clear organization filters</button>{/if}
    <div class="border-t border-edge pt-3 space-y-2">
      <h3 class="section-label">Saved searches</h3>
      <p class="text-ink-faint">Save summary query, provider, model, date bounds, archive/subagent choices, and organization filters. Dates stay fixed in UTC.</p>
      <label class="block">New search name <input class="block w-full bg-card border border-edge px-2 py-1 mt-1" bind:value={name} disabled={busy} /></label>
      <button class="text-accent underline" disabled={busy || !name.trim()} onclick={() => void action(async () => { await saveSearch(null, 0, savedSummarySearch(name.trim(), scope, filters, pinnedOnly, selectedTags)); name = ''; })}>Save current search</button>
      <label class="block">Saved search
        <select class="block w-full bg-card border border-edge p-1 mt-1" bind:value={chosen} disabled={busy} onchange={() => { rename = ''; error = null; }}>
          <option value="">Choose a saved search…</option>
          {#each searches as search (search.id)}<option value={String(search.id)}>{search.definition.name} · {search.definition.content_scope === 'summary' ? 'summary' : 'session content'} · {search.definition.scope}</option>{/each}
        </select>
      </label>
      {#if selected}
        <div class="flex flex-wrap gap-3">
          <button class="text-accent underline" disabled={busy} onclick={restore}>Run saved search</button>
          <button class="text-accent underline" disabled={busy} onclick={() => void action(() => saveSearch(selected!.id, selected!.revision, savedSummarySearch(selected!.definition.name, scope, filters, pinnedOnly, selectedTags)))}>Replace with current filters</button>
          <button class="text-amber-500 underline" disabled={busy} onclick={() => void action(async () => { await deleteSavedSearch(selected!.id, selected!.revision); chosen = ''; })}>Delete saved search</button>
        </div>
        <label class="block">Rename selected search <input class="block w-full bg-card border border-edge px-2 py-1 mt-1" bind:value={rename} disabled={busy} /></label>
        <button class="text-accent underline" disabled={busy || !rename.trim()} onclick={() => void action(() => saveSearch(selected!.id, selected!.revision, { ...selected!.definition, name: rename.trim() }))}>Rename search</button>
      {/if}
    </div>
    <div class="border-t border-edge pt-3 space-y-2">
      <h3 class="section-label">Manage tags</h3>
      <label class="block">Tag <select class="block w-full bg-card border border-edge p-1 mt-1" bind:value={tag} disabled={busy}><option value="">Choose a tag…</option>{#each tags as label}<option value={label}>{label}</option>{/each}</select></label>
      {#if tag}
        <label class="block">New tag label <input class="block w-full bg-card border border-edge px-2 py-1 mt-1" bind:value={tagName} disabled={busy} /></label>
        <div class="flex gap-3">
          <button class="text-accent underline" disabled={busy || !tagName.trim()} onclick={() => void action(async () => { await changeOrganizationTag(tag, tagName.trim()); tag = ''; tagName = ''; await refreshMetadata(); })}>Rename tag</button>
          <button class="text-amber-500 underline" disabled={busy} onclick={() => void action(async () => { await changeOrganizationTag(tag, null); tag = ''; await refreshMetadata(); })}>Delete tag</button>
        </div>
        <p class="text-ink-faint">Changes affect all local sessions using the label. Saved searches keep their original tag choice and require review after a rename or deletion.</p>
      {/if}
    </div>
    {#if busy}<p role="status" class="text-ink-muted">Loading or saving organization…</p>{/if}
    {#if message}<p role="status" class="text-ink-muted">{message}</p>{/if}
    {#if error}<p role="alert" class="text-amber-500">{error} <button class="underline" disabled={busy} onclick={() => void load()}>Reload saved searches and tags</button></p>{/if}
  </div>
</details>
