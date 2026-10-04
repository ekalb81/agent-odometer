<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { getTranscriptPage } from '../lib/ipc';
  import type { TranscriptCursor, TranscriptPage, TranscriptRecord } from '../lib/types';
  let { sessionId, recordId = null, blockIndex = null, onclose }: { sessionId: string; recordId?: string | null; blockIndex?: number | null; onclose: () => void } = $props();
  let dialog: HTMLDialogElement;
  let page = $state<TranscriptPage | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let anchor = $state('');
  let selected = $state<string | null>(null);
  let expanded = $state(new Set<string>());
  type Location = { cursor: TranscriptCursor | null; recordId: string | null };
  let currentLocation: Location = { cursor: null, recordId: null };
  let previous = $state<Location[]>([]);
  let generation = 0;
  // Only anchor metadata crosses pages. Raw bodies remain bounded to this page.
  let toolAnchors = $state(new Map<string, { calls: string[]; results: string[] }>());
  const unavailable = $derived(page && !['available', 'partial'].includes(page.availability));

  async function read(cursor: TranscriptCursor | null, target: string | null = null, remember = false): Promise<void> {
    const request = ++generation;
    loading = true; error = null;
    try {
      const result = await getTranscriptPage({ session_id: sessionId, cursor, record_id: target, max_records: 25, max_bytes: 131_072 });
      if (request !== generation) return;
      if (remember) previous = [...previous.slice(-99), currentLocation];
      currentLocation = { cursor, recordId: target };
      page = result;
      const next = new Map(toolAnchors);
      for (const record of result.records) {
        for (const block of record.presentation?.blocks ?? []) {
          if (!block.call_id || !['tool_call', 'tool_result', 'tool_error'].includes(block.kind)) continue;
          if (!next.has(block.call_id) && next.size >= 500) continue;
          const entry = next.get(block.call_id) ?? { calls: [], results: [] };
          const field = block.kind === 'tool_call' ? 'calls' : 'results';
          if (!entry[field].includes(record.id)) entry[field] = [...entry[field].slice(-3), record.id];
          next.set(block.call_id, entry);
        }
      }
      toolAnchors = next;
      if (target) { selected = target; expanded = new Set([target]); }
      else { expanded = new Set([...expanded].filter(id => result.records.some(record => record.id === id))); }
      await tick();
      if (request === generation && target) {
        const block = target === recordId && blockIndex !== null ? document.getElementById(`transcript-${target}-block-${blockIndex}`) : null;
        (block ?? document.getElementById(`transcript-${target}`))?.focus();
      }
    } catch { if (request === generation) { error = 'The transcript could not be read. Retry or reopen it.'; page = null; } }
    finally { if (request === generation) loading = false; }
  }
  onMount(() => { dialog.showModal(); return () => { generation++; }; });
  $effect(() => {
    const key = sessionId;
    const target = recordId;
    void key;
    page = null; anchor = target ?? '';
    previous = []; currentLocation = { cursor: null, recordId: null }; toolAnchors = new Map(); expanded = new Set(); selected = target;
    void read(null, target);
    return () => { generation++; };
  });
  async function jump(id: string): Promise<void> {
    if (!id.trim() || loading) return;
    anchor = id.trim();
    await read(null, anchor, true);
  }
  function toggle(id: string): void {
    selected = id;
    const next = new Set(expanded);
    if (next.has(id)) next.delete(id); else next.add(id);
    expanded = next;
  }
  function back(): void {
    const destination = previous.at(-1) ?? { cursor: null, recordId: null };
    previous = previous.slice(0, -1);
    void read(destination.cursor, destination.recordId);
  }
  function counterpart(callId: string, kind: string): string | null {
    const matches = toolAnchors.get(callId)?.[kind === 'tool_call' ? 'results' : 'calls'] ?? [];
    return matches.length === 1 ? matches[0] : null;
  }
  function title(record: TranscriptRecord): string {
    return record.presentation?.role ?? record.kind ?? 'Source record';
  }
  function issueLabel(value: string): string { return value.replaceAll('_', ' '); }
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={(event) => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="transcript-heading">
  <div class="inspector">
    <header>
      <div><h2 id="transcript-heading">Transcript inspector</h2><p>Source order · loaded only while this inspector is open</p></div>
      <button type="button" onclick={() => dialog.close()}>Close inspector</button>
    </header>
    <form onsubmit={(event) => { event.preventDefault(); void jump(anchor); }}>
      <label>Record anchor <input bind:value={anchor} placeholder="Paste a record anchor" maxlength="1024" /></label>
      <button type="submit" disabled={loading || !anchor.trim()}>Jump to record</button>
    </form>
    <div class="toolbar">
      <button type="button" disabled={loading || previous.length === 0} onclick={back}>Previous page</button>
      <button type="button" disabled={loading || !page?.next_cursor} onclick={() => page?.next_cursor && read(page.next_cursor, null, true)}>Next page</button>
      <button type="button" disabled={loading} onclick={() => { previous = []; toolAnchors = new Map(); void read(null); }}>Reload from start</button>
      {#if selected && !page?.records.some(record => record.id === selected)}<button type="button" disabled={loading} onclick={() => selected && jump(selected)}>Return to selected record</button>{/if}
    </div>
    <div class="records" aria-busy={loading}>
      {#if loading}<p role="status">Loading transcript page…</p>{/if}
      {#if error}<p role="alert">{error}</p>{/if}
      {#if unavailable}<p role="status">Transcript {issueLabel(page!.availability)}. Retained usage totals do not guarantee the source content is still available.</p>{/if}
      {#if page?.issues.length}<p role="status">Source limitations: {page.issues.map(issueLabel).join('; ')}. No missing content is reconstructed.</p>{/if}
      {#if page?.availability === 'partial'}<p role="status">This transcript view is partial. Earlier or omitted records may not be shown; inspect the source limitations and marked records.</p>{/if}
      {#if page && !loading && !unavailable && page.records.length === 0}<p>No records in this page.</p>{/if}
      {#each page?.records ?? [] as record (record.id)}
        <article id={`transcript-${record.id}`} tabindex="-1" class:selected={selected === record.id}>
          <div class="record-heading">
            <button type="button" aria-expanded={expanded.has(record.id)} onclick={() => toggle(record.id)}>{expanded.has(record.id) ? 'Collapse' : 'Expand'} {title(record)}</button>
            <span>{record.presentation?.timestamp ?? 'Timestamp not recorded'}</span>
          </div>
          <p class="anchor"><button type="button" onclick={() => { selected = record.id; anchor = record.id; }}>Select anchor</button> <code>{record.id}</code></p>
          {#if record.issue}<p role="status">{issueLabel(record.issue)} · payload unavailable</p>{/if}
          {#if expanded.has(record.id)}
            {#each record.presentation?.blocks ?? [] as block, index (index)}
              <section class="block" id={`transcript-${record.id}-block-${index}`} tabindex="-1">
                <h3>{issueLabel(block.kind)}{block.name ? ` · ${block.name}` : ''}</h3>
                {#if block.call_id}
                  <p>Call ID <code>{block.call_id}</code>
                    {#if counterpart(block.call_id, block.kind)}
                      <button type="button" disabled={loading} onclick={() => jump(counterpart(block.call_id!, block.kind)!)}>Jump to tool {block.kind === 'tool_call' ? 'result' : 'call'}</button>
                    {:else}<span> · Matching record is not uniquely identified in the pages visited.</span>{/if}
                  </p>
                {/if}
                {#if block.edit}
                  <p>Recorded replacement{block.edit.path ? ` · ${block.edit.path}` : ''}</p>
                  <div class="replacement"><div><h4>Before</h4><pre>{block.edit.before}</pre></div><div><h4>After</h4><pre>{block.edit.after}</pre></div></div>
                {:else}<pre>{block.text || 'No presentation text. Inspect the raw source record for recorded fields.'}</pre>{/if}
              </section>
            {/each}
            <details><summary>Raw source record</summary><pre>{record.raw_json ?? 'Raw payload unavailable'}</pre></details>
          {/if}
        </article>
      {/each}
      {#if page?.source_complete}<p>Reached the current source end. Earlier pages may contain marked omissions.</p>
      {:else if page?.next_cursor}<p>More source records are available. Tool pairing is limited to pages visited.</p>{/if}
    </div>
  </div>
</dialog>

<style>
  dialog { width: min(960px, calc(100vw - 24px)); height: min(820px, calc(100vh - 24px)); max-width: none; max-height: none; margin: auto; padding: 0; border: 1px solid var(--border); border-radius: 10px; background: var(--panel); color: var(--text); }
  dialog::backdrop { background: #0008; }
  .inspector { display: flex; flex-direction: column; height: 100%; min-width: 0; font-size: 12px; }
  header, form, .toolbar { display: flex; align-items: center; flex-wrap: wrap; gap: 8px; padding: 10px 14px; border-bottom: 1px solid var(--border); }
  header { justify-content: space-between; }
  h2 { font-weight: 600; font-size: 16px; } h3, h4 { font-weight: 600; }
  p { margin: 5px 0; } header p, .record-heading span, .anchor { color: var(--muted); font-size: 11px; }
  label { display: flex; align-items: center; gap: 8px; flex: 1; min-width: 180px; }
  input { min-width: 0; flex: 1; padding: 5px; border: 1px solid var(--border); border-radius: 4px; }
  button { color: var(--accent); border: 1px solid var(--border); background: var(--card); border-radius: 4px; padding: 4px 8px; } button:disabled { opacity: .45; }
  button:focus-visible, input:focus-visible, article:focus-visible, .block:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .records { overflow: auto; padding: 12px; min-height: 0; }
  article { padding: 10px; border: 1px solid var(--border); border-radius: 6px; margin-bottom: 8px; background: var(--card); }
  article.selected { border-color: var(--accent); }
  .record-heading { display: flex; align-items: center; justify-content: space-between; flex-wrap: wrap; gap: 8px; }
  .anchor, code { overflow-wrap: anywhere; }
  .block { border-top: 1px solid var(--border); margin-top: 8px; padding-top: 8px; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; font-family: var(--font-mono); margin-top: 6px; padding: 8px; background: var(--panel); max-height: 400px; overflow: auto; }
  .replacement { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 8px; } summary { cursor: pointer; margin-top: 8px; }
  @media (max-width: 600px) { .replacement { grid-template-columns: minmax(0, 1fr); } }
</style>
