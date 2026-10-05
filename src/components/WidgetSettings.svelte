<script lang="ts">
  import { onMount } from 'svelte';
  import { getWidgetSettings, setWidgetSettings, onWidgetSettingsUpdated, onWidgetSettingsError } from '../lib/ipc';
  import type { WidgetSettings, WidgetPreferences } from '../lib/types';
  let value = $state<WidgetSettings | null>(null);
  let draft = $state<WidgetPreferences>({ visible: false, provider: 'codex', kind: 'quota', always_on_top: false });
  let busy = $state(false); let error = $state<string | null>(null); let alive = true;
  let generation = 0;
  function update(next: WidgetSettings) {
    if (value && next.revision < value.revision) return;
    generation++; value = next; draft = { ...next.preferences };
  }
  async function load() {
    const request = generation;
    busy = true; error = null;
    try { const next = await getWidgetSettings(); if (alive && request === generation) update(next); }
    catch { if (alive && request === generation) error = 'Widget settings unavailable; existing settings preserved.'; }
    finally { if (alive) busy = false; }
  }
  async function save() {
    if (!value || busy) return;
    const request = generation;
    busy = true; error = null;
    try { const next = await setWidgetSettings(value.revision, { ...draft }); if (alive && request === generation) update(next); }
    catch { if (alive && request === generation) error = 'Widget settings could not be applied. Reload before retrying.'; }
    finally { if (alive) busy = false; }
  }
  onMount(() => {
    const unsubs: (() => void)[] = [];
    const retain = (fn: () => void) => { if (alive) unsubs.push(fn); else fn(); };
    void onWidgetSettingsUpdated(next => { if (alive) update(next); }).then(retain).catch(() => {});
    void onWidgetSettingsError(message => { if (alive) error = message; }).then(retain).catch(() => {});
    void load(); return () => { alive = false; generation++; unsubs.forEach(fn => fn()); };
  });
</script>

<section aria-label="Compact local widget" class="max-w-3xl">
  <h2 class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Compact local widget</h2>
  <p class="text-xs text-ink-faint mb-3">A separate Odometer window on Windows, macOS and Linux. This is a compact app window; it is not integrated into Windows Widget Board, macOS WidgetKit, or a desktop shell. It works from local evidence with live polling disabled.</p>
  <fieldset disabled={busy || !value} class="space-y-3 text-xs">
    <label class="flex gap-2 items-center"><input type="checkbox" bind:checked={draft.visible} /> Show widget now and on startup</label>
    <div class="flex gap-4 flex-wrap">
      <label>Widget provider <select class="ml-2 bg-card border border-edge rounded-md px-2 py-1" bind:value={draft.provider}><option value="codex">Codex</option><option value="claude_code">Claude Code</option><option value="gemini_cli">Gemini CLI</option></select></label>
      <label>Widget summary <select class="ml-2 bg-card border border-edge rounded-md px-2 py-1" bind:value={draft.kind}><option value="quota">Quota observations</option><option value="usage">Local session cumulative usage</option></select></label>
    </div>
    <label class="flex gap-2 items-center"><input type="checkbox" bind:checked={draft.always_on_top} /> Keep widget above other app windows</label>
    <button class="text-accent hover:underline" onclick={() => void save()}>Apply widget settings</button>
  </fieldset>
  <p class="text-xs text-ink-faint mt-3">Closing its window hides it and turns off startup visibility. Quota observations keep their source age and reset evidence. Usage covers available-source sessions, including archived files still present; missing retained sources are excluded. The tray remains available.</p>
  {#if busy}<p role="status" class="text-xs text-ink-muted mt-2">Loading or saving widget settings…</p>{/if}
  {#if error}<p role="alert" class="text-xs text-amber-500 mt-2">{error} <button class="underline" disabled={busy} onclick={() => void load()}>Reload widget settings</button></p>{/if}
</section>
