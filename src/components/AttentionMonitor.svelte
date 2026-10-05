<script lang="ts">
  import { onMount } from 'svelte';
  import { refreshAttention } from '../lib/stores/attention';
  import { ambient, ambientLabels, refreshAmbient, invalidateAmbient } from '../lib/stores/ambient';
  import { onQuotaPolicyUpdated } from '../lib/ipc';
  import type { AmbientNotice, AmbientRoute } from '../lib/types';
  let { onOpen }: { onOpen?: (route: AmbientRoute) => void } = $props();
  let latest = $state<AmbientNotice | null>(null);
  const seen = new Set<string>();
  function message(alert: AmbientNotice): string {
    return `${alert.provider}: ${ambientLabels[alert.code] ?? 'Local alert'}. Observed ${new Date(alert.observed_at).toLocaleString()}.`;
  }
  onMount(() => {
    let disposed = false, stopPolicy: (() => void) | null = null;
    void onQuotaPolicyUpdated(() => invalidateAmbient()).then(stop => {
      if (disposed) stop(); else stopPolicy = stop;
    }).catch(() => {});
    const unsubscribe = ambient.subscribe(snapshot => {
      if (!snapshot?.available || !snapshot.notifications.enabled) { latest = null; return; }
      if (latest && !snapshot.recent.some(alert => alert.id === latest?.id)) latest = null;
      for (const alert of snapshot.alerts) {
        if (seen.has(alert.id)) continue;
        seen.add(alert.id); latest = alert;
        if (typeof Notification !== 'undefined' && Notification.permission === 'granted') {
          try { new Notification('Odometer alert', { body: message(alert), tag: alert.id }); } catch { /* In-app evidence remains available. */ }
        }
      }
      if (seen.size > 100) {
        const current = new Set(snapshot.recent.map(alert => alert.id));
        for (const id of seen) if (!current.has(id)) seen.delete(id);
      }
    });
    const refresh = () => { void refreshAttention(); void refreshAmbient(); };
    refresh();
    const timer = setInterval(refresh, 15_000);
    return () => { disposed = true; stopPolicy?.(); clearInterval(timer); unsubscribe(); };
  });
</script>
{#if latest}
  <aside aria-label="Shared Odometer alert" role="status">
    <p>{message(latest)}</p>
    <button onclick={() => onOpen?.(latest!.route)}>Evidence</button>
    <button onclick={() => { latest = null; }} aria-label="Dismiss alert">Dismiss</button>
  </aside>
{/if}
<style>
  aside { position: fixed; right: 1rem; bottom: 2.5rem; z-index: 70; max-width: min(26rem, calc(100vw - 2rem)); display: flex; align-items: start; gap: .75rem; padding: .75rem; background: var(--bg); color: var(--ink); border: 1px solid var(--border); border-radius: 8px; box-shadow: 0 4px 20px #0002; font-size: .75rem; }
  button { padding: .2rem .4rem; border: 1px solid var(--border); border-radius: 4px; }
</style>
