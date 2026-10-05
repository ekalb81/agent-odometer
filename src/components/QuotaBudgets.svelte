<script lang="ts">
  import { getQuotaBudgetStatuses, getQuotaConfig, resolveProjects, setQuotaConfig } from '../lib/ipc';
  import type { BudgetUnit, ProjectInfo, QuotaAlert, QuotaBudget, QuotaBudgetCheck, QuotaConfigWire } from '../lib/types';
  import type { ViewScope } from '../lib/sessionProjection';
  import { providersStore } from '../lib/stores/providers.svelte';
  import { historyStore } from '../lib/stores/history.svelte';
  import { invalidateAmbient } from '../lib/stores/ambient';
  import { rates } from '../lib/stores/rates';

  interface Props {
    active?: boolean;
    harness?: ViewScope;
    onAlerts?: (alerts: QuotaAlert[]) => void;
  }
  let { active = true, harness = 'all', onAlerts }: Props = $props();

  interface BudgetDraft {
    provider: string;
    projectKey: string;
    unit: BudgetUnit;
    windowKind: string;
    periodHours: number;
    threshold: number;
  }

  const REFRESH_MS = 60_000;
  let config = $state<QuotaConfigWire | null>(null);
  let projects = $state<ProjectInfo[]>([]);
  let report = $state<QuotaBudgetCheck | null>(null);
  let loadError = $state<string | null>(null);
  let saveError = $state<string | null>(null);
  let projectError = $state<string | null>(null);
  let busy = $state(false);
  let editorOpen = $state(false);
  let editingId = $state<string | null>(null);
  let projectLoading = $state(false);
  let draft = $state<BudgetDraft>({
    provider: 'codex',
    projectKey: '',
    unit: 'tokens',
    windowKind: 'weekly',
    periodHours: 24,
    threshold: 100_000,
  });
  let lifecycleGeneration = 0;
  let reportGeneration = 0;

  const statusByBudget = $derived(new Map((report?.statuses ?? []).map((status) => [status.budget_id, status])));
  const availableProviders = $derived(providersStore.descriptors);
  const visibleProviders = $derived(availableProviders.filter((provider) => harness === 'all' || provider.id === harness));
  const percentProviders = $derived(visibleProviders.filter((provider) => provider.quota_source));
  const visibleBudgets = $derived(config?.budgets.filter((budget) => harness === 'all' || budget.provider === harness) ?? []);
  const visibleAlerts = $derived(report?.alerts.filter((alert) => harness === 'all' || alert.provider === harness) ?? []);
  const editingBudget = $derived(config?.budgets.find((budget) => budget.id === editingId) ?? null);

  function newDraft(): BudgetDraft {
    return {
      provider: harness === 'all' ? percentProviders[0]?.id ?? availableProviders[0]?.id ?? 'codex' : harness,
      projectKey: '',
      unit: 'tokens',
      windowKind: 'weekly',
      periodHours: 24,
      threshold: 100_000,
    };
  }

  async function refresh(lifecycleToken: number): Promise<void> {
    if (busy) return;
    const requestToken = ++reportGeneration;
    try {
      const [nextConfig, nextReport] = await Promise.all([getQuotaConfig(), getQuotaBudgetStatuses()]);
      if (lifecycleToken !== lifecycleGeneration || requestToken !== reportGeneration) return;
      config = nextConfig;
      report = nextReport;
      loadError = null;
      // Crossings are persisted for every provider; notify even when another
      // harness is selected, or its crossing would be consumed silently.
      if (nextReport.alerts.length > 0) onAlerts?.(nextReport.alerts);
    } catch {
      if (lifecycleToken === lifecycleGeneration && requestToken === reportGeneration) loadError = 'Budget status could not be refreshed.';
    }
  }

  $effect(() => {
    // Purge/recovery and same-version rate replacements invalidate headroom
    // immediately. Cleanup rejects in-flight results from the previous inputs.
    void historyStore.status.status;
    void historyStore.status.coverage_complete;
    void $rates;
    report = null;
    const token = ++lifecycleGeneration;
    if (!active) return;
    void refresh(token);
    const timer = setInterval(() => void refresh(token), REFRESH_MS);
    return () => {
      clearInterval(timer);
      lifecycleGeneration++;
      reportGeneration++;
    };
  });

  async function loadProjects(): Promise<void> {
    projectLoading = true;
    projectError = null;
    try {
      projects = await resolveProjects();
    } catch {
      projectError = 'Projects could not be loaded. Retry before adding a project budget.';
    } finally {
      projectLoading = false;
    }
  }

  function beginAdd(): void {
    editingId = null;
    draft = newDraft();
    editorOpen = true;
    void loadProjects();
  }

  function beginEdit(budget: QuotaBudget): void {
    editingId = budget.id;
    draft = {
      provider: budget.provider,
      projectKey: budget.project_key ?? '',
      unit: budget.unit,
      windowKind: budget.window_kind ?? 'weekly',
      periodHours: budget.period_hours ?? 24,
      threshold: budget.threshold,
    };
    editorOpen = true;
    void loadProjects();
  }

  function changeUnit(unit: BudgetUnit): void {
    draft.unit = unit;
    if (unit === 'percent_of_window') {
      draft.projectKey = '';
      draft.provider = percentProviders[0]?.id ?? draft.provider;
      draft.threshold = Math.min(100, Math.max(1, draft.threshold));
    }
  }

  function formatValue(value: number, unit: BudgetUnit): string {
    if (unit === 'percent_of_window') return `${value.toFixed(0)}%`;
    if (unit === 'usd') return `$${value.toFixed(2)} USD API estimate`;
    return `${value.toLocaleString()} tokens`;
  }

  function unavailableLabel(reason: string | null | undefined): string {
    if (!reason) return 'unavailable';
    const labels: Record<string, string> = {
      pricing_incomplete: 'Pricing unavailable', non_usd_rates: 'Non-USD rate card',
      history_unavailable: 'History unavailable', project_scope_unavailable: 'Project history unavailable',
      project_unavailable: 'Project unavailable', stale_quota: 'Quota is stale',
      quota_unavailable: 'Quota unavailable', disabled: 'Budget disabled',
    };
    return labels[reason] ?? 'Unavailable';
  }

  async function save(next: QuotaConfigWire): Promise<boolean> {
    if (!config || busy) return false;
    invalidateAmbient();
    busy = true;
    saveError = null;
    // Invalidate any in-flight report captured before this edit. The next
    // scheduled check evaluates the returned revision and saved budgets.
    reportGeneration++;
    try {
      config = await setQuotaConfig({ ...next, revision: config.revision ?? null });
      report = null;
      return true;
    } catch (cause) {
      saveError = String(cause) || 'Budget settings could not be saved.';
      return false;
    } finally {
      busy = false;
      if (!saveError && active) void refresh(lifecycleGeneration);
    }
  }

  async function saveDraft(): Promise<void> {
    if (!config || busy) return;
    const threshold = Number(draft.threshold);
    const periodHours = Number(draft.periodHours);
    if (!Number.isFinite(threshold) || threshold <= 0) {
      saveError = 'Enter a threshold greater than zero.';
      return;
    }
    if (draft.unit === 'percent_of_window' && threshold > 100) {
      saveError = 'Percent thresholds must be at most 100.';
      return;
    }
    if (draft.unit !== 'percent_of_window' && (!Number.isInteger(periodHours) || periodHours < 1 || periodHours > 8760)) {
      saveError = 'Rolling periods must be between 1 and 8760 hours.';
      return;
    }
    if (draft.unit === 'percent_of_window' && draft.projectKey) {
      saveError = 'Percent-of-window budgets are provider-wide.';
      return;
    }
    const existing = editingBudget;
    const budget: QuotaBudget = {
      id: existing?.id ?? `${draft.provider}-${draft.unit}-${Date.now()}`,
      provider: draft.provider,
      project_key: draft.unit === 'percent_of_window' || !draft.projectKey ? null : draft.projectKey,
      unit: draft.unit,
      window_kind: draft.unit === 'percent_of_window' ? draft.windowKind : null,
      period_hours: draft.unit === 'percent_of_window' ? null : periodHours,
      threshold,
      enabled: existing?.enabled ?? true,
    };
    const budgets = existing
      ? config.budgets.map((item) => item.id === existing.id ? budget : item)
      : [...config.budgets, budget];
    if (await save({ ...config, budgets })) editorOpen = false;
  }

  async function toggleBudget(budget: QuotaBudget): Promise<void> {
    if (!config) return;
    const budgets = config.budgets.map((item) => item.id === budget.id ? { ...item, enabled: !item.enabled } : item);
    await save({ ...config, budgets });
  }

  async function removeBudget(budgetId: string): Promise<void> {
    if (!config) return;
    if (await save({ ...config, budgets: config.budgets.filter((budget) => budget.id !== budgetId) }) && editingId === budgetId) {
      editorOpen = false;
      editingId = null;
    }
  }

  async function toggleNotifications(): Promise<void> {
    if (!config) return;
    const enabling = !config.notifications.enabled;
    if (enabling && typeof Notification !== 'undefined' && Notification.permission === 'default') {
      try { await Notification.requestPermission(); } catch { /* Panel alerts remain available. */ }
    }
    await save({ ...config, notifications: { ...config.notifications, enabled: enabling } });
  }
</script>

<details class="mt-2" data-testid="quota-budgets-panel">
  <summary class="text-[11px] font-semibold text-ink-muted cursor-pointer select-none">Soft budgets &amp; alerts</summary>
  <div class="mt-1.5 flex flex-col gap-1.5">
    {#if loadError}<p role="alert" class="text-[11px] text-neg">{loadError}</p>{/if}
    {#if config}
      <label class="flex items-center gap-1.5 text-[11px] text-ink-muted">
        <input type="checkbox" checked={config.notifications.enabled} disabled={busy} onchange={toggleNotifications} />
        Enable shared alerts (budgets and opted-in categories)
      </label>
      <p class="text-[10px] text-ink-faint">Quiet hours and other categories are configured in Settings → Shared alerts.</p>
      {#if report?.as_of}<p class="text-[10px] text-ink-faint">Status as of {new Date(report.as_of).toLocaleTimeString()}</p>{/if}
      {#each visibleBudgets as budget (budget.id)}
        {@const status = statusByBudget.get(budget.id)}
        {@const project = projects.find((item) => item.project_key === budget.project_key)}
        <div class="flex flex-wrap items-center gap-1.5 border-t border-edgerow pt-1.5 text-[11px]" data-testid="quota-budget-row">
          <span class="font-semibold text-ink">{providersStore.displayName(budget.provider)}</span>
          <span class="text-ink-muted">{project ? `· ${project.label}` : budget.project_key ? '· project unavailable' : '· all projects'}</span>
          <span class="text-ink-muted">
            {budget.unit === 'percent_of_window' ? `transcript ${budget.window_kind ?? 'window'} quota` : budget.unit === 'usd' ? `${budget.period_hours ?? 24}h USD API estimate` : `${budget.period_hours ?? 24}h tokens`}
            ≥ {formatValue(budget.threshold, budget.unit)}
          </span>
          <span class="ml-auto font-mono text-ink-muted">
            {status?.current_value != null ? `${formatValue(status.current_value, budget.unit)} now` : `Unavailable (${unavailableLabel(status?.unavailable)})`}
          </span>
          <button type="button" disabled={busy} class="text-accent hover:underline" onclick={() => beginEdit(budget)}>Edit</button>
          <button type="button" disabled={busy} class="text-ink-muted hover:text-ink" aria-label={`${budget.enabled ? 'Disable' : 'Enable'} budget`} onclick={() => toggleBudget(budget)}>{budget.enabled ? 'Enabled' : 'Disabled'}</button>
          <button type="button" disabled={busy} class="text-neg hover:underline" aria-label="Remove budget" onclick={() => removeBudget(budget.id)}>Remove</button>
        </div>
      {/each}
      <div class="flex items-center gap-2">
        <button type="button" disabled={busy} class="text-accent hover:underline" onclick={beginAdd}>Add budget</button>
      </div>

      {#if editorOpen}
        <fieldset class="grid grid-cols-1 sm:grid-cols-2 gap-2 border border-edge rounded-sm p-2 text-[11px]" disabled={busy}>
          <legend class="px-1 text-ink-muted">{editingBudget ? 'Edit budget' : 'New budget'}</legend>
          <label class="flex flex-col gap-1 text-ink-muted">Provider
            <select bind:value={draft.provider} class="bg-panel border border-edge rounded-sm px-1.5 py-1 text-ink">
              {#each (draft.unit === 'percent_of_window' ? percentProviders : visibleProviders) as provider (provider.id)}
                <option value={provider.id}>{provider.display_name}</option>
              {/each}
            </select>
          </label>
          <label class="flex flex-col gap-1 text-ink-muted">Budget type
            <select value={draft.unit} onchange={(event) => changeUnit((event.currentTarget as HTMLSelectElement).value as BudgetUnit)} class="bg-panel border border-edge rounded-sm px-1.5 py-1 text-ink">
              <option value="tokens">Tokens</option><option value="usd">USD API estimate</option>
              {#if percentProviders.length > 0}<option value="percent_of_window">Percent of transcript quota</option>{/if}
            </select>
          </label>
          {#if draft.unit !== 'percent_of_window'}
            <label class="flex flex-col gap-1 text-ink-muted">Project scope
              <select bind:value={draft.projectKey} disabled={projectLoading || !!projectError} class="bg-panel border border-edge rounded-sm px-1.5 py-1 text-ink">
                <option value="">All projects</option>
                {#if draft.projectKey && !projects.some((project) => project.project_key === draft.projectKey)}
                  <option value={draft.projectKey}>Unresolved project ({draft.projectKey})</option>
                {/if}
                {#each [...projects].sort((a, b) => a.label.localeCompare(b.label)) as project (project.project_key)}
                  <option value={project.project_key}>{project.label}</option>
                {/each}
              </select>
            </label>
            {#if projectLoading}<p role="status" class="text-ink-faint">Loading projects…</p>{/if}
            {#if projectError}<div role="alert" class="text-neg">{projectError}<button type="button" class="ml-1 underline" onclick={loadProjects}>Retry</button></div>{/if}
          {/if}
          {#if draft.unit === 'percent_of_window'}
            <label class="flex flex-col gap-1 text-ink-muted">Quota window
              <select bind:value={draft.windowKind} class="bg-panel border border-edge rounded-sm px-1.5 py-1 text-ink">
                <option value="burst">Burst</option><option value="daily">Daily</option><option value="weekly">Weekly</option><option value="monthly">Monthly</option>
              </select>
            </label>
          {:else}
            <label class="flex flex-col gap-1 text-ink-muted">Rolling period (hours)
              <input type="number" min="1" max="8760" step="1" bind:value={draft.periodHours} class="bg-panel border border-edge rounded-sm px-1.5 py-1 text-ink" />
            </label>
          {/if}
          <label class="flex flex-col gap-1 text-ink-muted">Threshold {draft.unit === 'percent_of_window' ? '(%)' : draft.unit === 'usd' ? '(USD)' : '(tokens)'}
            <input type="number" min="0.01" max={draft.unit === 'percent_of_window' ? 100 : undefined} step={draft.unit === 'usd' ? '0.01' : '1'} bind:value={draft.threshold} class="bg-panel border border-edge rounded-sm px-1.5 py-1 text-ink" />
          </label>
          <p class="sm:col-span-2 text-[10px] text-ink-faint">
            USD uses the current API base estimate, not a bill or subscription allowance. Percent budgets use transcript quota observations and are not attributed to an approved live account. Values and crossing decisions come from the backend.
          </p>
          <div class="sm:col-span-2 flex items-center gap-3">
            <button type="button" disabled={busy || !!projectError || projectLoading} class="text-accent hover:underline disabled:opacity-50" onclick={saveDraft}>Save budget</button>
            <button type="button" disabled={busy} class="text-ink-muted hover:underline" onclick={() => { editorOpen = false; editingId = null; }}>Cancel</button>
          </div>
        </fieldset>
      {/if}
    {:else if !loadError}
      <p role="status" class="text-[11px] text-ink-faint">Loading budget settings…</p>
    {/if}
    {#if saveError}<p role="alert" class="text-[11px] text-neg">{saveError}</p>{/if}
    {#if visibleAlerts.length}
      <div class="flex flex-col gap-1" aria-label="Budget alerts">
        {#each visibleAlerts as alert (alert.budget_id + alert.fired_at)}
          <p class="text-[11px] text-amber-600 bg-amber-500/10 border border-amber-500/30 rounded-sm px-1.5 py-1">{alert.message}</p>
        {/each}
      </div>
    {/if}
  </div>
</details>
