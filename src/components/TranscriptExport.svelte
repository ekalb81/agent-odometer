<script lang="ts">
  import { onMount } from 'svelte';
  import { getTranscriptPage, writeExport } from '../lib/ipc';
  import { previewTranscriptExport, type TranscriptExportPreview } from '../lib/transcriptExport';
  let { sessionId, onclose }: { sessionId: string; onclose: () => void } = $props();
  let dialog: HTMLDialogElement;
  let toolCalls = $state(false), toolResults = $state(false), reasoning = $state(false);
  let phrases = $state('');
  let preview = $state<TranscriptExportPreview | null>(null);
  let reviewed = $state(false), loading = $state(false), saving = $state(false);
  let message = $state('');
  let generation = 0;
  $effect(() => {
    void sessionId; void toolCalls; void toolResults; void reasoning; void phrases;
    generation++; preview = null; reviewed = false; loading = false; message = '';
  });
  onMount(() => { dialog.showModal(); return () => { generation++; preview = null; }; });
  async function prepare() {
    const request = ++generation;
    preview = null; reviewed = false; loading = true; message = '';
    try {
      const result = await previewTranscriptExport(sessionId, { toolCalls, toolResults, reasoning, redactPhrases: phrases.split(/\r?\n/).filter(Boolean) }, getTranscriptPage, () => request !== generation);
      if (request === generation) preview = result;
    } catch {
      if (request === generation) message = 'Preview unavailable. Use at most 100 redaction phrases of 256 characters each, then try again.';
    } finally { if (request === generation) loading = false; }
  }
  async function save() {
    if (!preview || !reviewed || saving) return;
    const html = preview.html;
    const request = generation;
    saving = true; message = '';
    try {
      const saved = await writeExport('odometer-transcript.html', 'html', html);
      if (request === generation) message = saved ? 'Saved the exact preview to your selected file.' : 'Save cancelled.';
    } catch { if (request === generation) message = 'The export could not be saved. No destination details were recorded.'; }
    finally { saving = false; }
  }
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={(event) => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="export-transcript-heading">
  <header><h2 id="export-transcript-heading">Export transcript</h2><button type="button" onclick={() => dialog.close()} disabled={saving}>Close export</button></header>
  <div class="controls">
    <p>Create a local HTML excerpt. Tool payloads are excluded by default; attachments and unknown source records are always omitted.</p>
    <fieldset disabled={saving}><legend>Include additional content</legend>
      <label><input type="checkbox" bind:checked={toolCalls} /> Tool calls and arguments</label>
      <label><input type="checkbox" bind:checked={toolResults} /> Tool results and errors</label>
      <label><input type="checkbox" bind:checked={reasoning} /> Recorded reasoning</label>
    </fieldset>
    <label class="phrases">Also redact these exact phrases, one per line<textarea bind:value={phrases} maxlength="25700" rows="2" disabled={saving} placeholder="Names, project details, or other private text"></textarea></label>
    <p>Common credential fields, local paths, URLs, and encoded attachments are redacted automatically. This cannot find every secret. Review the full preview before saving or sharing. No content is uploaded.</p>
    <button type="button" onclick={prepare} disabled={loading || saving}>{loading ? 'Reading bounded source pages…' : 'Build preview'}</button>
    {#if loading}<button type="button" onclick={() => { generation++; loading = false; }}>Cancel preview</button>{/if}
    {#if preview}
      <p role="status">{`${preview.complete ? 'Source read to its end.' : 'Incomplete source excerpt.'} ${preview.records} records read; ${preview.omitted} records or blocks omitted; ${preview.redactions} redactions.${preview.limited ? ' Safety limit reached.' : ''}`}</p>
      <label><input type="checkbox" bind:checked={reviewed} disabled={saving} /> I reviewed every included section for sensitive content.</label>
      <button type="button" onclick={save} disabled={!reviewed || saving}>{saving ? 'Saving…' : 'Save reviewed HTML…'}</button>
    {/if}
    {#if message}<p role="status">{message}</p>{/if}
  </div>
  {#if preview}<iframe title="Exact transcript export preview" sandbox="" srcdoc={preview.html}></iframe>{/if}
</dialog>

<style>
  dialog { width: min(1020px, calc(100vw - 24px)); height: min(900px, calc(100vh - 24px)); max-width: none; max-height: none; margin: auto; padding: 0; border: 1px solid var(--border); border-radius: 10px; background: var(--panel); color: var(--text); }
  dialog::backdrop { background: #0008; }
  header { display: flex; justify-content: space-between; align-items: center; padding: 16px; border-bottom: 1px solid var(--border); }
  h2 { font-size: 1rem; font-weight: 600; }
  .controls { padding: 16px; display: flex; gap: 10px; flex-wrap: wrap; font-size: 12px; }
  p, fieldset, .phrases { width: 100%; }
  fieldset { display: flex; flex-wrap: wrap; gap: 12px; }
  label { display: flex; gap: 6px; align-items: center; }
  .phrases { display: block; }
  textarea { display: block; width: 100%; margin-top: 4px; border: 1px solid var(--border); border-radius: 4px; padding: 6px; background: var(--bg); }
  button { border: 1px solid var(--border); border-radius: 4px; padding: 5px 10px; }
  button:disabled { opacity: .5; }
  iframe { display: block; width: calc(100% - 32px); min-height: 450px; margin: 0 16px 16px; background: white; border: 1px solid var(--border); }
</style>
