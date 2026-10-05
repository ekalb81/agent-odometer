<script lang="ts">
  import { onMount } from 'svelte';
  import { attention, attentionLabels, refreshAttention } from '../lib/stores/attention';
  import type { AttentionAlert } from '../lib/types';
  let latest = $state<AttentionAlert | null>(null);
  let initialized = false;
  const seen = new Set<string>();
  function message(alert: AttentionAlert): string {
    return `${alert.provider_label} · Session ${alert.session_ref.slice(0, 8)}: ${attentionLabels[alert.category]}. Source: ${alert.source}. Observed ${new Date(alert.observed_at).toLocaleString()}.`;
  }
  onMount(() => {
    const unsubscribe = attention.subscribe(snapshot => {
      if (!snapshot?.available) { latest = null; return; }
      if (latest && !snapshot.alerts.some(alert => alert.id === latest?.id)) latest = null;
      if (!snapshot.preferences.categories.length) latest = null;
      for (const alert of snapshot.alerts) {
        if (seen.has(alert.id)) continue;
        seen.add(alert.id);
        if (!initialized) continue; // Opening a window never replays its backlog.
        latest = alert;
        if (typeof Notification !== 'undefined' && Notification.permission === 'granted') {
          try { new Notification('Odometer transcript alert', { body: message(alert), tag: alert.id }); } catch { /* In-app notice remains available. */ }
        }
      }
      // Retain the currently returned bounded IDs, including dismissed notices.
      if (seen.size > 100) { const current = new Set(snapshot.alerts.map(alert => alert.id)); for (const id of seen) if (!current.has(id)) seen.delete(id); }
      initialized = true;
    });
    void refreshAttention();
    const timer = setInterval(() => { void refreshAttention(); }, 15_000);
    return () => { clearInterval(timer); unsubscribe(); };
  });
</script>

{#if latest}
  <aside aria-label="Transcript attention alert" role="status">
    <p>{message(latest)}</p>
    <button onclick={() => { latest = null; }} aria-label="Dismiss transcript alert">Dismiss</button>
  </aside>
{/if}

<style>
  aside { position: fixed; right: 1rem; bottom: 2.5rem; z-index: 70; max-width: min(26rem, calc(100vw - 2rem)); display: flex; align-items: start; gap: .75rem; padding: .75rem; background: var(--bg); color: var(--ink); border: 1px solid var(--border); border-radius: 8px; box-shadow: 0 4px 20px #0002; font-size: .75rem; }
  button { padding: .2rem .4rem; border: 1px solid var(--border); border-radius: 4px; }
</style>
