<script lang="ts">
  import { onMount } from 'svelte';
  import { writeExport } from '../lib/ipc';
  let { svg, markdown, onclose }: { svg: string; markdown: string; onclose: () => void } = $props();
  let dialog: HTMLDialogElement;
  let saving = $state(false);
  let status = $state('');
  let alive = true;
  onMount(() => { dialog.showModal(); return () => { alive = false; }; });
  async function save(): Promise<void> {
    saving = true; status = '';
    try {
      const saved = await writeExport('odometer-activity.svg', 'svg', svg);
      if (alive) status = saved ? 'Saved the exact SVG preview to your selected file.' : 'Save canceled.';
    } catch { if (alive) status = 'The summary could not be saved.'; }
    finally { if (alive) saving = false; }
  }
  async function copy(): Promise<void> {
    try { await navigator.clipboard.writeText(markdown); if (alive) status = 'Copied the exact Markdown shown below.'; }
    catch { if (alive) status = 'Copy unavailable. Select and copy the Markdown below.'; }
  }
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={(event) => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="activity-summary-heading">
  <header><h2 id="activity-summary-heading">Preview activity summary</h2><button type="button" onclick={() => dialog.close()}>Close summary</button></header>
  <div class="content">
    <p>This local snapshot uses the calendar's current range, provider, project, and session filters. Project names and session identities are omitted. Nothing is published or uploaded.</p>
    <div class="svg-preview"><img alt="Exact local SVG activity summary preview" src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`} /></div>
    <div class="actions"><button type="button" disabled={saving} onclick={save}>{saving ? 'Saving…' : 'Save SVG…'}</button><button type="button" onclick={copy}>Copy Markdown</button></div>
    <p role="status">{status}</p>
    <label>Companion Markdown<textarea readonly value={markdown} aria-label="Companion Markdown" spellcheck="false"></textarea></label>
  </div>
</dialog>

<style>
  dialog { width: min(840px, calc(100vw - 24px)); max-width: none; max-height: calc(100vh - 24px); margin: auto; padding: 0; border: 1px solid var(--border); border-radius: 10px; background: var(--panel); color: var(--text); }
  dialog::backdrop { background: #0008; }
  header { display: flex; align-items: center; justify-content: space-between; gap: 1rem; padding: 1rem; border-bottom: 1px solid var(--border); }
  h2 { font-size: 1rem; font-weight: 600; }
  .content { padding: 1rem; font-size: 0.75rem; }
  .svg-preview { overflow: auto; margin: 1rem 0; border: 1px solid var(--border); border-radius: 6px; }
  img { display: block; width: 100%; min-width: 700px; height: auto; }
  button { border: 1px solid var(--border); border-radius: 4px; padding: 0.375rem 0.625rem; background: var(--panel); }
  button:disabled { opacity: .5; }
  button:focus-visible, textarea:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .actions { display: flex; flex-wrap: wrap; gap: 0.5rem; }
  [role='status'] { min-height: 1.25rem; margin: .5rem 0; }
  textarea { display: block; width: 100%; min-height: 12rem; margin-top: .375rem; padding: .5rem; resize: vertical; border: 1px solid var(--border); background: var(--panel); color: var(--text); font-family: monospace; }
</style>
