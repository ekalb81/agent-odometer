<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { getOrganizationSummaries, getSessionAnnotation, editSessionAnnotation } from '../lib/ipc';
  import { organizationStore } from '../lib/stores/organization.svelte';
  import type { SessionAnnotation } from '../lib/types';

  let { sessionKey }: { sessionKey: string } = $props();
  let annotation = $state<SessionAnnotation | null>(null);
  let editing = $state(false);
  let note = $state('');
  let tags = $state('');
  let pinned = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let alive = true;
  onDestroy(() => { alive = false; });
  onMount(() => { void load(); });

  async function load() {
    busy = true; error = null;
    try {
      const rows = await getOrganizationSummaries([sessionKey]);
      if (!rows[0]) throw new Error('Organization unavailable for this session');
      const value = await getSessionAnnotation(rows[0].identity);
      if (alive) {
        annotation = value; note = value.note; tags = value.summary.tags.join(', ');
        pinned = value.summary.pinned; organizationStore.update(value.summary);
      }
    } catch (cause) { if (alive) error = String(cause); }
    finally { if (alive) busy = false; }
  }

  async function save() {
    if (!annotation || busy) return;
    busy = true; error = null;
    try {
      const value = await editSessionAnnotation({
        identity: annotation.summary.identity, revision: annotation.summary.revision,
        pinned, note, tags: [...new Set(tags.split(',').map(tag => tag.trim()).filter(Boolean))],
      });
      organizationStore.update(value.summary);
      if (alive) { annotation = value; editing = false; }
    } catch (cause) { if (alive) error = String(cause); }
    finally { if (alive) busy = false; }
  }

  function cancel() {
    if (!annotation) return;
    note = annotation.note; tags = annotation.summary.tags.join(', ');
    pinned = annotation.summary.pinned; editing = false; error = null;
  }
</script>

<section class="px-5 py-3 border-b border-edge text-[11px]" aria-label="Private session organization">
  <div class="flex items-center gap-2">
    <span class="section-label">Organization</span>
    {#if annotation}
      <span class="text-ink-muted flex-1">{annotation.summary.pinned ? 'Pinned · ' : ''}{annotation.summary.tags.join(' · ')}{annotation.summary.has_note ? ' · Private note' : ''}</span>
      <button class="text-accent hover:underline" disabled={busy} aria-expanded={editing} onclick={() => { editing ? cancel() : editing = true; }}>{editing ? 'Cancel' : 'Edit organization'}</button>
    {/if}
  </div>
  {#if annotation?.recovery_backup_unrestored}
    <p class="mt-2 text-amber-500">Recovery rebuilt source history. Earlier pins, tags, and notes remain in the preserved database backup and were not restored. Edits here belong to the rebuilt history.</p>
  {/if}
  {#if editing}
    <div class="mt-3 space-y-2">
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={pinned} disabled={busy} /> Pin this session</label>
      <label class="block">Tags (comma separated)
        <input class="block w-full mt-1 bg-card border border-edge rounded-sm px-2 py-1.5" bind:value={tags} disabled={busy} />
      </label>
      <label class="block">Private note
        <textarea class="block w-full mt-1 bg-card border border-edge rounded-sm px-2 py-1.5" rows="4" bind:value={note} disabled={busy}></textarea>
      </label>
      <p class="text-ink-faint">Local only. Notes and tags are excluded from reports, exports, diagnostics, and MCP. Confirmed history purge removes this session’s organization.</p>
      <button class="text-accent hover:underline" disabled={busy} onclick={() => void save()}>Save organization</button>
      <button class="ml-3 text-ink-muted hover:underline" disabled={busy} onclick={cancel}>Discard changes</button>
    </div>
  {/if}
  {#if busy}<p role="status" class="mt-2 text-ink-muted">{editing ? 'Saving' : 'Loading'} organization…</p>{/if}
  {#if error}
    <p role="alert" class="mt-2 text-amber-500">{error} <button class="underline" disabled={busy} onclick={() => void load()}>Reload organization</button></p>
  {/if}
</section>
