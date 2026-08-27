<script lang="ts">
  import { projectStore } from '../lib/stores/projects.svelte';
  import { mergeProjects, setProjectAlias, unmergeProject } from '../lib/ipc';
  import type { ProjectInfo, ProjectProvenance } from '../lib/types';

  /**
   * Local alias / merge management for the project dimension (issue #41).
   *
   * The backend for this has existed since project identity landed —
   * `set_project_alias`, `merge_projects`, `unmerge_project` and
   * `resolve_projects` are all registered commands with IPC bindings, and
   * `projectStore.all()` was written with the comment "for a management
   * UI". Nothing called them, so a merge or a rename could not be made or
   * undone from inside the app.
   *
   * Everything here is reversible and local: an alias never rewrites a
   * source transcript or the auto-computed `project_label`, and a merge is
   * a display-time fold that `unmergeProject` restores. The copy says so,
   * because "merge" reads as destructive if nothing tells you otherwise.
   */

  const PROVENANCE_LABEL: Record<ProjectProvenance, string> = {
    repository_root: 'Repository root',
    workspace_root: 'Workspace root',
    provider_project_id: 'Provider project id',
    fallback_path_identity: 'Path identity',
  };

  const PROVENANCE_HINT: Record<ProjectProvenance, string> = {
    repository_root: 'Resolved from the Git repository containing the working directory.',
    workspace_root: 'Resolved from a workspace or monorepo root above the working directory.',
    provider_project_id: 'Resolved from a project id the agent harness reported.',
    fallback_path_identity:
      'No repository or workspace root was found, so the working directory path itself identifies this project.',
  };

  let projects = $state<ProjectInfo[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let busyKey = $state<string | null>(null);

  /** Key of the project whose rename row is open, and its draft label. */
  let renamingKey = $state<string | null>(null);
  let renameDraft = $state('');

  /** Key of the project whose merge row is open, and the chosen target. */
  let mergingKey = $state<string | null>(null);
  let mergeTargetKey = $state('');

  async function reload(): Promise<void> {
    loading = true;
    await projectStore.refresh();
    projects = [...projectStore.all()].sort((a, b) =>
      a.label.localeCompare(b.label, undefined, { sensitivity: 'base' }),
    );
    loading = false;
  }

  $effect(() => {
    void reload();
  });

  /** Runs one override edit, holding the row busy and surfacing failures. */
  async function edit(projectKey: string, action: () => Promise<unknown>): Promise<void> {
    busyKey = projectKey;
    error = null;
    try {
      await action();
      await reload();
    } catch (cause) {
      // The backend rejects a self-merge and a merge that would create a
      // cycle; both arrive here as text meant to be read.
      error = String(cause);
    } finally {
      busyKey = null;
    }
  }

  function startRename(project: ProjectInfo): void {
    mergingKey = null;
    renamingKey = project.project_key;
    renameDraft = project.label;
  }

  async function saveRename(project: ProjectInfo): Promise<void> {
    const trimmed = renameDraft.trim();
    renamingKey = null;
    if (trimmed === project.label) return;
    // An empty box means "no alias" rather than a project literally named
    // "", which is what the reset button does deliberately.
    const alias = trimmed.length === 0 ? null : trimmed;
    await edit(project.project_key, () => setProjectAlias(project.project_key, alias));
  }

  async function clearAlias(project: ProjectInfo): Promise<void> {
    renamingKey = null;
    await edit(project.project_key, () => setProjectAlias(project.project_key, null));
  }

  function startMerge(project: ProjectInfo): void {
    renamingKey = null;
    mergingKey = project.project_key;
    mergeTargetKey = '';
  }

  async function confirmMerge(project: ProjectInfo): Promise<void> {
    if (!mergeTargetKey) return;
    const target = mergeTargetKey;
    mergingKey = null;
    await edit(project.project_key, () => mergeProjects(project.project_key, target));
  }

  async function undoMerge(project: ProjectInfo): Promise<void> {
    await edit(project.project_key, () => unmergeProject(project.project_key));
  }

  /** Merge targets for `project`: every other project. */
  function mergeTargets(project: ProjectInfo): ProjectInfo[] {
    return projects.filter((candidate) => candidate.project_key !== project.project_key);
  }

  const fmt = new Intl.NumberFormat();
</script>

<section>
  <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Projects</h2>
  <p class="text-xs text-ink-faint mb-3 max-w-3xl">
    Odometer groups sessions by the project their working directory belongs to, resolved from a
    repository root where it can find one. Rename a project to give it a local label, or merge two
    that are the same work &mdash; a repository checked out twice, or a directory you renamed. Both
    are local, reversible, and display-only: neither rewrites a source transcript or the
    auto-detected name underneath.
  </p>

  {#if error}
    <p class="text-xs text-amber-500 mb-3 max-w-3xl" role="alert">{error}</p>
  {/if}

  <div class="bg-card border border-edge rounded-lg max-w-3xl overflow-hidden">
    {#if loading}
      <p class="text-xs text-ink-faint px-4 py-3">Loading projects&hellip;</p>
    {:else if projects.length === 0}
      <p class="text-xs text-ink-faint px-4 py-3">
        No projects yet. One appears once a scanned session has a working directory.
      </p>
    {:else}
      <table class="w-full text-xs">
        <thead>
          <tr class="text-ink-faint border-b border-edge">
            <th class="text-left font-medium px-4 py-2">Project</th>
            <th class="text-left font-medium px-4 py-2">Identified by</th>
            <th class="text-right font-medium px-4 py-2">Sessions</th>
            <th class="text-right font-medium px-4 py-2 w-px whitespace-nowrap">Actions</th>
          </tr>
        </thead>
        <tbody>
          {#each projects as project (project.project_key)}
            {@const merged = project.member_keys.length > 1}
            {@const busy = busyKey === project.project_key}
            <tr class="border-b border-edge/50 last:border-b-0 align-top">
              <td class="px-4 py-2">
                <span class="text-ink font-medium">{project.label}</span>
                {#if merged}
                  <span
                    class="ml-1 text-[10px] text-ink-faint cursor-help"
                    title="{project.member_keys.length} auto-detected projects are folded into this one. Undo restores them."
                  >
                    &middot; merged from {project.member_keys.length}
                  </span>
                {/if}
                <span class="block text-[10px] text-ink-faint font-mono mt-0.5 break-all">
                  {project.project_key}
                </span>
              </td>
              <td class="px-4 py-2 text-ink-2">
                <span class="cursor-help" title={PROVENANCE_HINT[project.provenance]}>
                  {PROVENANCE_LABEL[project.provenance]}
                </span>
              </td>
              <td class="px-4 py-2 text-right text-ink-2 font-mono">
                {fmt.format(project.session_count)}
              </td>
              <td class="px-4 py-2 text-right whitespace-nowrap">
                <button
                  class="text-accent hover:underline disabled:opacity-50 disabled:no-underline"
                  disabled={busy}
                  onclick={() => startRename(project)}
                >
                  Rename
                </button>
                <button
                  class="ml-3 text-accent hover:underline disabled:opacity-50 disabled:no-underline"
                  disabled={busy || projects.length < 2}
                  title={projects.length < 2
                    ? 'Merging needs a second project to merge into.'
                    : 'Fold this project into another one, for display only.'}
                  onclick={() => startMerge(project)}
                >
                  Merge&hellip;
                </button>
                {#if merged}
                  <button
                    class="ml-3 text-accent hover:underline disabled:opacity-50 disabled:no-underline"
                    disabled={busy}
                    onclick={() => undoMerge(project)}
                  >
                    Undo merge
                  </button>
                {/if}
              </td>
            </tr>

            {#if renamingKey === project.project_key}
              <tr class="border-b border-edge/50 bg-app/40">
                <td colspan="4" class="px-4 py-3">
                  <div class="flex items-center gap-2 flex-wrap">
                    <label class="text-ink-2" for="project-alias-{project.project_key}">
                      Local label
                    </label>
                    <input
                      id="project-alias-{project.project_key}"
                      class="bg-card border border-edge rounded px-2 py-1 text-ink min-w-64"
                      bind:value={renameDraft}
                      onkeydown={(event) => {
                        if (event.key === 'Enter') void saveRename(project);
                        if (event.key === 'Escape') renamingKey = null;
                      }}
                    />
                    <button class="text-accent hover:underline" onclick={() => void saveRename(project)}>
                      Save
                    </button>
                    <button class="text-ink-faint hover:underline" onclick={() => (renamingKey = null)}>
                      Cancel
                    </button>
                    <button
                      class="ml-auto text-ink-faint hover:underline"
                      title="Drop the local label and show the auto-detected one again."
                      onclick={() => void clearAlias(project)}
                    >
                      Reset to detected name
                    </button>
                  </div>
                </td>
              </tr>
            {/if}

            {#if mergingKey === project.project_key}
              <tr class="border-b border-edge/50 bg-app/40">
                <td colspan="4" class="px-4 py-3">
                  <div class="flex items-center gap-2 flex-wrap">
                    <label class="text-ink-2" for="project-merge-{project.project_key}">
                      Show <span class="text-ink font-medium">{project.label}</span> under
                    </label>
                    <select
                      id="project-merge-{project.project_key}"
                      class="bg-card border border-edge rounded px-2 py-1 text-ink"
                      bind:value={mergeTargetKey}
                    >
                      <option value="">Choose a project&hellip;</option>
                      {#each mergeTargets(project) as target (target.project_key)}
                        <option value={target.project_key}>{target.label}</option>
                      {/each}
                    </select>
                    <button
                      class="text-accent hover:underline disabled:opacity-50 disabled:no-underline"
                      disabled={!mergeTargetKey}
                      onclick={() => void confirmMerge(project)}
                    >
                      Merge
                    </button>
                    <button class="text-ink-faint hover:underline" onclick={() => (mergingKey = null)}>
                      Cancel
                    </button>
                    <span class="basis-full text-[11px] text-ink-faint">
                      Its sessions keep their own history and stay separately attributed underneath;
                      only the grouping changes. Undo merge puts it back.
                    </span>
                  </div>
                </td>
              </tr>
            {/if}
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</section>
