import { render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { tick } from 'svelte';
import QuotaBudgets from './QuotaBudgets.svelte';
import type { ProjectInfo, QuotaBudget, QuotaBudgetCheck, QuotaConfigWire } from '../lib/types';

const { checkQuotaBudgets, getQuotaConfig, resolveProjects, setQuotaConfig } = vi.hoisted(() => ({
  checkQuotaBudgets: vi.fn(),
  getQuotaConfig: vi.fn(),
  resolveProjects: vi.fn(),
  setQuotaConfig: vi.fn(),
}));

vi.mock('../lib/ipc', () => ({ checkQuotaBudgets, getQuotaConfig, resolveProjects, setQuotaConfig }));
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
    checkQuotaBudgets.mockReset().mockResolvedValue(report());
    persistedConfig = config;
    getQuotaConfig.mockReset().mockImplementation(async () => persistedConfig);
    resolveProjects.mockReset().mockResolvedValue([project]);
    setQuotaConfig.mockReset().mockImplementation(async (next: QuotaConfigWire) => {
      persistedConfig = { ...next, revision: 'revision-2' };
      return persistedConfig;
    });
  });

  afterEach(() => vi.useRealTimers());

  it('notifies every provider crossing while keeping budget rows scoped to the selected harness', async () => {
    const alert = {
      budget_id: existingBudget.id, provider: 'claude_code', project_key: null,
      message: 'Claude budget crossed', current_value: 600_000, threshold: 500_000,
      fired_at: '2026-10-04T15:00:00.000Z',
    };
    checkQuotaBudgets.mockResolvedValue({ ...report(), alerts: [alert] });
    const onAlerts = vi.fn();
    render(QuotaBudgets, { harness: 'codex', onAlerts });

    await waitFor(() => expect(onAlerts).toHaveBeenCalledWith([alert]));
    expect(screen.queryByTestId('quota-budget-row')).not.toBeInTheDocument();
    expect(screen.queryByText(alert.message)).not.toBeInTheDocument();
  });

  it('adds a project USD budget and preserves the existing other-provider budget and revision', async () => {
    const user = userEvent.setup();
    render(QuotaBudgets);
    await screen.findByText(/500,000 tokens/);
    expect(checkQuotaBudgets).toHaveBeenCalledTimes(1);

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
    checkQuotaBudgets.mockResolvedValue(report([
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
    checkQuotaBudgets.mockReset().mockReturnValueOnce(oldReport.promise).mockResolvedValueOnce(report());
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
