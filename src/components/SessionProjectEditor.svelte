<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { clearSessionProjectOverride, reassignSessionProject } from '../lib/ipc';
  import { projectStore } from '../lib/stores/projects.svelte';
  import type { SessionSummary } from '../lib/types';

  let { session }: { session: Pick<SessionSummary, 'storage_id' | 'project_key' | 'project_label'> } = $props();
  let editing = $state(false);
  let target = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let saved = $state(false);
  let alive = true;
  const current = $derived(projectStore.forSession(session));
  const overridden = $derived(projectStore.hasOverride(session.storage_id));
  const projects = $derived(projectStore.all().sort((a, b) => a.label.localeCompare(b.label)));

  onMount(() => { void projectStore.load(); });
  onDestroy(() => { alive = false; });

  function toggleEditor() {
    editing = !editing;
    target = '';
    error = null;
    if (editing) void projectStore.refreshIfIdle();
  }

  async function refresh() {
    await projectStore.refresh();
    if (alive && !projectStore.error) {
      saved = false;
      error = null;
    }
  }

  async function edit(action: 'assign' | 'split' | 'restore') {
    if (busy || (action === 'assign' && !target)) return;
    const sessionKey = session.storage_id;
    const projectKey = target;
    busy = true;
    error = null;
    saved = false;
    try {
      if (action === 'restore') await clearSessionProjectOverride(sessionKey);
      else await reassignSessionProject(sessionKey, action === 'split' ? null : projectKey);
      if (alive) saved = true;
      // Refresh even if selection changed while the edit was pending.
      await projectStore.refresh();
      if (alive && !projectStore.error) {
        editing = false;
        saved = false;
      }
    } catch (cause) {
      if (alive) error = String(cause);
    } finally {
      if (alive) busy = false;
    }
  }
</script>

<section class="px-5 py-3 border-b border-edge text-[11px]" aria-label="Session project">
  <div class="flex items-center gap-2 min-w-0">
    <span class="section-label">Project</span>
    <span class="min-w-0 flex-1 truncate text-ink" title={current?.label ?? session.project_label ?? undefined}>
      {current?.label ?? session.project_label ?? 'No project assigned'}
    </span>
    <button class="text-accent hover:underline shrink-0" aria-expanded={editing}
      disabled={busy} onclick={toggleEditor}>
      {editing ? 'Cancel' : 'Change project'}
    </button>
  </div>
  {#if editing}
    <div class="mt-3 space-y-2">
      <p class="text-ink-muted">Move this session to an existing project, or give it a standalone project. You can restore its detected project later.</p>
      {#if !projectStore.loaded}
        <p role="status" class="text-ink-muted">Loading projects…</p>
      {:else if !projectStore.error}
        <label class="block text-ink-muted" for="session-project-{session.storage_id}">Destination project</label>
        <div class="flex gap-2">
          <select id="session-project-{session.storage_id}" bind:value={target} disabled={busy}
            class="min-w-0 flex-1 bg-card border border-edge rounded-sm px-2 py-1 text-ink">
            <option value="">Choose a project…</option>
            {#each projects.filter((project) => project.project_key !== current?.project_key) as project (project.project_key)}
              <option value={project.project_key}>{project.label}</option>
            {/each}
          </select>
          <button class="text-accent hover:underline disabled:opacity-50" disabled={busy || !target}
            onclick={() => void edit('assign')}>Move session</button>
        </div>
        <div class="flex flex-wrap gap-x-3 gap-y-2">
          <button class="text-accent hover:underline disabled:opacity-50" disabled={busy}
            onclick={() => void edit('split')}>Make standalone project</button>
          {#if overridden}
            <button class="text-accent hover:underline disabled:opacity-50" disabled={busy}
              onclick={() => void edit('restore')}>Restore detected project</button>
          {/if}
        </div>
        <p class="text-ink-faint">Only local grouping changes. Source transcripts stay unchanged. Rename standalone projects in Settings.</p>
      {/if}
    </div>
  {/if}
  {#if busy}<p role="status" class="mt-2 text-ink-muted">Saving project…</p>{/if}
  {#if error}<p role="alert" class="mt-2 text-amber-500">{error}</p>{/if}
  {#if projectStore.error}
    <div class="mt-2 text-amber-500" role="alert">
      {saved ? 'Project saved, but the displayed grouping could not be refreshed.' : 'Could not load projects. Displayed grouping may be out of date.'}
      <button class="ml-1 underline" disabled={busy} onclick={() => void refresh()}>Retry</button>
    </div>
  {/if}
</section>
