<script lang="ts">
  import { attention, attentionError, attentionLabels, refreshAttention, saveAttention } from '../lib/stores/attention';
  import type { AttentionEventKind, AttentionPreferences } from '../lib/types';
  let draft = $state<AttentionPreferences | null>(null);
  let saving = $state(false), message = $state('');
  const categories = Object.keys(attentionLabels) as AttentionEventKind[];
  $effect(() => {
    const current = $attention?.preferences;
    if (current && draft?.revision !== current.revision) draft = { ...current, categories: [...current.categories], providers: [...current.providers] };
  });
  function category(kind: AttentionEventKind, checked: boolean): void {
    if (!draft) return;
    draft.categories = checked ? [...draft.categories, kind] : draft.categories.filter(value => value !== kind);
  }
  async function save(): Promise<void> {
    if (!draft || saving) return;
    saving = true; message = '';
    try {
      await saveAttention({ ...draft, categories: [...draft.categories], providers: [...draft.providers] });
      message = 'Attention preferences saved. Only new observations can notify.';
    } catch { message = 'Attention preferences could not be saved. Refresh before trying again.'; }
    finally { saving = false; }
  }
  async function permission(): Promise<void> {
    if (typeof Notification === 'undefined') { message = 'Desktop notifications are unavailable; in-app notices remain available.'; return; }
    try { const result = await Notification.requestPermission(); message = result === 'granted' ? 'Desktop notifications allowed for opted-in categories.' : 'Desktop notifications were not allowed; in-app notices remain available.'; }
    catch { message = 'Desktop notifications are unavailable; in-app notices remain available.'; }
  }
  const providerName = (provider: string) => ({ codex: 'Codex', claude_code: 'Claude Code', gemini_cli: 'Gemini CLI' })[provider] ?? 'Other provider';
</script>

<section id="attention-settings" aria-labelledby="attention-heading">
  <h2 id="attention-heading" class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Agent attention</h2>
  <div class="panel">
    <p>Transcript categories also require <a href="#ambient-settings">Shared alerts</a> to be enabled. Shared quiet hours suppress both desktop and in-app delivery without replay.</p>
    <p>All alerts start off. Choose recorded events to receive local notices while Odometer is open. This reports transcript evidence, not process visibility or task quality. No hooks or network access are added.</p>
    {#if $attentionError || $attention?.available === false}<p role="status">Attention state is unavailable. Alerts are suppressed until it can be read and saved safely.</p>{/if}
    {#if draft}
      <fieldset disabled={saving}>
        <legend>Notify for new events</legend>
        <div class="categories">{#each categories as kind}<label><input type="checkbox" checked={draft.categories.includes(kind)} onchange={event => category(kind, event.currentTarget.checked)} /> {attentionLabels[kind]}</label>{/each}</div>
        <div class="options">
          <label>Provider <select value={draft.providers[0] ?? ''} onchange={event => { if (draft) draft.providers = event.currentTarget.value ? [event.currentTarget.value] : []; }}><option value="">All providers</option><option value="codex">Codex</option><option value="claude_code">Claude Code</option><option value="gemini_cli">Gemini CLI</option></select></label>
          <label>Tool category <select bind:value={draft.tool_kind}><option value={null}>Any tool category</option><option value="read">Read</option><option value="search">Search</option><option value="mutation">Mutation</option><option value="command">Command</option><option value="other">Other</option></select></label>
          <label>Evidence expires after <select bind:value={draft.stale_after_seconds}>{#if ![60, 300, 900].includes(draft.stale_after_seconds)}<option value={draft.stale_after_seconds}>{draft.stale_after_seconds} seconds (saved)</option>{/if}<option value={60}>1 minute</option><option value={300}>5 minutes</option><option value={900}>15 minutes</option></select></label>
        </div>
      </fieldset>
      <p>Tool matching applies only to tool events. Waiting requires an explicit recorded input-request tool; otherwise it stays unknown. Stale or missing observations become unknown. Notices omit session titles, prompts, tool arguments, and paths, and are limited to one every 30 seconds. Suppressed events are not replayed later.</p>
      <div class="actions"><button onclick={save} disabled={saving}>Save attention preferences</button><button onclick={() => void refreshAttention()} disabled={saving}>Refresh attention</button><button onclick={permission}>Allow desktop notifications…</button></div>
    {:else if !$attentionError}<p>Loading attention preferences…</p>{/if}
    {#if message}<p role="status">{message}</p>{/if}
    {#if $attention?.preferences.categories.length}
      <h3>Most recent transcript evidence</h3>
      {#if !$attention.observations.length}<p>No current supported observations. State is unknown until a session is scanned or appended while enabled.</p>{/if}
      {#each $attention.observations as row (row.session_ref)}
        <article><strong>{providerName(row.provider)} · Session {row.session_ref.slice(0, 8)} · {row.state}</strong><p>{row.source} · Observed {new Date(row.observed_at).toLocaleString()}</p>{#if row.stale}<p>Expired evidence; last observed state was {row.observed_state}.</p>{/if}{#if row.partial}<p>Only a bounded recent slice was examined; older evidence may be omitted.</p>{/if}</article>
      {/each}
    {/if}
  </div>
</section>

<style>
  .panel { display: flex; flex-direction: column; gap: .75rem; padding: .875rem; border: 1px solid var(--border); border-radius: 8px; font-size: .75rem; }
  p { color: var(--muted); }
  legend, h3 { font-weight: 600; margin-bottom: .5rem; }
  .categories { display: grid; grid-template-columns: repeat(auto-fit, minmax(13rem, 1fr)); gap: .5rem; }
  label { display: flex; align-items: center; gap: .4rem; }
  .options, .actions { display: flex; flex-wrap: wrap; gap: .75rem; margin-top: .75rem; }
  select, button { padding: .3rem .5rem; border: 1px solid var(--border); border-radius: 4px; background: var(--bg); }
  article { border-top: 1px solid var(--border); padding-top: .5rem; }
  :is(input, select, button):focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
</style>
