<script lang="ts">
  import { onMount } from 'svelte';
  import { getTranscriptPage } from '../lib/ipc';
  import type { TranscriptCursor, TranscriptPage } from '../lib/types';
  let { sessionId, onclose, oninspect }: { sessionId: string; onclose: () => void; oninspect: (anchor: string) => void } = $props();
  let dialog: HTMLDialogElement;
  let page = $state<TranscriptPage | null>(null);
  let loading = $state(false);
  let error = $state(false);
  let generation = 0;
  async function read(cursor: TranscriptCursor | null = null): Promise<void> {
    const request = ++generation;
    loading = true; error = false;
    try {
      const result = await getTranscriptPage({ session_id: sessionId, cursor, max_records: 25, max_bytes: 131_072 });
      if (request === generation) page = result;
    } catch { if (request === generation) { page = null; error = true; } }
    finally { if (request === generation) loading = false; }
  }
  onMount(() => { dialog.showModal(); return () => { generation++; }; });
  $effect(() => { void sessionId; page = null; void read(); return () => { generation++; }; });
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={(event) => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="context-heading">
  <header><h2 id="context-heading">Context explanation</h2><button type="button" onclick={() => dialog.close()}>Close explanation</button></header>
  <p>Provider usage totals remain authoritative. This view lists recorded evidence in the current source page; it does not estimate token shares, per-file tokens, or reconstruct the model's complete context.</p>
  <p>Method: classify recorded message roles and tool block types; identify explicit compaction boundaries and per-call usage fields. Repeated source records may describe the same message. A tool call or skill invocation does not prove its full content entered context. Instructions, skills, and references without explicit records are unavailable.</p>
  <nav aria-label="Context pages"><button type="button" disabled={loading} onclick={() => read()}>Reload from start</button><button type="button" disabled={loading || !page?.next_cursor} onclick={() => page?.next_cursor && read(page.next_cursor)}>Next evidence page</button></nav>
  <div aria-busy={loading}>
    {#if loading}<p role="status">Loading context evidence…</p>{/if}
    {#if error}<p role="alert">Context evidence could not be read. Retry or reopen this view.</p>{/if}
    {#if page}
      <p role="status">Coverage: {page.records.length} source records in this page · {page.availability.replaceAll('_', ' ')}. {page.source_complete ? 'Reached current source end; this is still page evidence, not complete context reconstruction.' : 'Partial history: earlier, later, or omitted records may not be represented.'}</p>
      {#if page.issues.length}<p role="status">Source gaps: {page.issues.map(issue => issue.replaceAll('_', ' ')).join('; ')}.</p>{/if}
      {#if !page.records.length}<p>No source evidence available. Missing values are unavailable, never zero.</p>{/if}
      {#each page.records as record (record.id)}
        <article>
          <button type="button" onclick={() => oninspect(record.id)}>Inspect source record</button>
          <code>{record.id}</code>
          {#if record.issue}<p>Source gap: {record.issue.replaceAll('_', ' ')}.</p>{/if}
          {#if record.context_evidence}
            {@const evidence = record.context_evidence}
            {#if evidence.compaction}<h3>Recorded compaction boundary</h3><p>Recorded pre-compaction tokens: {evidence.pre_compaction_tokens ?? 'unavailable'}. Post-compaction context size: unavailable. Adjacent usage records are observations, not proof of a compaction reduction.</p>{/if}
            {#if evidence.contributors.length}<p>Recorded contributors: {evidence.contributors.join(', ')}. Token attribution unavailable.</p>{/if}
            {#if evidence.input_tokens !== null || evidence.output_tokens !== null || evidence.context_window !== null}
              <p>Recorded per-call input: {evidence.input_tokens ?? 'unavailable'} · output: {evidence.output_tokens ?? 'unavailable'} · context window: {evidence.context_window ?? 'unavailable'}.</p>
            {/if}
            {#if evidence.unsupported}<p>Context interpretation unavailable for this record type.</p>{/if}
          {:else}<p>Context evidence unavailable for this record.</p>{/if}
        </article>
      {/each}
      <p>No compaction marker in this page does not prove compaction never occurred.</p>
    {/if}
  </div>
</dialog>

<style>
  dialog { width: min(900px, calc(100vw - 32px)); max-height: calc(100vh - 32px); overflow: auto; background: var(--panel); color: var(--text); border: 1px solid var(--border); border-radius: 8px; padding: 20px; }
  dialog::backdrop { background: #0009; }
  header, nav { display: flex; gap: 16px; justify-content: space-between; flex-wrap: wrap; }
  h2 { font-size: 18px; } h3 { font-size: 15px; }
  p { margin: 12px 0; font-size: 13px; } button { color: var(--accent); border: 1px solid var(--border); background: var(--card); border-radius: 4px; padding: 4px 8px; font-size: 13px; } button:disabled { opacity: .45; }
  button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  article { border-top: 1px solid var(--border); padding: 12px 0; } code { display: block; overflow-wrap: anywhere; font-size: 11px; margin-top: 6px; }
</style>
