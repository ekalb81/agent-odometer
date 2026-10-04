<script lang="ts">
  import { onMount } from 'svelte';
  import type { Session } from '../lib/types';
  import { getTranscriptPage, writeExport } from '../lib/ipc';
  import { retainedHandoff, sourceHandoff, prepareHandoff, type HandoffSelection } from '../lib/transcriptHandoff';
  import { redactTranscriptText } from '../lib/transcriptExport';
  import TranscriptInspector from './TranscriptInspector.svelte';

  let { session, onclose }: { session: Session; onclose: () => void } = $props();
  let dialog: HTMLDialogElement;
  let selection = $state<HandoffSelection>({ records: [], coverage: '' });
  let selected = $state<string[]>([]);
  let includeTools = $state(false);
  let note = $state('');
  let phrases = $state('');
  let preview = $state<ReturnType<typeof prepareHandoff> | null>(null);
  let reviewed = $state(false), loading = $state(false), saving = $state(false);
  let message = $state('');
  let anchor = $state<string | null>(null);
  let generation = 0;
  let lastSessionId: string | null = null;
  $effect(() => {
    const current = session;
    void includeTools;
    if (current.storage_id !== lastSessionId) { lastSessionId = current.storage_id; note = ''; phrases = ''; }
    generation++; selection = retainedHandoff(current); selected = [];
    preview = null; reviewed = false; loading = false; saving = false; message = ''; anchor = null;
  });
  $effect(() => { void selected; void note; void phrases; preview = null; reviewed = false; message = ''; });
  onMount(() => { dialog.showModal(); return () => { generation++; }; });

  async function loadSource(): Promise<void> {
    const request = ++generation;
    preview = null; reviewed = false; loading = true; message = '';
    try {
      const source = await sourceHandoff(session.storage_id, includeTools, getTranscriptPage, () => request !== generation);
      if (request !== generation) return;
      const retained = retainedHandoff(session);
      selection = { records: [...retained.records, ...source.records], coverage: `${retained.coverage} ${source.coverage}` };
      selected = []; // Source replacement/read changes require explicit selection again.
    } catch { if (request === generation) message = 'Source read unavailable. Retained fields remain explicitly incomplete.'; }
    finally { if (request === generation) loading = false; }
  }
  function build(): void {
    try { preview = prepareHandoff(selection, selected, note, phrases.split(/\r?\n/).filter(Boolean)); reviewed = false; message = ''; }
    catch (reason) { preview = null; message = String(reason).replace(/^Error: /, ''); }
  }
  async function publish(format: 'copy' | 'html'): Promise<void> {
    if (!preview || !reviewed || saving) return;
    const content = preview;
    const request = generation;
    saving = true; message = '';
    try {
      if (format === 'copy') {
        await navigator.clipboard.writeText(content.markdown);
        if (request === generation) message = 'Copied the exact reviewed handoff.';
      } else {
        const saved = await writeExport('odometer-handoff.html', 'html', content.html);
        if (request === generation) message = saved ? 'Saved the exact reviewed handoff.' : 'Save canceled.';
      }
    } catch { if (request === generation) message = 'The handoff could not be copied or saved. No destination details were recorded.'; }
    finally { if (request === generation) saving = false; }
  }
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={(event) => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="handoff-heading">
  <header><h2 id="handoff-heading">Prepare handoff</h2><button type="button" onclick={() => dialog.close()} disabled={saving}>Close handoff</button></header>
  <div class="content">
    <p>Select up to 30 records from this session. No parent or subagent content is fetched, and nothing is run or uploaded. Retained fields may overlap source records.</p>
    <fieldset disabled={saving || loading}><legend>Original source</legend>
      <label><input type="checkbox" bind:checked={includeTools} /> Allow tool calls and results as selectable content</label>
      <button type="button" onclick={loadSource}>{loading ? 'Reading bounded source…' : 'Read source records'}</button>
    </fieldset>
    {#if loading}<button type="button" onclick={() => { generation++; loading = false; }}>Cancel read</button>{/if}
    <p class="notice">{selection.coverage}</p>
    <fieldset class="records" disabled={saving || loading}><legend>Records to include · {selected.length} selected</legend>
      {#each selection.records as record (record.key)}
        <div class="record">
          <label><input type="checkbox" value={record.key} bind:group={selected} disabled={!selected.includes(record.key) && selected.length >= 30} />
            <span><strong>{record.reference}</strong> · {record.role} · {record.timestamp ?? 'timestamp not recorded'}<small>{redactTranscriptText(record.text, []).text.slice(0, 180)}{record.text.length > 180 ? '…' : ''}</small>{#if record.truncated}<small>Text truncated at preparation limit.</small>{/if}</span>
          </label>
          {#if record.sourceAnchor}<button type="button" onclick={() => anchor = record.sourceAnchor}>Inspect exact source</button>{/if}
        </div>
      {:else}<p>No retained message fields are available. Read source records to check the original transcript.</p>{/each}
    </fieldset>
    <label class="text-input">User-written task state and next steps (optional)<textarea bind:value={note} maxlength="5000" rows="3" disabled={saving}></textarea></label>
    <label class="text-input">Also redact these exact phrases, one per line<textarea bind:value={phrases} maxlength="25700" rows="2" disabled={saving}></textarea></label>
    <p>Common credentials, paths, URLs, and encoded attachments are redacted. This cannot find every secret. Review the entire preview before sharing.</p>
    <button type="button" onclick={build} disabled={!selected.length || loading || saving}>Build handoff preview</button>
    {#if preview}
      <p>{preview.redactions} redactions. The HTML and copied Markdown contain the same selected handoff text.</p>
      <label><input type="checkbox" bind:checked={reviewed} disabled={saving} /> I reviewed every selected record and the handoff text.</label>
      <div class="actions"><button type="button" onclick={() => publish('copy')} disabled={!reviewed || saving}>Copy reviewed handoff</button><button type="button" onclick={() => publish('html')} disabled={!reviewed || saving}>Save reviewed handoff…</button></div>
      <iframe title="Exact handoff HTML preview" sandbox="" srcdoc={preview.html}></iframe>
      <label class="text-input">Exact handoff Markdown<textarea readonly value={preview.markdown} rows="10"></textarea></label>
    {/if}
    {#if message}<p role="status">{message}</p>{/if}
  </div>
</dialog>
{#if anchor}<TranscriptInspector sessionId={session.storage_id} recordId={anchor} onclose={() => anchor = null} />{/if}

<style>
  dialog { width: min(1000px, calc(100vw - 24px)); max-width: none; max-height: calc(100vh - 24px); margin: auto; padding: 0; border: 1px solid var(--border); border-radius: 10px; background: var(--panel); color: var(--text); }
  dialog::backdrop { background: #0008; }
  header { display: flex; justify-content: space-between; align-items: center; gap: 1rem; padding: 1rem; border-bottom: 1px solid var(--border); }
  h2 { font-size: 1rem; font-weight: 600; }
  .content { display: flex; flex-direction: column; align-items: flex-start; gap: .75rem; padding: 1rem; font-size: .75rem; }
  fieldset, .text-input, iframe { width: 100%; }
  fieldset { display: flex; flex-direction: column; gap: .5rem; padding: .5rem; border: 1px solid var(--border); border-radius: 4px; }
  label { display: flex; align-items: flex-start; gap: .5rem; }
  .text-input { display: block; }
  input { flex: 0 0 auto; margin-top: 2px; }
  textarea { display: block; width: 100%; margin-top: .375rem; padding: .5rem; resize: vertical; border: 1px solid var(--border); border-radius: 4px; color: var(--text); background: var(--bg); }
  .records { max-height: 20rem; overflow: auto; }
  .record { flex-shrink: 0; padding: .5rem 0; border-bottom: 1px solid var(--border); overflow-wrap: anywhere; }
  small { display: block; font-size: .6875rem; margin-top: .25rem; color: var(--muted); }
  .notice { border-left: 3px solid var(--accent); padding-left: .75rem; }
  button { border: 1px solid var(--border); border-radius: 4px; padding: .375rem .625rem; }
  button:disabled { opacity: .5; }
  button:focus-visible, input:focus-visible, textarea:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .actions { display: flex; flex-wrap: wrap; gap: .5rem; }
  iframe { min-height: 400px; border: 1px solid var(--border); background: white; }
</style>
