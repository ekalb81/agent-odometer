<script lang="ts">
  import { onMount } from 'svelte';
  import { getQuotaConfig, setQuotaConfig } from '../lib/ipc';
  import { ambient, ambientLabels, invalidateAmbient, readAmbient } from '../lib/stores/ambient';
  import type { AmbientCategories, QuotaConfigWire } from '../lib/types';
  let { onOpen }: { onOpen?: (route: import("../lib/types").AmbientRoute) => void } = $props();
  let draft = $state<QuotaConfigWire | null>(null), saving = $state(false), message = $state('');
  let quiet = $state(false), start = $state(22), end = $state(7);
  let generation = 0;
  const defaults: AmbientCategories = { attention: false, provider_incidents: false, stale_quota: false, retention_risk: false };
  const categories: [keyof AmbientCategories, string][] = [
    ['attention', 'Transcript attention (selected transcript categories)'],
    ['provider_incidents', 'Public provider incidents (cached observations only)'],
    ['stale_quota', 'Expired transcript quota observations'],
    ['retention_risk', 'Missing source transcripts in retained history'],
  ];
  async function load(): Promise<void> {
    const request = ++generation;
    try {
      const next = await getQuotaConfig();
      if (request !== generation) return;
      draft = { ...next, notifications: { ...next.notifications, ambient: { ...defaults, ...next.notifications.ambient } } };
      quiet = !!next.notifications.quiet_hours;
      [start, end] = next.notifications.quiet_hours ?? [22, 7];
    } catch { if (request === generation) { draft = null; message = 'Shared alert settings unavailable.'; } }
  }
  onMount(() => { void load(); void readAmbient(); return () => { generation++; }; });
  async function save(): Promise<void> {
    if (!draft || saving) return;
    saving = true; message = '';
    const request = ++generation;
    invalidateAmbient();
    try {
      const result = await setQuotaConfig({ ...draft, notifications: { ...draft.notifications, quiet_hours: quiet ? [start, end] : null } });
      if (request !== generation) return;
      draft = result; message = 'Shared alert policy saved. Suppressed observations never replay.';
      void readAmbient();
    } catch { if (request === generation) message = 'Settings changed or could not be saved. Refresh before retrying.'; }
    finally { if (request === generation) saving = false; }
  }
  async function permission(): Promise<void> {
    if (typeof Notification === 'undefined') { message = 'Desktop notifications unavailable; in-app alerts remain available.'; return; }
    try { const result = await Notification.requestPermission(); message = result === 'granted' ? 'Desktop delivery allowed; shared policy still applies.' : 'Desktop delivery unavailable; in-app alerts remain available.'; }
    catch { message = 'Desktop notifications unavailable; in-app alerts remain available.'; }
  }
</script>
<section id="ambient-settings" class="max-w-3xl" aria-label="Shared alerts">
  <h2 class="text-sm font-semibold mb-2">Shared alerts</h2>
  <div class="panel">
    <p>One policy applies to budgets and all selected categories, in every provider tab. Alerts are local and advisory; they never change token, price, or quota history.</p>
    {#if draft}
      <fieldset disabled={saving}>
        <label><input type="checkbox" bind:checked={draft.notifications.enabled} /> Enable shared alerts</label>
        <p>Existing budget notification choices are preserved. New categories start off.</p>
        {#each categories as [key, label]}
          <label><input type="checkbox" bind:checked={draft.notifications.ambient![key]} /> {label}</label>
        {/each}
        <label><input type="checkbox" bind:checked={quiet} /> Quiet hours in this computer's local time</label>
        {#if quiet}
          <label>Start hour <input type="number" min="0" max="23" bind:value={start} /></label>
          <label>End hour <input type="number" min="0" max="23" bind:value={end} /></label>
          <p>Start is included; end is excluded. A later start wraps through midnight. Equal hours disable the quiet interval.</p>
        {/if}
        <p>Disabled, quiet, expired, and startup observations are consumed without delayed replay. Provider alerts use cached public observations; this setting starts no background polling.</p>
        <button onclick={save}>Save shared policy</button>
        <button onclick={load}>Refresh settings</button>
        <button onclick={permission}>Allow desktop notifications</button>
      </fieldset>
    {/if}
    {#if message}<p role="status">{message}</p>{/if}
    <h3>Recent delivered alerts</h3>
    {#if !$ambient?.available}<p>Notification state unavailable; delivery fails closed.</p>
    {:else if !$ambient.recent.length}<p>No recent delivered alerts.</p>
    {:else}{#each $ambient.recent as alert (alert.id)}
      <p>{alert.provider}: {ambientLabels[alert.code] ?? 'Local alert'} · {new Date(alert.delivered_at).toLocaleString()} <button onclick={() => onOpen?.(alert.route)}>Evidence</button></p>
    {/each}{/if}
  </div>
</section>
<style>
  .panel { display: flex; flex-direction: column; gap: .75rem; padding: .875rem; border: 1px solid var(--border); border-radius: 8px; font-size: .75rem; }
  p { color: var(--muted); } label { display: block; margin: .5rem 0; } input[type=number] { width: 4rem; border: 1px solid var(--border); } button { margin-right: .5rem; padding: .25rem .5rem; border: 1px solid var(--border); border-radius: 4px; } h3 { font-weight: 600; }
</style>
