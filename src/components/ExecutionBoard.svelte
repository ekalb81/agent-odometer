<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { getExecutionPage, getSessionPricing } from '../lib/ipc';
  import { rates } from '../lib/stores/rates';
  import { projectStore } from '../lib/stores/projects.svelte';
  import { formatCredits, harnessCurrency } from '../lib/currency';
  import { activityBlocks, parentRelations, timestamp, MAX_BOARD_RECORDS, MAX_BOARD_SESSIONS } from '../lib/executionBoard';
  import type { ExecutionPage, ExecutionRecord, SessionSummary, SummaryPricing } from '../lib/types';
  import TranscriptInspector from './TranscriptInspector.svelte';
  let { sessions, initialId, onclose }: { sessions: SessionSummary[]; initialId: string; onclose: () => void } = $props();
  let dialog: HTMLDialogElement;
  let selected = $state<string[]>(untrack(() => [initialId]));
  let provider = $state(''); let project = $state(''); let candidatePage = $state(0);
  let pages = $state<Record<string, ExecutionPage>>({});
  let records = $state<Record<string, ExecutionRecord[]>>({});
  let busy = $state<string[]>([]); let errors = $state<Record<string, string>>({});
  let prices = $state<Record<string, SummaryPricing>>({});
  let targets = $state<{ sessionId: string; recordId: string | null }[]>([]);
  let generation = 0;
  const chosen = $derived(sessions.filter(session => selected.includes(session.storage_id)));
  const candidates = $derived(sessions.filter(session => (!provider || session.harness === provider) && (!project || projectKey(session) === project)));
  const projects = $derived([...new Map(sessions.filter(session => projectKey(session)).map(session => [projectKey(session)!, projectLabel(session)])).entries()]);
  const visibleCandidates = $derived(candidates.slice(candidatePage * 25, candidatePage * 25 + 25));
  const parents = $derived(parentRelations(chosen));
  const bounds = $derived.by(() => {
    const times = chosen.flatMap(session => [timestamp(session.started_at), timestamp(session.last_event_at)]).filter((value): value is number => value !== null);
    return times.length ? { start: Math.min(...times), end: Math.max(...times) } : null;
  });
  // Value signature prevents unrelated streaming sessions from refetching this selection.
  const signature = $derived(JSON.stringify(chosen.map(session => ({
    id: session.storage_id, updated: session.last_event_at,
    availability: session.source_availability, lifecycle: session.lifecycle,
    tokens: session.tokens_total, buckets: session.buckets,
    project: projectKey(session), parent: session.parent_thread_id,
  }))));
  onMount(() => { dialog.showModal(); return () => { generation++; }; });
  $effect(() => {
    void signature; const rateCard = $rates;
    const ids = untrack(() => chosen.map(session => session.storage_id));
    const request = ++generation;
    pages = {}; records = {}; errors = {}; prices = {}; busy = ids; targets = [];
    untrack(() => { for (const id of ids) void load(id, request, false); });
    if (ids.length && rateCard) void getSessionPricing(ids).then(result => { if (request === generation) prices = result; }).catch(() => {});
    return () => { generation++; };
  });
  async function load(id: string, request = generation, more = false) {
    if (!busy.includes(id)) busy = [...busy, id];
    try {
      const page = await getExecutionPage({ session_id: id, cursor: more ? pages[id]?.next_cursor : null, max_records: 25, max_bytes: 131_072 });
      if (request !== generation || !selected.includes(id)) return;
      pages = { ...pages, [id]: page };
      const prior = more && ['available', 'partial'].includes(page.availability) ? records[id] ?? [] : [];
      records = { ...records, [id]: [...prior, ...page.records].slice(0, MAX_BOARD_RECORDS) };
      errors = { ...errors, [id]: '' };
    } catch { if (request === generation) errors = { ...errors, [id]: 'Execution metadata unavailable. Retry this session.' }; }
    finally { if (request === generation) busy = busy.filter(value => value !== id); }
  }
  function toggle(id: string) {
    selected = selected.includes(id) ? selected.filter(value => value !== id) : selected.length < MAX_BOARD_SESSIONS ? [...selected, id] : selected;
  }
  function inspect(sessionId: string, recordId: string | null) {
    const existing = targets.findIndex(target => target.sessionId === sessionId);
    if (existing >= 0) targets = targets.map((target, index) => index === existing ? { sessionId, recordId } : target);
    else targets = [...targets.slice(-1), { sessionId, recordId }];
  }
  function label(session: SessionSummary | undefined) { return session?.thread_name ?? session?.first_user_message?.slice(0, 60) ?? session?.agent_nickname ?? session?.id ?? 'Session unavailable'; }
  function projectKey(session: SessionSummary) { return projectStore.forSession(session)?.project_key ?? session.project_key; }
  function projectLabel(session: SessionSummary) { return projectStore.forSession(session)?.label ?? session.project_label ?? 'Project unavailable'; }
  function position(time: number) { return bounds ? Math.max(0, Math.min(100, (time - bounds.start) / Math.max(1, bounds.end - bounds.start) * 100)) : 0; }
  function utc(time: number) { return new Date(time).toISOString(); }
  function parentLabel(session: SessionSummary) {
    if (!session.parent_thread_id) return 'No recorded parent';
    const parent = chosen.find(item => item.storage_id === parents.get(session.storage_id));
    return parent && timestamp(parent.started_at) !== null && timestamp(session.started_at) !== null ? `Recorded parent: ${label(parent)}` : 'Parent relationship unresolved in selection';
  }
  function depth(session: SessionSummary) {
    let current = session; let count = 0;
    // Relations already reject cycles; the cap also bounds traversal if metadata changes.
    while (count < MAX_BOARD_SESSIONS) {
      const parent = chosen.find(item => item.storage_id === parents.get(current.storage_id));
      if (!parent || timestamp(parent.started_at) === null || timestamp(current.started_at) === null) break;
      count++; current = parent;
    }
    return count;
  }
</script>

<dialog bind:this={dialog} onclose={onclose} onkeydown={(event) => { if (event.key === 'Escape') event.stopPropagation(); }} aria-labelledby="execution-heading">
  <header><div><h2 id="execution-heading">Execution board</h2><p>Local, read-only comparison · up to 8 sessions</p></div><button onclick={() => dialog.close()}>Close board</button></header>
  <div class="body">
    <p>Activity is recorded source timing, not productivity. Session spans are elapsed time; tool intervals require unique call/result links and recorded timestamps. Usage and estimates below are cumulative for each session, independent of this timeline.</p>
    <details><summary>Select sessions ({selected.length}/8)</summary>
      <div class="controls">
        <label>Provider <select bind:value={provider} onchange={() => { candidatePage = 0; }}><option value="">All providers</option>{#each [...new Set(sessions.map(session => session.harness))] as harness}<option value={harness}>{harness}</option>{/each}</select></label>
        <label>Project <select bind:value={project} onchange={() => { candidatePage = 0; }}><option value="">All projects</option>{#each projects as [key, name]}<option value={key}>{name}</option>{/each}</select></label>
        <button onclick={() => { selected = []; }}>Clear selection</button>
      </div>
      <p>Filters affect the candidate list. Selected sessions remain visible until removed.</p>
      {#each visibleCandidates as session (session.storage_id)}<label class="candidate"><input type="checkbox" checked={selected.includes(session.storage_id)} disabled={!selected.includes(session.storage_id) && selected.length >= MAX_BOARD_SESSIONS} onchange={() => toggle(session.storage_id)} />{label(session)} · {session.harness}</label>{/each}
      <div class="controls"><button disabled={candidatePage === 0} onclick={() => { candidatePage--; }}>Previous candidates</button><span>{candidates.length} matching sessions · page {candidatePage + 1}</span><button disabled={(candidatePage + 1) * 25 >= candidates.length} onclick={() => { candidatePage++; }}>Next candidates</button></div>
    </details>
    {#if !chosen.length}<p>Select sessions to compare. Source records are loaded only for your selection.</p>{/if}
    {#if bounds}<p class="axis">Shared UTC axis: {utc(bounds.start)} → {utc(bounds.end)}</p>{/if}
    {#each chosen as session (session.storage_id)}
      {@const page = pages[session.storage_id]}
      {@const blocks = activityBlocks(records[session.storage_id] ?? [])}
      {@const observedBlockCount = (records[session.storage_id] ?? []).reduce((count, record) => count + Math.max(1, record.blocks.filter(block => block.kind === 'tool_call').length), 0)}
      {@const price = prices[session.storage_id]?.pricing.plan}
      <section class="lane" style:margin-left={`${Math.min(3, depth(session)) * 10}px`} aria-label={`Execution of ${label(session)}`}>
        <div class="lane-heading"><h3>{depth(session) ? '↳ ' : ''}{label(session)} · {session.harness}</h3><div><button onclick={() => inspect(session.storage_id, blocks[0]?.recordId ?? null)}>Inspect / compare</button><button onclick={() => toggle(session.storage_id)}>Remove</button></div></div>
        <p>{parentLabel(session)} · {projectLabel(session)}</p>
        <p>Usage: {session.tokens_total.total_tokens.toLocaleString()} tokens · priced plan estimate: {price && $rates ? formatCredits(price.total, harnessCurrency($rates, session.harness)) : 'unavailable'}{#if price && (price.missing_models.length || price.unpriced_models.length)} · partial estimate{/if}</p>
        <p>Elapsed session span: {session.started_at} → {session.last_event_at}; gaps inside the span are not measured active work.</p>
        {#if busy.includes(session.storage_id)}<p role="status">Loading bounded metadata…</p>{/if}
        {#if errors[session.storage_id]}<p role="alert">{errors[session.storage_id]}</p>{/if}
        {#if page}<p>Source: {page.availability.replaceAll('_', ' ')} · {records[session.storage_id]?.length ?? 0} visited records · {page.source_complete ? 'current source end reached; earlier gaps remain marked' : 'partial coverage; unvisited records are excluded'}</p>{#each page.issues as issue}<p>{issue.replaceAll('_', ' ')}</p>{/each}{/if}
        <div class="timeline" aria-label="Recorded activity on shared UTC axis">
          {#each blocks.filter(block => block.start !== null) as block, index (`${block.recordId}-${index}`)}
            <button class="activity" style:left={`${Math.min(99.4, position(block.start!))}%`} style:width={`${Math.max(0.6, (block.end === null ? 0 : position(block.end) - position(block.start!)))}%`} title={`${block.label} · ${utc(block.start!)}${block.end === null ? '' : ` → ${utc(block.end)}`} · ${block.unresolved ?? (block.end === null ? 'Recorded activity point' : 'Recorded tool interval')}`} aria-label={`Inspect ${block.label} at ${utc(block.start!)}`} onclick={() => inspect(session.storage_id, block.recordId)}></button>
          {/each}
        </div>
        <details><summary>Visited activity and unresolved evidence ({blocks.length})</summary>{#each blocks as block, index (`${block.recordId}-${index}`)}<div class="record"><button onclick={() => inspect(session.storage_id, block.recordId)}>{block.label}</button> · {block.start === null ? 'Timestamp unavailable' : utc(block.start)}{#if block.unresolved} · {block.unresolved}{/if}{#if block.resultId}<button onclick={() => inspect(session.storage_id, block.resultId)}>Inspect result</button>{/if}</div>{/each}</details>
        {#if observedBlockCount > MAX_BOARD_RECORDS}<p>100 activity blocks rendered; {observedBlockCount - MAX_BOARD_RECORDS} additional visited blocks omitted. Continue in the inspector.</p>{/if}
        <div class="controls"><button disabled={busy.includes(session.storage_id)} onclick={() => { records = { ...records, [session.storage_id]: [] }; void load(session.storage_id); }}>Reload source</button><button disabled={busy.includes(session.storage_id) || !page?.next_cursor || (records[session.storage_id]?.length ?? 0) >= MAX_BOARD_RECORDS} onclick={() => void load(session.storage_id, generation, true)}>Load next bounded page</button></div>
        {#if (records[session.storage_id]?.length ?? 0) >= MAX_BOARD_RECORDS}<p>100-record rendering limit reached. Continue navigation in the inspector.</p>{/if}
      </section>
    {/each}
    {#if targets.length}<section><h3>Source comparison</h3><p>Up to two inspectors, each with bounded pages. Select a block in another session to compare.</p><div class="comparison">{#each targets as target (target.sessionId)}<div><h3>{label(chosen.find(session => session.storage_id === target.sessionId)!)}</h3><TranscriptInspector embedded sessionId={target.sessionId} recordId={target.recordId} onclose={() => { targets = targets.filter(item => item.sessionId !== target.sessionId); }} /></div>{/each}</div></section>{/if}
  </div>
</dialog>

<style>
  dialog { width: min(1200px, calc(100vw - 24px)); height: min(940px, calc(100vh - 24px)); max-width: none; max-height: none; margin: auto; padding: 0; border: 1px solid var(--border); border-radius: 10px; background: var(--panel); color: var(--text); font-size: 12px; }
  dialog::backdrop { background: #0008; } header, .lane-heading, .controls { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; justify-content: space-between; }
  dialog[open] { display: flex; flex-direction: column; } header { padding: 14px; flex-shrink: 0; border-bottom: 1px solid var(--border); } .body { padding: 14px; overflow: auto; flex: 1; min-height: 0; }
  h2 { font-size: 18px; font-weight: 600; } h3 { font-weight: 600; } p { margin: 8px 0; } button, select { padding: 4px 8px; border: 1px solid var(--border); border-radius: 4px; } button { color: var(--accent); } button:disabled { opacity: .45; }
  button:focus-visible, select:focus-visible, input:focus-visible, summary:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .candidate { display: block; margin: 8px 0; overflow-wrap: anywhere; } .lane { margin: 14px 0; padding: 12px; border: 1px solid var(--border); border-radius: 6px; } .lane-heading h3 { min-width: 0; overflow-wrap: anywhere; } summary { cursor: pointer; } .axis { font-family: var(--font-mono); overflow-wrap: anywhere; }
  .timeline { position: relative; height: 30px; background: var(--card); border: 1px solid var(--border); margin: 10px 0; overflow: hidden; } .activity { position: absolute; top: 6px; bottom: 6px; min-width: 4px; padding: 0; background: var(--accent); }
  .record { margin: 6px 0; overflow-wrap: anywhere; } .comparison { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 12px; }
  @media (max-width: 800px) { .comparison { grid-template-columns: minmax(0, 1fr); } }
</style>
