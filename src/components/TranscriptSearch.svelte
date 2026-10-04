<script lang="ts">
  import { onMount, tick } from 'svelte';
  import { searchSessionContent, resolveRetainedSearchTarget } from '../lib/ipc';
  import type { TranscriptSearchCursor, TranscriptSearchPage, TranscriptSearchHit, TranscriptSearchTarget, RetainedSearchLanding } from '../lib/types';
  import TranscriptInspector from './TranscriptInspector.svelte';
  let { sessionId, onclose }: { sessionId: string; onclose: () => void } = $props();
  let dialog: HTMLDialogElement;
  let landingElement = $state<HTMLElement>();
  let query = $state('');
  let conversation = $state(true);
  let toolCalls = $state(false);
  let toolResults = $state(false);
  let page = $state<TranscriptSearchPage | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let sourceTarget = $state<Extract<TranscriptSearchTarget, { kind: 'source_record' }> | null>(null);
  let landing = $state<RetainedSearchLanding | null>(null);
  let opening = $state(false);
  let landingError = $state(false);
  let generation = 0;
  const emptyScope = $derived(!conversation && !toolCalls && !toolResults);

  function invalidate(): void {
    generation++;
    page = null; loading = false; error = null;
    sourceTarget = null; landing = null; opening = false; landingError = false;
  }
  $effect(() => {
    void sessionId;
    query = ''; conversation = true; toolCalls = false; toolResults = false;
    invalidate();
    return () => { generation++; };
  });
  onMount(() => { dialog.showModal(); return () => { generation++; }; });

  async function search(cursor: TranscriptSearchCursor | null = null): Promise<void> {
    if (!query.trim() || emptyScope) return;
    const request = ++generation;
    page = null; landing = null; landingError = false; opening = false; error = null; loading = true;
    try {
      const result = await searchSessionContent({ session_id: sessionId, query, scope: { conversation, tool_calls: toolCalls, tool_results: toolResults }, cursor });
      if (request === generation) page = result;
    } catch {
      if (request === generation) error = 'The search could not finish. The source or retained snapshot may have changed. Search again from the start.';
    } finally { if (request === generation) loading = false; }
  }
  async function open(hit: TranscriptSearchHit): Promise<void> {
    if (hit.target.kind === 'source_record') { sourceTarget = hit.target; return; }
    const request = ++generation;
    opening = true; landing = null; landingError = false;
    try {
      const result = await resolveRetainedSearchTarget(hit.target);
      if (request !== generation) return;
      landing = result;
      await tick();
      if (request === generation) landingElement?.focus();
    } catch { if (request === generation) landingError = true; }
    finally { if (request === generation) opening = false; }
  }
  function label(kind: string): string { return kind.replaceAll('_', ' '); }
  function issueLabel(issue: string): string {
    const labels: Record<string, string> = {
      retained_prompt_and_final_reply_only: 'Only retained prompt and final reply fields are searched; these can be truncated',
      retained_matches_may_overlap_source: 'Retained matches are separate from source records and may overlap them',
      retained_fields_truncated: 'Retained fields may omit content after 500 characters; full fields were not searched',
      retained_snapshot_too_large: 'The retained snapshot exceeds the 8 MiB search limit and was not searched',
      retained_snapshot_unsupported: 'This retained snapshot format cannot be searched',
      retained_messages_unavailable: 'Retained messages are unavailable',
      retained_history_unavailable: 'Retained history is unavailable',
      unrecognized_content_not_searched: 'Unrecognized content was not searched',
      record_too_large: 'An oversized source record was not searched',
    };
    return labels[issue] ?? label(issue);
  }
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={(event) => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="content-search-heading">
  <div class="search">
    <header><div><h2 id="content-search-heading">Search session content</h2><p>Local, literal search · conversation messages selected by default</p></div><button type="button" onclick={() => dialog.close()}>Close search</button></header>
    <form onsubmit={(event) => { event.preventDefault(); void search(); }}>
      <label class="query">Find text <input bind:value={query} oninput={invalidate} maxlength="256" placeholder="Search this session’s messages" /></label>
      <fieldset><legend>Content to search</legend>
        <label><input type="checkbox" bind:checked={conversation} onchange={invalidate} /> Conversation messages</label>
        <label><input type="checkbox" bind:checked={toolCalls} onchange={invalidate} /> Tool call arguments</label>
        <label><input type="checkbox" bind:checked={toolResults} onchange={invalidate} /> Tool results and errors</label>
      </fieldset>
      <button type="submit" disabled={!query.trim() || emptyScope}>Search from start</button>
    </form>
    <div class="results" aria-busy={loading}>
      {#if loading}<p role="status">Searching a bounded content page…</p>{/if}
      {#if error}<p role="alert">{error}</p>{/if}
      {#if page}
        <h3>{page.phase === 'source' ? 'Source record matches' : 'Retained message matches'}</h3>
        <p>{page.phase === 'source' ? 'Matching records' : 'Matching retained fields'} on this page: {page.hits.length}. Examined {page.phase === 'source' ? `${page.scanned_records} source records` : `${page.scanned_messages} retained fields`}.</p>
        {#if page.issues.length}<p role="status">{page.issues.map(issueLabel).join('; ')}.</p>{/if}
        {#if page.hits.length === 0}<p>No matches in the examined {page.phase === 'source' ? 'source' : 'retained'} page.</p>{/if}
        {#each page.hits as hit, index (index)}
          <article>
            <button type="button" disabled={opening} onclick={() => open(hit)}>Open {hit.target.kind === 'source_record' ? 'source record' : 'retained message'} · {label(hit.content_kind)}</button>
            <pre>{hit.snippet.truncated_before ? '…' : ''}{hit.snippet.text.slice(0, hit.snippet.match_start)}<mark>{hit.snippet.text.slice(hit.snippet.match_start, hit.snippet.match_end)}</mark>{hit.snippet.text.slice(hit.snippet.match_end)}{hit.snippet.truncated_after ? '…' : ''}</pre>
          </article>
        {/each}
        {#if page.source_complete}<p>Reached the current source end for this search. Match counts describe each page, not every occurrence.</p>
        {:else}<p>Source coverage is partial. Unsearched or unavailable content may contain more matches.</p>{/if}
        {#if page.retained_complete}<p>Reached the end of retained prompt and final reply fields. Other messages and tool bodies are not retained.</p>{/if}
        {#if page.next_cursor}<button type="button" disabled={opening} onclick={() => search(page!.next_cursor)}>Search {page.next_cursor.position.phase === 'retained' && page.phase === 'source' ? 'retained messages' : 'next page'}</button>{/if}
      {/if}
      {#if opening}<p role="status">Opening the exact retained message…</p>{/if}
      {#if landingError}<p role="alert">This retained message is no longer available in the reviewed snapshot. Search again; no other message was selected.</p>{/if}
      {#if landing}
        <section bind:this={landingElement} tabindex="-1" class="landing" aria-label="Selected retained message">
          <h3>Retained {landing.target.kind === 'retained_turn' ? label(landing.target.field) : 'message'}</h3>
          <p>Snapshot field · source content may be missing{landing.truncated ? ' · text may be truncated' : ''}</p><pre>{landing.text}</pre>
        </section>
      {/if}
    </div>
  </div>
</dialog>
{#if sourceTarget}<TranscriptInspector sessionId={sourceTarget.session_id} recordId={sourceTarget.record_id} blockIndex={sourceTarget.block_index} onclose={() => { sourceTarget = null; }} />{/if}

<style>
  dialog { width: min(960px, calc(100vw - 32px)); height: min(820px, calc(100vh - 32px)); max-width: none; max-height: none; margin: auto; padding: 0; overflow: hidden; border: 1px solid var(--border); border-radius: 12px; background: var(--panel); color: var(--text); }
  dialog::backdrop { background: #0009; }
  .search { padding: 20px; height: 100%; min-height: 0; display: flex; flex-direction: column; }
  .results { overflow: auto; min-height: 0; flex: 1; }
  header { display: flex; justify-content: space-between; align-items: start; gap: 16px; flex-shrink: 0; }
  h2 { font-size: 18px; font-weight: 600; } h3 { font-size: 14px; font-weight: 600; }
  p { margin: 8px 0; font-size: 12px; color: var(--muted); }
  form { display: grid; gap: 12px; margin: 16px 0; flex-shrink: 0; }
  .query { display: grid; gap: 6px; font-size: 13px; }
  input:not([type]) { background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: 8px; }
  fieldset { display: flex; gap: 14px; flex-wrap: wrap; font-size: 12px; } legend { margin-bottom: 6px; color: var(--muted); } fieldset label { display: flex; gap: 6px; align-items: center; }
  button { font-size: 12px; color: var(--accent); background: var(--card); border: 1px solid var(--border); padding: 6px 10px; border-radius: 6px; width: fit-content; } button:disabled { opacity: .45; } button:hover:enabled { background: var(--panel); }
  article, .landing { background: var(--card); border: 1px solid var(--border); padding: 12px; margin: 12px 0; border-radius: 8px; }
  pre { white-space: pre-wrap; overflow-wrap: anywhere; font-size: 12px; margin: 10px 0 0; font-family: inherit; }
  mark { background: #a68e2440; color: inherit; }
  .landing:focus, button:focus-visible, input:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  @media (max-width: 600px) { .search { padding: 14px; } header { flex-wrap: wrap; } fieldset { flex-direction: column; gap: 8px; } }
</style>
