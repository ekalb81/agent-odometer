<script lang="ts">
  import type { ViewScope } from '../lib/sessionProjection';
  import { getLiveQuotaStatus, identifyQuotaAccount, approveQuotaAccount, changeQuotaAccount, liveQuotaUnavailable,
    type DiscoveredQuotaAccount, type LiveQuotaStatus } from '../lib/liveQuota';
  import { quotaWindowLabel, resetCountdown } from '../lib/subscriptionUsage';
  let { active = true, harness = 'all' }: { active?: boolean; harness?: ViewScope } = $props();
  let status = $state<LiveQuotaStatus | null>(null);
  let candidate = $state<DiscoveredQuotaAccount | null>(null);
  let label = $state('');
  let error = $state<string | null>(null);
  let saving = $state(false);
  let now = $state(Date.now());
  let generation = 0;
  let requestGeneration = 0;
  let visible = $derived(active && (harness === 'all' || harness === 'codex'));

  async function refresh(token: number): Promise<void> {
    const request = ++requestGeneration;
    try {
      const next = await getLiveQuotaStatus();
      if (token !== generation || request !== requestGeneration) return;
      status = next; now = Date.now();
    } catch { if (token === generation && request === requestGeneration) { status = null; error = 'Live quota status could not be read.'; } }
  }
  $effect(() => {
    const token = ++generation;
    if (!visible) return;
    void refresh(token);
    const timer = setInterval(() => void refresh(token), 30_000);
    return () => { clearInterval(timer); generation++; };
  });
  async function identify(): Promise<void> {
    saving = true; error = null; candidate = null;
    const token = generation;
    try {
      const found = await identifyQuotaAccount();
      if (token === generation) { candidate = found; label = `Codex account ${(status?.accounts.length ?? 0) + 1}`; }
    } catch (reason) { if (token === generation) error = liveQuotaUnavailable(String(reason)); }
    finally { saving = false; }
  }
  async function approve(): Promise<void> {
    if (!candidate) return;
    saving = true; error = null; requestGeneration++;
    try { await approveQuotaAccount(candidate.account_id, label); candidate = null; await refresh(generation); }
    catch { error = 'Approval could not be saved. Identify the account again if the lookup expired.'; }
    finally { saving = false; }
  }
  async function change(accountId: string, enabled: boolean, revoke = false): Promise<void> {
    saving = true; error = null; candidate = null; requestGeneration++;
    try { await changeQuotaAccount(accountId, enabled, revoke); await refresh(generation); }
    catch { error = 'Consent change could not be saved. Polling is paused for this app run; retry before restarting.'; await refresh(generation); }
    finally { saving = false; }
  }
</script>

{#if harness === 'all' || harness === 'codex'}
  <details class="live-accounts" data-testid="live-quota-accounts">
    <summary>Live account quotas <span>Opt-in</span></summary>
    <p>Codex can read its current signed-in account through the installed CLI. Allowing a lookup uses that sign-in once to identify the account. Polling starts only after you approve the account below. Odometer stores account consent, never credentials or response bodies.</p>
    <p>Only one account can poll through this CLI at a time. Enabling an account pauses the others. Historical transcript usage remains unattributed. Claude Code live quota is unavailable until a supported source is available.</p>
    {#if error}<p role="alert">{error}</p>{/if}
    {#if status?.configuration_error}<p role="alert">{status.configuration_error}</p>{/if}
    <button type="button" disabled={saving || status?.busy || !!status?.configuration_error} onclick={identify}>Allow one account lookup</button>
    {#if candidate}
      <div class="candidate">
        <p>Account <code>{candidate.account_id}</code> · {candidate.plan_type ?? 'Plan unknown'}</p>
        <label>Local account label <input maxlength="80" bind:value={label} /></label>
        <button type="button" disabled={saving || !label.trim()} onclick={approve}>Enable polling for this account</button>
        <button type="button" disabled={saving} onclick={() => { candidate = null; }}>Cancel</button>
      </div>
    {/if}
    {#if !status}<p>Loading account settings…</p>
    {:else if status.accounts.length === 0}<p>No accounts approved. Live polling is off.</p>{/if}
    {#each status?.accounts ?? [] as account (account.consent.account_id)}
      <article>
        <strong>{account.consent.label}</strong>
        <p class="identity"><code>{account.consent.account_id}</code> · Codex CLI · live provider</p>
        {#if account.observed_at}<p>Fetched {new Date(account.observed_at).toLocaleString()}</p>{/if}
        {#if account.unavailable}<p>{liveQuotaUnavailable(account.unavailable)}</p>{/if}
        {#if !account.unavailable && account.ordinary_usage_allowed === false}<p>Provider reports ordinary included usage is blocked.</p>
        {:else if account.ordinary_usage_allowed === null && !account.unavailable}<p>Usage permission not reported. Window percentages do not establish access.</p>{/if}
        {#each account.buckets as bucket (bucket.limit_id)}
          <div class="bucket">
            <strong>{bucket.limit_name ?? bucket.limit_id}</strong>
            {#if !account.unavailable && bucket.spend_control_reached === true}<p>Provider spend control reached.</p>{/if}
            {#each bucket.snapshot.windows as window, index (`${window.kind}-${index}`)}
              <p>{quotaWindowLabel(window)} ·
                {#if window.unavailable}Unavailable ({window.unavailable.replaceAll('_', ' ')})
                {:else if window.unlimited}Unlimited
                {:else if window.remaining !== null}{window.remaining.toLocaleString(undefined, { maximumFractionDigits: 1 })}{window.unit === 'percent' ? '%' : ' credits'} left
                {:else}Not reported{/if}
                {#if window.stale} · Stale observation{/if}
                {#if window.resets_at} · Resets {resetCountdown(window.resets_at, now)}{/if}
              </p>
            {/each}
            {#if bucket.snapshot.windows.length === 0}<p>No quota windows reported.</p>{/if}
          </div>
        {/each}
        <div class="actions">
          <button type="button" disabled={saving} onclick={() => change(account.consent.account_id, !account.consent.enabled)}>{account.consent.enabled ? 'Pause polling' : 'Enable this account'}</button>
          <button type="button" disabled={saving} onclick={() => change(account.consent.account_id, false, true)}>Revoke consent</button>
        </div>
      </article>
    {/each}
  </details>
{/if}

<style>
  .live-accounts { margin-top: 0.75rem; padding: 0.65rem; border: 1px solid var(--border); border-radius: 6px; }
  summary { cursor: pointer; font-weight: 600; }
  summary span, .identity { font-size: 0.8rem; color: var(--muted); }
  p { font-size: 0.8rem; margin: 0.5rem 0; }
  code { overflow-wrap: anywhere; }
  article, .candidate { margin-top: 0.75rem; padding-top: 0.6rem; border-top: 1px solid var(--border); }
  button, input { font: inherit; }
  button { margin: 0.25rem 0.35rem 0.25rem 0; padding: 0.3rem 0.55rem; border: 1px solid var(--border); border-radius: 4px; background: var(--card); color: var(--accent); }
  button:focus-visible, input:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  button:disabled { opacity: 0.5; }
  input { border: 1px solid var(--border); border-radius: 4px; padding: 0.25rem; background: var(--panel); }
  label { display: flex; flex-wrap: wrap; gap: 0.5rem; align-items: center; }
  .bucket { margin: 0.5rem 0 0.5rem 0.6rem; }
  .actions { display: flex; flex-wrap: wrap; }
</style>
