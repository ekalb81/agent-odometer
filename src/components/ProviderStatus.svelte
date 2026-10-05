<script lang="ts">
  import { get } from 'svelte/store';
  import { config } from '../lib/stores/config';
  import { getProviderServiceStatus, setProviderStatusEnabled } from '../lib/ipc';
  import type { ProviderServiceStatusSnapshot, ProviderStatusIndicator } from '../lib/types';

  let snapshot = $state<ProviderServiceStatusSnapshot | null>(null);
  let saving = $state(false), message = $state('');
  let generation = 0;
  let poll = 0;
  const names: Record<string, string> = { codex: 'OpenAI · Codex', claude_code: 'Claude', gemini_cli: 'Gemini CLI' };
  const labels: Record<ProviderStatusIndicator, string> = {
    operational: 'Operational', minor: 'Minor incident', major: 'Major incident',
    critical: 'Critical incident', maintenance: 'Maintenance',
  };
  function time(value: string | null): string {
    return value && Number.isFinite(Date.parse(value)) ? new Date(value).toLocaleString() : 'Not observed';
  }
  async function refresh(request: number): Promise<void> {
    const currentPoll = ++poll;
    try {
      const result = await getProviderServiceStatus();
      if (request !== generation || currentPoll !== poll) return;
      snapshot = result; message = '';
    } catch {
      if (request === generation && currentPoll === poll) { snapshot = null; message = 'Public status is unavailable. Local usage remains available.'; }
    }
  }
  $effect(() => {
    const enabled = $config.provider_status_enabled === true;
    const request = ++generation;
    snapshot = null; message = '';
    if (!enabled) return () => { generation++; };
    void refresh(request);
    // Memory snapshots refresh locally. Rust alone owns outbound cadence.
    const timer = setInterval(() => { void refresh(request); }, 15_000);
    return () => { generation++; clearInterval(timer); };
  });
  async function toggle(event: Event): Promise<void> {
    const checkbox = event.currentTarget as HTMLInputElement;
    const enabled = checkbox.checked;
    checkbox.checked = get(config).provider_status_enabled === true;
    if (saving) return;
    saving = true; message = '';
    try {
      const previous = get(config);
      const updated = await setProviderStatusEnabled(enabled);
      // Do not replace a newer authoritative config-updated event.
      if (get(config) === previous) config.set(updated);
    } catch { message = 'The service-status setting could not be saved.'; }
    finally { saving = false; }
  }
</script>

<section id="provider-status-settings" aria-labelledby="provider-status-heading">
  <h2 id="provider-status-heading" class="text-sm font-semibold uppercase tracking-wider text-ink-muted mb-2">Provider service status</h2>
  <div class="status-panel">
    <label><input type="checkbox" checked={$config.provider_status_enabled === true} onchange={toggle} disabled={saving} /> Check public provider service status</label>
    <p>Off by default. While this panel is open, enabled checks read fixed public OpenAI and Claude status pages at most every five minutes, with backoff on failures. No account credentials or session content are sent. Proxy authentication and redirects are not supported.</p>
    <p>Provider-wide status does not establish the availability of your account or a specific session. Status never changes usage, prices, quotas, or budgets.</p>
    {#if !$config.provider_status_enabled}
      <p>Public status checks are off. Local usage and transcript quota observations remain available.</p>
    {:else if snapshot}
      {#each snapshot.providers as provider (provider.provider)}
        <article>
          <strong>{names[provider.provider] ?? 'Provider'}</strong>
          {#if provider.current_indicator && provider.state === 'current'}
            <span>{labels[provider.current_indicator]}</span>
          {:else}
            <span>{provider.state === 'pending' ? 'Checking public source…' : provider.state === 'unsupported' ? 'Unavailable · no supported public source' : provider.state === 'disabled' ? 'Checks disabled' : provider.state === 'stale' ? 'Unavailable · last check is stale' : 'Unavailable · public source could not be checked'}</span>
            {#if provider.last_known_indicator}<p>Last known: {labels[provider.last_known_indicator]} · {time(provider.checked_at)}. This is not a current reading.</p>{/if}
          {/if}
          {#if provider.source_url}<p class="source">Source: {provider.source_url}</p>{/if}
          {#if provider.state !== 'unsupported'}
            <p>Last successful check: {time(provider.checked_at)} · Source updated: {time(provider.source_updated_at)}</p>
            <p>Last attempt: {time(provider.last_attempt_at)} · Next check no earlier than: {time(provider.next_attempt_at)}</p>
            {#if provider.failure}<p>{provider.failure === 'rate_limited' ? 'Rate limited; waiting before retry.' : provider.failure === 'invalid_response' ? 'The source response was not recognized.' : provider.failure === 'http_error' ? 'The source returned an HTTP error.' : 'Offline or timed out.'}</p>{/if}
          {/if}
        </article>
      {/each}
    {:else if !message}<p>Checking public source…</p>{/if}
    {#if message}<p role="status">{message}</p>{/if}
  </div>
</section>

<style>
  .status-panel { display: flex; flex-direction: column; gap: .625rem; padding: .875rem; border: 1px solid var(--border); border-radius: 8px; font-size: .75rem; }
  label { display: flex; gap: .5rem; align-items: center; }
  p { color: var(--muted); overflow-wrap: anywhere; }
  article { display: flex; flex-direction: column; gap: .375rem; padding: .75rem 0; border-top: 1px solid var(--border); }
  .source { font-family: monospace; font-size: .6875rem; }
  input:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
</style>
