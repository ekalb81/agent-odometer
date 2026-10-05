import { render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { tick } from 'svelte';
import QuotaBudgets from './QuotaBudgets.svelte';
import { historyStore } from '../lib/stores/history.svelte';
import { rates } from '../lib/stores/rates';
import type { ProjectInfo, QuotaBudget, QuotaBudgetCheck, QuotaConfigWire, RateCard } from '../lib/types';

const { getQuotaBudgetStatuses, getQuotaConfig, previewControlledAction, resolveProjects, setQuotaConfig } = vi.hoisted(() => ({
  getQuotaBudgetStatuses: vi.fn(),
  getQuotaConfig: vi.fn(),
  previewControlledAction: vi.fn(),
  resolveProjects: vi.fn(),
  setQuotaConfig: vi.fn(),
}));

vi.mock('../lib/ipc', () => ({ getQuotaBudgetStatuses, getQuotaConfig, previewControlledAction, resolveProjects, setQuotaConfig }));
vi.mock('../lib/stores/providers.svelte', () => ({
  providersStore: {
    descriptors: [
      { id: 'codex', display_name: 'Codex', quota_source: true },
      { id: 'claude_code', display_name: 'Claude Code', quota_source: false },
    ],
    displayName: (id: string) => id === 'codex' ? 'Codex' : 'Claude Code',
  },
}));

const existingBudget: QuotaBudget = {
  id: 'claude-week', provider: 'claude_code', project_key: null, unit: 'tokens',
  window_kind: null, period_hours: 168, threshold: 500_000, enabled: true,
};
const config: QuotaConfigWire = {
  revision: 'revision-1', budgets: [existingBudget],
  notifications: { enabled: false, quiet_hours: null }, max_cache_age_secs: 21_600,
};
const project: ProjectInfo = {
  project_key: 'repo:odometer', label: 'agent-odometer', provenance: 'repository_root',
  member_keys: ['repo:odometer'], session_count: 12,
};
let persistedConfig = config;

function report(statuses: QuotaBudgetCheck['statuses'] = []): QuotaBudgetCheck {
  return { as_of: '2026-10-04T15:00:00.000Z', statuses, alerts: [] };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

describe('QuotaBudgets', () => {
  beforeEach(() => {
    historyStore.set({ ...historyStore.status, status: 'ready', coverage_complete: true });
    rates.set(null);
    getQuotaBudgetStatuses.mockReset().mockResolvedValue(report());
    persistedConfig = config;
    getQuotaConfig.mockReset().mockImplementation(async () => persistedConfig);
    resolveProjects.mockReset().mockResolvedValue([project]);
    setQuotaConfig.mockReset().mockImplementation(async (next: QuotaConfigWire) => {
      persistedConfig = { ...next, revision: 'revision-2' };
      return persistedConfig;
    });
    previewControlledAction.mockReset().mockResolvedValue({ target_type: 'provider_guard_configuration',
      proposed_change: 'Future reviewed guard', backup_requirement: 'Exact backup required',
      postcondition_requirement: 'Read-back required', apply_available: false, undo_available: false });
  });

  afterEach(() => vi.useRealTimers());

  it('previews an existing advisory budget without applying or undoing a guard', async () => {
    const user = userEvent.setup();
    render(QuotaBudgets);
    await screen.findByText('Soft budgets & alerts');
    await user.click(screen.getByText('Soft budgets & alerts'));
    await user.click(screen.getByRole('button', { name: 'Preview future guard' }));
    await screen.findByText('Future reviewed guard');
    expect(previewControlledAction).toHaveBeenCalledWith({ kind: 'budget_guard', draft: {
      budget_id: 'claude-week', expected_config_revision: 'revision-1',
    } });
    expect(screen.getByText(/Apply and undo are unavailable/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Apply|^Undo/ })).toBeNull();
    expect(setQuotaConfig).not.toHaveBeenCalled();
  });

  it('keeps the latest dry-run result when an older preview returns late', async () => {
    const user = userEvent.setup();
    const first = deferred<{ target_type: string; proposed_change: string; backup_requirement: string;
      postcondition_requirement: string; apply_available: false; undo_available: false }>();
    previewControlledAction.mockReset()
      .mockReturnValueOnce(first.promise)
      .mockResolvedValueOnce({ target_type: 'provider_guard_configuration',
        proposed_change: 'Latest reviewed guard', backup_requirement: 'Exact backup required',
        postcondition_requirement: 'Read-back required', apply_available: false, undo_available: false });
    render(QuotaBudgets);
    await user.click(await screen.findByText('Soft budgets & alerts'));
    const button = screen.getByRole('button', { name: 'Preview future guard' });
    await user.click(button);
    await user.click(button);
    await screen.findByText('Latest reviewed guard');
    first.resolve({ target_type: 'provider_guard_configuration', proposed_change: 'Old guard',
      backup_requirement: 'Exact backup required', postcondition_requirement: 'Read-back required',
      apply_available: false, undo_available: false });
    await tick();
    expect(screen.getByText('Latest reviewed guard')).toBeInTheDocument();
    expect(screen.queryByText('Old guard')).not.toBeInTheDocument();
  });

  it.each(['history', 'rates'] as const)('invalidates displayed budget values on %s changes and rejects late prior results', async (source) => {
    const row = (value: number) => report([{ budget_id: existingBudget.id, current_value: value, unavailable: null }]);
    getQuotaBudgetStatuses.mockResolvedValueOnce(row(123));
    render(QuotaBudgets);
    await screen.findByText('123 tokens now');
    const stale = deferred<QuotaBudgetCheck>();
    getQuotaBudgetStatuses.mockReturnValueOnce(stale.promise);
    const invalidate = (complete: boolean) => {
      if (source === 'history') historyStore.set({ ...historyStore.status, status: 'ready', coverage_complete: complete });
      // Only object identity changes; pricing invalidation must not use version.
      else rates.set({ version: 13 } as RateCard);
    };
    invalidate(false);
    await waitFor(() => expect(getQuotaBudgetStatuses).toHaveBeenCalledTimes(2));
    expect(screen.queryByText('123 tokens now')).not.toBeInTheDocument();
    getQuotaBudgetStatuses.mockResolvedValueOnce(row(456));
    invalidate(true);
    await screen.findByText('456 tokens now');
    stale.resolve(row(999));
    await tick();
    expect(screen.queryByText('999 tokens now')).not.toBeInTheDocument();
    expect(screen.getByText('456 tokens now')).toBeInTheDocument();
  });

  it('keeps rows scoped while presentation reads cannot deliver an alert edge', async () => {
    const alert = {
      budget_id: existingBudget.id, provider: 'claude_code', project_key: null,
      message: 'Claude budget crossed', current_value: 600_000, threshold: 500_000,
      fired_at: '2026-10-04T15:00:00.000Z',
    };
    getQuotaBudgetStatuses.mockResolvedValue({ ...report(), alerts: [] });
    const onAlerts = vi.fn();
    render(QuotaBudgets, { harness: 'codex', onAlerts });

    await waitFor(() => expect(getQuotaBudgetStatuses).toHaveBeenCalled());
    expect(onAlerts).not.toHaveBeenCalled();
    expect(screen.queryByTestId('quota-budget-row')).not.toBeInTheDocument();
    expect(screen.queryByText(alert.message)).not.toBeInTheDocument();
  });

  it('adds a project USD budget and preserves the existing other-provider budget and revision', async () => {
    const user = userEvent.setup();
    render(QuotaBudgets);
    await screen.findByText(/500,000 tokens/);
    expect(getQuotaBudgetStatuses).toHaveBeenCalledTimes(1);

    await user.click(screen.getByText('Soft budgets & alerts'));
    await user.click(screen.getByRole('button', { name: 'Add budget' }));
    await screen.findByRole('option', { name: 'agent-odometer' });
    await user.selectOptions(screen.getByLabelText('Provider'), 'claude_code');
    await user.selectOptions(screen.getByLabelText('Budget type'), 'usd');
    await user.selectOptions(screen.getByLabelText('Project scope'), 'repo:odometer');
    await user.clear(screen.getByLabelText('Rolling period (hours)'));
    await user.type(screen.getByLabelText('Rolling period (hours)'), '168');
    await user.clear(screen.getByLabelText('Threshold (USD)'));
    await user.type(screen.getByLabelText('Threshold (USD)'), '25');
    await user.click(screen.getByRole('button', { name: 'Save budget' }));

    await waitFor(() => expect(setQuotaConfig).toHaveBeenCalledTimes(1));
    const saved = setQuotaConfig.mock.calls[0][0] as QuotaConfigWire;
    expect(saved.revision).toBe('revision-1');
    expect(saved.notifications).toEqual(config.notifications);
    expect(saved.budgets).toHaveLength(2);
    expect(saved.budgets[0]).toEqual(existingBudget);
    expect(saved.budgets[1]).toMatchObject({
      provider: 'claude_code', project_key: 'repo:odometer', unit: 'usd',
      period_hours: 168, threshold: 25, enabled: true,
    });
  });

  it('renders unavailable values honestly and leaves the saved config intact when a save fails', async () => {
    const user = userEvent.setup();
    getQuotaBudgetStatuses.mockResolvedValue(report([
      { budget_id: existingBudget.id, current_value: null, unavailable: 'pricing_incomplete' },
    ]));
    setQuotaConfig.mockRejectedValueOnce('settings changed; reload before saving');
    render(QuotaBudgets);

    expect(await screen.findByText('Unavailable (Pricing unavailable)')).toBeInTheDocument();
    const row = screen.getByTestId('quota-budget-row');
    await user.click(within(row).getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Threshold (tokens)'));
    await user.type(screen.getByLabelText('Threshold (tokens)'), '600000');
    await user.click(screen.getByRole('button', { name: 'Save budget' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('settings changed');
    expect(config.budgets[0].threshold).toBe(500_000);
  });

  it('edits, disables, and removes an existing budget while preserving each returned revision', async () => {
    const user = userEvent.setup();
    render(QuotaBudgets);
    await screen.findByTestId('quota-budget-row');
    await user.click(screen.getByText('Soft budgets & alerts'));
    const row = screen.getByTestId('quota-budget-row');

    await user.click(within(row).getByRole('button', { name: 'Disable budget' }));
    await waitFor(() => expect(setQuotaConfig).toHaveBeenCalledTimes(1));
    expect(setQuotaConfig.mock.calls[0][0].revision).toBe('revision-1');
    expect(setQuotaConfig.mock.calls[0][0].budgets[0].enabled).toBe(false);
    await setQuotaConfig.mock.results[0].value;
    await tick();
    await waitFor(() => expect(screen.getByRole('button', { name: 'Enable budget' })).toBeInTheDocument());

    await user.click(within(row).getByRole('button', { name: 'Edit' }));
    await user.clear(screen.getByLabelText('Threshold (tokens)'));
    await user.type(screen.getByLabelText('Threshold (tokens)'), '600000');
    await user.click(screen.getByRole('button', { name: 'Save budget' }));
    await waitFor(() => expect(setQuotaConfig).toHaveBeenCalledTimes(2));
    await setQuotaConfig.mock.results[1].value;
    await tick();
    await waitFor(() => expect(within(row).getByText(/600,000 tokens/)).toBeInTheDocument());
    expect(setQuotaConfig.mock.calls[1][0].revision).toBe('revision-2');
    expect(setQuotaConfig.mock.calls[1][0].budgets[0]).toMatchObject({ threshold: 600_000, enabled: false });

    await user.click(within(row).getByRole('button', { name: 'Remove budget' }));
    await waitFor(() => expect(setQuotaConfig).toHaveBeenCalledTimes(3));
    await setQuotaConfig.mock.results[2].value;
    await tick();
    expect(setQuotaConfig.mock.calls[2][0].budgets).toEqual([]);
  });

  it('ignores a slow stale refresh after a newer budget report has arrived', async () => {
    vi.useFakeTimers();
    const oldConfig = deferred<QuotaConfigWire>();
    const oldReport = deferred<QuotaBudgetCheck>();
    const freshBudget = { ...existingBudget, id: 'fresh-budget', threshold: 900_000 };
    getQuotaConfig.mockReset().mockReturnValueOnce(oldConfig.promise).mockResolvedValueOnce({ ...config, budgets: [freshBudget] });
    getQuotaBudgetStatuses.mockReset().mockReturnValueOnce(oldReport.promise).mockResolvedValueOnce(report());
    render(QuotaBudgets);

    await tick();
    await vi.advanceTimersByTimeAsync(60_000);
    await tick();
    expect(screen.getByText(/900,000 tokens/)).toBeInTheDocument();
    oldConfig.resolve(config);
    oldReport.resolve(report());
    await tick();
    expect(screen.getByText(/900,000 tokens/)).toBeInTheDocument();
    expect(screen.queryByText(/500,000 tokens/)).not.toBeInTheDocument();
  });
});
