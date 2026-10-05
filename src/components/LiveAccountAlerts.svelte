<script lang="ts">
  import { listen } from '@tauri-apps/api/event';
  import { getQuotaConfig, setQuotaConfig } from '../lib/ipc';
  import { getLiveQuotaStatus, onLiveQuotaUpdated } from '../lib/liveQuota';
  import type { LiveQuotaStatus } from '../lib/liveQuota';
  import { liveAlertScopes } from '../lib/liveAccountAlerts';
  import type { QuotaConfigWire, LiveAccountBudget } from '../lib/types';
  import type { ViewScope } from '../lib/sessionProjection';
  import { invalidateAmbient } from '../lib/stores/ambient';
  let { active = true, harness = 'all' }: { active?: boolean; harness?: ViewScope } = $props();
  let config = $state<QuotaConfigWire | null>(null), status = $state<LiveQuotaStatus | null>(null);
  let scope = $state(''), threshold = $state(80), enabled = $state(false), saving = $state(false), message = $state('');
  let now = $state(Date.now());
  let generation = 0;
  let visible = $derived(active && (harness === 'all' || harness === 'codex'));
  let scopes = $derived(liveAlertScopes(status, now));
  let rules = $derived(config?.live_account_budgets ?? []);
  let availableScopes = $derived(scopes.filter(item => !rules.some(rule => rule.account_id === item.rule.account_id && rule.consented_at === item.rule.consented_at && rule.limit_id === item.rule.limit_id && rule.window_kind === item.rule.window_kind && rule.window_minutes === item.rule.window_minutes)));
  async function load(): Promise<void> {
    const token = ++generation;
    try {
      const [next, cached] = await Promise.all([getQuotaConfig(), getLiveQuotaStatus()]);
      if (token !== generation) return;
      config = next; status = cached; now = Date.now(); scope = ''; saving = false; message = '';
    } catch { if (token === generation) { config = null; status = null; saving = false; message = 'Account alert settings unavailable.'; } }
  }
  $effect(() => {
    if (!visible) { generation++; config = null; return; }
    void load();
    let cancelled = false;
    const unlisten: (() => void)[] = [];
    for (const subscription of [listen('quota-policy-updated', () => void load()), onLiveQuotaUpdated(() => void load())]) {
      void subscription.then(stop => { if (cancelled) stop(); else unlisten.push(stop); }).catch(() => {});
    }
    const timer = setInterval(() => { now = Date.now(); }, 15_000);
    return () => { cancelled = true; generation++; clearInterval(timer); unlisten.forEach(stop => stop()); };
  });
  async function save(next: LiveAccountBudget[]): Promise<void> {
    if (!config || saving) return;
    const token = ++generation;
    saving = true; message = ''; invalidateAmbient();
    try {
      const saved = await setQuotaConfig({ ...config, live_account_budgets: next });
      if (token !== generation) return;
      config = saved; scope = ''; enabled = false; message = 'Account rules saved. Shared alerts and quiet hours apply.';
    } catch { if (token === generation) { message = 'Settings changed or save failed. Refresh before retrying.'; config = null; } }
    finally { if (token === generation) saving = false; }
  }
  function add(): void {
    const selected = availableScopes.find(item => item.key === scope);
    if (!selected || !Number.isFinite(threshold) || threshold <= 0 || threshold > 100) return;
    void save([...rules, { ...selected.rule, id: crypto.randomUUID(), threshold_percent: threshold, enabled }]);
  }
  function label(rule: LiveAccountBudget): string {
    const matching = scopes.find(item => item.rule.account_id === rule.account_id && item.rule.consented_at === rule.consented_at && item.rule.limit_id === rule.limit_id && item.rule.window_kind === rule.window_kind && item.rule.window_minutes === rule.window_minutes);
    if (matching) return matching.label;
    const accounts = status?.accounts.filter(item => item.consent.account_id === rule.account_id && item.consent.consented_at === rule.consented_at) ?? [];
    const account = accounts.length === 1 ? accounts[0] : null;
    const identity = account ? account.consent.label : `Saved account ${rule.account_id.slice(0, 256)}`;
    const bucket = account?.buckets.find(item => item.limit_id === rule.limit_id);
    const availability = !account ? 'saved consent unavailable' : !account.consent.enabled ? 'polling paused' : 'current reading unavailable';
    return `${identity} · ${bucket?.limit_name ?? rule.limit_id.slice(0, 128)} · ${rule.window_kind} ${rule.window_minutes} min · ${availability}`;
  }
</script>
{#if harness === 'all' || harness === 'codex'}
<details class="account-alerts" data-testid="live-account-alerts">
  <summary>Approved account quota alerts <span>Opt-in</span></summary>
  <p>Rules watch an exact approved account, model limit, and window using cached live provider observations. This editor starts no lookup or polling. Transcript budgets remain separate.</p>
  <p>Shared alerts must be enabled; shared quiet hours also apply. Unknown, expired, or changed-account observations cannot rearm an alert.</p>
  {#if config}
    <fieldset disabled={saving}>
      <label>Approved account and window <select bind:value={scope}><option value="">Choose a current cached window</option>{#each availableScopes as item (item.key)}<option value={item.key}>{item.label}</option>{/each}</select></label>
      {#if scopes.length > availableScopes.length}<p>Configured windows already have a rule. Enable, disable, or remove the existing rule below.</p>{/if}
      {#if !scopes.length}<p>No current unambiguous approved account window. Enable polling separately and wait for a valid observation.</p>{/if}
      <label>Percent used threshold <input type="number" min="1" max="100" bind:value={threshold} /></label>
      <label><input type="checkbox" bind:checked={enabled} /> Enable this new account rule</label>
      <button disabled={!availableScopes.some(item => item.key === scope) || !Number.isFinite(threshold) || threshold <= 0 || threshold > 100 || rules.length >= 32} onclick={add}>Add account rule</button>
      {#each rules as rule (rule.id)}
        <div class="rule"><p>{label(rule)} · {rule.threshold_percent}% used · {rule.enabled ? 'Enabled' : 'Disabled'}</p>
        <button onclick={() => save(rules.map(item => item.id === rule.id ? { ...item, enabled: !item.enabled } : item))}>{rule.enabled ? 'Disable' : 'Enable'} account rule</button>
        <button onclick={() => save(rules.filter(item => item.id !== rule.id))}>Remove account rule</button></div>
      {/each}
    </fieldset>
  {:else}<p>Account alert settings loading or unavailable.</p>{/if}
  <button disabled={saving} onclick={load}>Refresh account alert settings</button>
  {#if message}<p role="status">{message}</p>{/if}
</details>
{/if}
<style>
  .account-alerts { margin-top: .75rem; padding: .65rem; border: 1px solid var(--border); border-radius: 6px; font-size: .8rem; }
  .rule p { overflow-wrap: anywhere; }
  summary { cursor: pointer; font-weight: 600; } summary span, p { color: var(--muted); } p, label { margin: .5rem 0; } label { display: flex; align-items: center; gap: .5rem; flex-wrap: wrap; }
  select { max-width: 100%; } input[type=number] { width: 5rem; } button, input, select { font: inherit; padding: .25rem; background: var(--panel); border: 1px solid var(--border); border-radius: 4px; } button { margin: .25rem .35rem .25rem 0; } .rule { border-top: 1px solid var(--border); } fieldset { border: 0; } button:disabled { opacity: .5; }
</style>
