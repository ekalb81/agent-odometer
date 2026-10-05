<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import OfflineComparisons from './OfflineComparisons.svelte';
  import { getCuratedDataset, getCuratedCandidates, previewCuratedCase, commitCuratedCase, removeCuratedCase, getOrganizationSummaries, exportCuratedDataset } from '../lib/ipc';
  import type { CuratedDataset as Dataset, CuratedCandidates, CuratedPreview, CuratedCase, OrganizationSummary, TranscriptCursor } from '../lib/types';
  import { organizationStore } from '../lib/stores/organization.svelte';
  let { sessionKey = null, onclose }: { sessionKey?: string | null; onclose: () => void } = $props();
  let dialog: HTMLDialogElement;
  let dataset = $state<Dataset | null>(null), candidates = $state<CuratedCandidates | null>(null);
  let source = $state<OrganizationSummary | null>(null), preview = $state<CuratedPreview | null>(null);
  let chosen = $state<string[]>([]), name = $state(''), rubric = $state(''), phrases = $state('');
  let caseId = $state<number | null>(null), reviewed = $state(false), busy = $state(false), loading = $state(false);
  let message = $state(''), removal = $state<CuratedCase | null>(null);
  let comparisons = $state(false);
  let generation = 0, alive = true;
  let seenEpoch = organizationStore.epoch;
  const eligible = $derived(source?.outcome?.label === 'accepted' || source?.outcome?.label === 'rejected');
  function invalidate() { generation++; preview = null; reviewed = false; message = ''; }
  $effect(() => {
    const epoch = organizationStore.epoch;
    if (epoch !== seenEpoch) { seenEpoch = epoch; dataset = null; source = null; candidates = null; chosen = []; invalidate(); void reload(); }
  });
  onDestroy(() => { alive = false; generation++; });
  onMount(() => { dialog.showModal(); void reload(); });
  async function reload() {
    invalidate(); removal = null; loading = true;
    const request = generation;
    try {
      const value = await getCuratedDataset();
      if (alive && request === generation) dataset = value;
      if (sessionKey && alive && request === generation) await loadSource(sessionKey);
    } catch { if (alive && request === generation) message = 'Dataset unavailable. Reload history before retrying.'; }
    finally { if (alive && request === generation) loading = false; }
  }
  async function loadSource(key: string, cursor: TranscriptCursor | null = null) {
    invalidate(); loading = true;
    const request = generation;
    try {
      const [rows, page] = await Promise.all([getOrganizationSummaries([key]), getCuratedCandidates(key, cursor)]);
      if (!alive || request !== generation) return;
      source = rows[0] ?? null; candidates = page;
      if (!source) message = 'Source identity is unavailable. Reload history before selecting an example.';
    } catch { if (alive && request === generation) message = 'Example source unavailable. Reload and review the source again.'; }
    finally { if (alive && request === generation) loading = false; }
  }
  function select(id: string, checked: boolean) { chosen = checked ? [...chosen, id] : chosen.filter(value => value !== id); invalidate(); }
  async function prepare() {
    if (!dataset || !source || busy) return;
    invalidate(); busy = true; const request = generation;
    try {
      const value = await previewCuratedCase({ identity: source.identity, outcome_revision: source.revision,
        dataset_revision: dataset.revision, case_id: caseId, record_ids: chosen, name, rubric,
        redact_phrases: phrases.split(/\r?\n/).filter(Boolean) });
      if (alive && request === generation) preview = value;
    } catch { if (alive && request === generation) message = 'Preview unavailable or changed. Reload, select a smaller example, and review it again.'; }
    finally { if (alive) busy = false; }
  }
  async function save() {
    if (!preview || !reviewed || busy) return;
    busy = true; const request = generation;
    try {
      const value = await commitCuratedCase(preview.token, reviewed);
      if (alive && request === generation) { dataset = value; preview = null; reviewed = false; caseId = null; chosen = []; message = 'Reviewed example saved locally.'; }
    } catch { if (alive && request === generation) { preview = null; reviewed = false; message = 'Example was not saved. The source, label, or dataset may have changed; reload and rebuild the preview.'; } }
    finally { if (alive) busy = false; }
  }
  async function edit(value: CuratedCase) {
    caseId = value.id; name = value.content.name; rubric = value.content.rubric; chosen = [...value.content.source_records]; phrases = ''; removal = null;
    await loadSource(value.session_key);
  }
  async function remove() {
    if (!dataset || !removal || busy) return;
    busy = true; const request = generation;
    try {
      const value = await removeCuratedCase(removal.id, dataset.revision);
      if (alive && request === generation) { dataset = value; if (caseId === removal.id) { caseId = null; chosen = []; } removal = null; invalidate(); message = 'Example removed. Its content is gone; the content-free version journal records the removal.'; }
    } catch { if (alive && request === generation) message = 'Example was not removed. Reload the dataset before retrying.'; }
    finally { if (alive) busy = false; }
  }
  async function exportDataset() {
    if (!dataset || busy) return;
    busy = true; const request = generation;
    try {
      const saved = await exportCuratedDataset(dataset.revision, dataset.export_digest ?? '');
      if (alive && request === generation) message = saved ? 'Saved the reviewed dataset version to your selected file.' : 'Dataset export cancelled.';
    } catch { if (alive && request === generation) message = 'Dataset export could not be saved. No destination details were recorded.'; }
    finally { if (alive) busy = false; }
  }
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={event => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="curated-heading">
  <header><h2 id="curated-heading">Curated local examples</h2><button onclick={() => dialog.close()} disabled={busy}>Close dataset</button></header>
  <div class="body">
    <p>Choose small examples from explicitly Accepted or Rejected sessions. Conversation text only: tools, reasoning, attachments, and unknown records are omitted. No model evaluation or upload occurs.</p>
    <p>At most 64 cases, four selected records per case, and 16 KiB per case. Each record is limited to 3000 UTF-8 bytes after redaction; every shortened excerpt is marked. Optional rubric: 1024 bytes. Names: 128 bytes.</p>
    <button onclick={() => void reload()} disabled={busy || loading}>Reload dataset</button>
    <button onclick={() => { comparisons = true; }} disabled={busy || loading}>Compare frozen offline runs</button>
    {#if loading}<p role="status">Loading local examples…</p>{/if}
    {#if message}<p role="status">{message}</p>{/if}
    {#if dataset}
      {#if dataset.recovery_backup_unrestored}<p class="warning">Earlier examples remain in the preserved recovery backup and were not restored. This dataset represents rebuilt history.</p>{/if}
      <section aria-label="Dataset version">
        <h3>Version {dataset.revision} · {dataset.cases.length} examples</h3>
        <p>Format {dataset.format_version}. Change history {dataset.earliest_change_revision == null ? 'is empty' : `starts at revision ${dataset.earliest_change_revision}`}. Only the latest 4096 content-free changes are retained; older comparisons may be unavailable. Cases are user-curated, not a representative sample.</p>
        <button onclick={() => void exportDataset()} disabled={busy}>Export this dataset version JSON</button>
        {#if !dataset.cases.length}<p>No curated examples yet.</p>{/if}
        {#each dataset.cases as value (value.id)}
          <details><summary>Example {value.id} · {value.content.name} · {value.content.expected_outcome} · version {value.version}</summary>
            <p>Captured from {value.content.source_provider} at {value.content.captured_at}. {value.content.source_records.length} selected source records. The stored case is a reviewed excerpt. Source availability is checked again when editing; a missing source does not erase this saved excerpt.</p>
            {#each value.content.blocks as block}<h4>{block.role}{block.truncated ? ' · shortened excerpt' : ''}</h4><pre>{block.text}</pre>{/each}
            {#if value.content.rubric}<p>Rubric: {value.content.rubric}</p>{/if}
            <button onclick={() => void edit(value)} disabled={busy || loading}>Edit example {value.id}</button>
            <button onclick={() => { removal = value; }} disabled={busy}>Review removal of example {value.id}</button>
          </details>
        {/each}
        {#if removal}<section aria-label="Review example removal"><p>Remove example {removal.id}: {removal.content.name}? Case content is removed from the dataset. Its ID and change hash remain in the version journal; earlier exports and backups remain separate files.</p>
          <button onclick={() => void remove()} disabled={busy}>Remove reviewed example</button><button onclick={() => { removal = null; }} disabled={busy}>Cancel removal</button>
        </section>{/if}
      </section>
      {#if source}
        {#if !eligible}<p>Use Edit organization to enter Accepted or Rejected before curating this source.</p>
        {:else}
          <fieldset disabled={busy || loading}><legend>{caseId ? `Replace example ${caseId}` : 'Select an example'} · human label: {source.outcome?.label}</legend>
            <p>Source: {candidates?.availability ?? 'unavailable'}. {chosen.length} records selected. Selection can span pages; the exact preview shows all included content.</p>
            {#each candidates?.records ?? [] as record, index (record.record_id)}<label class="record"><input type="checkbox" checked={chosen.includes(record.record_id)} disabled={!chosen.includes(record.record_id) && chosen.length >= 4} onchange={event => select(record.record_id, event.currentTarget.checked)} /> Record {index + 1} · {record.role}: {record.excerpt}</label>{/each}
            {#if !candidates?.records.length}<p>No readable conversation text on this source page. Missing or changed sources cannot be added.</p>{/if}
            <button onclick={() => void loadSource(source!.identity.session_key)}>Start source pages again</button>
            {#if candidates?.next_cursor}<button onclick={() => void loadSource(source!.identity.session_key, candidates!.next_cursor)}>Next source page</button>{/if}
            <label>Example name<input bind:value={name} maxlength="128" oninput={invalidate} /></label>
            <label>Optional review rubric<textarea bind:value={rubric} maxlength="1024" rows="2" oninput={invalidate}></textarea></label>
            <label>Also redact these exact phrases, one per line<textarea bind:value={phrases} maxlength="25700" rows="2" oninput={invalidate}></textarea></label>
            <p>Common credential fields, paths, URLs, and encoded content are redacted automatically, using the transcript export policy. No automatic redaction finds every secret. Review every included section before adding it.</p>
            <button onclick={() => void prepare()} disabled={!chosen.length || !name.trim()}>Build exact minimized preview</button>
          </fieldset>
        {/if}
      {/if}
      {#if preview}<section aria-label="Exact curated example preview"><h3>{preview.content.name} · {preview.content.expected_outcome}</h3>
        <p>{preview.content.blocks.length} selected conversation records · {preview.content.redactions} redactions. Review authorization expires after five minutes. Source provenance is bound to this exact selection.</p>
        {#each preview.content.blocks as block}<h4>{block.role}{block.truncated ? ' · shortened excerpt' : ''}</h4><pre>{block.text}</pre>{/each}
        {#if preview.content.rubric}<p>Rubric: {preview.content.rubric}</p>{/if}
        <label class="record"><input type="checkbox" bind:checked={reviewed} disabled={busy} /> I reviewed every included section and want to store this exact example locally.</label>
        <button onclick={() => void save()} disabled={!reviewed || busy}>{caseId ? 'Replace reviewed example' : 'Add reviewed example'}</button>
      </section>{/if}
      <p>Confirmed history purge also removes cases from that source. Deletion removes case content; version changes disclose additions, edits, and removals without keeping older content. Private organization notes and tags are never included. Ordinary exports, diagnostics, logs, and MCP omit this dataset.</p>
    {/if}
  </div>
</dialog>
{#if comparisons}<OfflineComparisons onclose={() => { comparisons = false; }} />{/if}

<style>
  dialog { width: min(900px, calc(100vw - 24px)); max-height: calc(100vh - 24px); margin: auto; padding: 0; border: 1px solid var(--border); border-radius: 8px; background: var(--panel); color: var(--text); }
  dialog::backdrop { background: #0008; }
  header { position: sticky; top: 0; z-index: 1; background: var(--panel); display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 16px; border-bottom: 1px solid var(--border); }
  h2, h3 { font-weight: 600; } h2 { font-size: 1rem; }
  .body { padding: 16px; display: flex; flex-direction: column; gap: 12px; font-size: .75rem; }
  section, fieldset, details { border: 1px solid var(--border); border-radius: 4px; padding: 12px; display: flex; flex-direction: column; gap: 10px; }
  label { display: flex; flex-direction: column; gap: 4px; } .record { flex-direction: row; align-items: flex-start; overflow-wrap: anywhere; }
  input:not([type=checkbox]), textarea { width: 100%; border: 1px solid var(--border); padding: 6px; background: var(--bg); }
  button { border: 1px solid var(--border); border-radius: 4px; padding: 5px 10px; } button:disabled { opacity: .5; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; background: var(--bg); padding: 10px; }
  .warning { color: var(--amber, #b66d00); }
</style>
