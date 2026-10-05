<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { getOrganizationSummaries, getSessionAnnotation, editSessionAnnotation } from '../lib/ipc';
  import { organizationStore } from '../lib/stores/organization.svelte';
  import type { HumanOutcome, SessionAnnotation } from '../lib/types';
  import { notRated } from '../lib/humanOutcomes';
  import CuratedDataset from './CuratedDataset.svelte';

  let { sessionKey }: { sessionKey: string } = $props();
  let annotation = $state<SessionAnnotation | null>(null);
  let editing = $state(false);
  let note = $state('');
  let tags = $state('');
  let pinned = $state(false);
  let outcome = $state<HumanOutcome>(notRated());
  let outcomeChanged = $state(false);
  let repairMinutes = $state('');
  let firstPass = $state('');
  let busy = $state(false);
  let error = $state<string | null>(null);
  let datasetOpen = $state(false);
  let alive = true;
  onDestroy(() => { alive = false; });
  onMount(() => { void load(); });

  async function load() {
    const epoch = organizationStore.epoch;
    busy = true; error = null;
    try {
      const rows = await getOrganizationSummaries([sessionKey]);
      if (!rows[0]) throw new Error('Organization unavailable for this session');
      const value = await getSessionAnnotation(rows[0].identity);
      if (alive) {
        if (!organizationStore.update(value.summary, epoch)) throw new Error('Organization changed; reload before editing');
        annotation = value; note = value.note; tags = value.summary.tags.join(', ');
        pinned = value.summary.pinned;
        resetOutcome(value);
      }
    } catch (cause) { if (alive) error = String(cause); }
    finally { if (alive) busy = false; }
  }

  async function save() {
    if (!annotation || busy) return;
    const epoch = organizationStore.epoch;
    busy = true; error = null;
    try {
      const value = await editSessionAnnotation({
        identity: annotation.summary.identity, revision: annotation.summary.revision,
        pinned, note, tags: [...new Set(tags.split(',').map(tag => tag.trim()).filter(Boolean))],
        ...(outcomeChanged ? { outcome: { label: outcome.label,
          repair_minutes: repairMinutes.trim() ? Number(repairMinutes) : null,
          first_pass_accepted: firstPass === '' ? null : firstPass === 'true' } } : {}),
      });
      if (alive) {
        if (!organizationStore.update(value.summary, epoch)) throw new Error('Organization changed; reload before saving');
        annotation = value; resetOutcome(value); editing = false;
      }
    } catch (cause) { if (alive) error = String(cause); }
    finally { if (alive) busy = false; }
  }

  function cancel() {
    if (!annotation) return;
    note = annotation.note; tags = annotation.summary.tags.join(', ');
    pinned = annotation.summary.pinned; resetOutcome(annotation); editing = false; error = null;
  }
  function resetOutcome(value: SessionAnnotation) {
    outcome = { ...(value.summary.outcome ?? notRated()) };
    repairMinutes = outcome.repair_minutes == null ? '' : String(outcome.repair_minutes);
    firstPass = outcome.first_pass_accepted == null ? '' : String(outcome.first_pass_accepted);
    outcomeChanged = false;
  }
</script>

<section class="px-5 py-3 border-b border-edge text-[11px]" aria-label="Private session organization">
  <div class="flex items-center gap-2">
    <span class="section-label">Organization</span>
    {#if annotation}
      <span class="text-ink-muted flex-1">{annotation.summary.pinned ? 'Pinned · ' : ''}{annotation.summary.tags.join(' · ')}{annotation.summary.has_note ? ' · Private note' : ''}{annotation.summary.outcome && annotation.summary.outcome.label !== 'not_rated' ? ` · Human: ${annotation.summary.outcome.label}` : ''}</span>
      <button class="text-accent hover:underline" disabled={busy} aria-expanded={editing} onclick={() => { editing ? cancel() : editing = true; }}>{editing ? 'Cancel' : 'Edit organization'}</button>
    {/if}
  </div>
  {#if !editing && (annotation?.summary.outcome?.label === 'accepted' || annotation?.summary.outcome?.label === 'rejected')}
    <button class="mt-2 text-accent underline" onclick={() => { datasetOpen = true; }}>Curate reviewed example</button>
  {/if}
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
      <p class="text-ink-faint">Optional repair details belong in this private note.</p>
      <fieldset class="border-t border-edge pt-2 space-y-2" disabled={busy}>
        <legend class="section-label">Explicit human outcome</legend>
        <label class="block">Task outcome
          <select class="block w-full mt-1 bg-card border border-edge px-2 py-1.5" bind:value={outcome.label} onchange={() => { outcomeChanged = true; if (outcome.label === 'not_rated' || (outcome.label !== 'accepted' && firstPass === 'true')) firstPass = ''; }}>
            <option value="not_rated">Not rated</option><option value="accepted">Accepted</option><option value="rejected">Rejected</option><option value="unresolved">Unresolved</option>
          </select>
        </label>
        <label class="block">User-reported repair minutes (optional)
          <input type="number" min="0" max="525600" step="1" class="block w-full mt-1 bg-card border border-edge px-2 py-1.5" value={repairMinutes} oninput={(event) => { repairMinutes = event.currentTarget.value; outcomeChanged = true; }} />
        </label>
        <label class="block">Accepted on first pass (explicit report)
          <select class="block w-full mt-1 bg-card border border-edge px-2 py-1.5" bind:value={firstPass} disabled={outcome.label === 'not_rated'} onchange={() => { outcomeChanged = true; }}>
            <option value="">Not reported</option><option value="true" disabled={outcome.label !== 'accepted'}>Yes</option><option value="false">No</option>
          </select>
        </label>
        <p class="text-ink-faint">No label is inferred from Git status, tokens, cost, or repair time. The root task includes linked subagent work; separately rated subagents are excluded from task summaries. Repair minutes do not measure session duration or accepted end-to-end delivery time.</p>
      </fieldset>
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
{#if datasetOpen}<CuratedDataset {sessionKey} onclose={() => { datasetOpen = false; }} />{/if}
