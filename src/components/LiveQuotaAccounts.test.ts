import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { tick } from 'svelte';
import LiveQuotaAccounts from './LiveQuotaAccounts.svelte';
import type { LiveQuotaStatus } from '../lib/liveQuota';
const mocks = vi.hoisted(() => ({ status: vi.fn(), identify: vi.fn(), approve: vi.fn(), change: vi.fn() }));
vi.mock('../lib/liveQuota', async (original) => ({ ...await original<typeof import('../lib/liveQuota')>(),
  getLiveQuotaStatus: mocks.status, identifyQuotaAccount: mocks.identify, approveQuotaAccount: mocks.approve, changeQuotaAccount: mocks.change }));
beforeEach(() => {
  vi.resetAllMocks();
  mocks.status.mockResolvedValue({ accounts: [], busy: false, configuration_error: null });
  mocks.identify.mockResolvedValue({ account_id: 'synthetic-account', plan_type: 'pro' });
  mocks.approve.mockResolvedValue(undefined);
});
it('does not use credentials on mount and requires account approval after an explicit lookup', async () => {
  render(LiveQuotaAccounts);
  await screen.findByText('No accounts approved. Live polling is off.');
  expect(mocks.identify).not.toHaveBeenCalled(); expect(mocks.approve).not.toHaveBeenCalled();
  await userEvent.click(screen.getByText('Live account quotas'));
  await userEvent.click(screen.getByRole('button', { name: 'Allow one account lookup' }));
  await screen.findByText('synthetic-account');
  expect(mocks.approve).not.toHaveBeenCalled();
  await userEvent.clear(screen.getByLabelText('Local account label'));
  await userEvent.type(screen.getByLabelText('Local account label'), 'Work');
  await userEvent.click(screen.getByRole('button', { name: 'Enable polling for this account' }));
  await waitFor(() => expect(mocks.approve).toHaveBeenCalledWith('synthetic-account', 'Work'));
});
it('rejects a late identity result after switching out of the active view', async () => {
  let resolve!: (value: { account_id: string; plan_type: string }) => void;
  mocks.identify.mockImplementation(() => new Promise((done) => { resolve = done; }));
  const view = render(LiveQuotaAccounts);
  await userEvent.click(screen.getByText('Live account quotas'));
  await userEvent.click(screen.getByRole('button', { name: 'Allow one account lookup' }));
  await view.rerender({ active: false });
  resolve({ account_id: 'late-account', plan_type: 'pro' });
  await waitFor(() => expect(screen.queryByText('late-account')).not.toBeInTheDocument());
  expect(mocks.approve).not.toHaveBeenCalled();
});

afterEach(() => vi.useRealTimers());
it.each(['stale_observation', 'clock_skew'])('qualifies %s observations and suppresses current access claims', async (reason) => {
  const observed = '2026-01-01T00:00:00Z';
  const status: LiveQuotaStatus = { busy: false, configuration_error: null, accounts: [{
    provider: 'codex', consent: { account_id: 'synthetic-account', label: 'Work', enabled: true, consented_at: observed },
    observed_at: observed, ordinary_usage_allowed: false, unavailable: reason,
    buckets: [{ limit_id: 'codex', limit_name: null, spend_control_reached: true, snapshot: {
      provider: 'codex', provenance: 'live_provider', unavailable: null, windows: [],
    } }],
  }] };
  mocks.status.mockResolvedValue(status);
  render(LiveQuotaAccounts);
  expect(await screen.findByText(/current usage permission is unavailable/)).toBeInTheDocument();
  expect(screen.queryByText('Provider reports ordinary included usage is blocked.')).not.toBeInTheDocument();
  expect(screen.queryByText('Provider spend control reached.')).not.toBeInTheDocument();
});
it('does not resurrect an account when a pre-revoke status read resolves late', async () => {
  vi.useFakeTimers();
  const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
  const account = { provider: 'codex', consent: { account_id: 'account-a', label: 'Account A', enabled: true, consented_at: '2026-01-01T00:00:00Z' },
    observed_at: null, ordinary_usage_allowed: null, buckets: [], unavailable: 'no_observation' };
  const prior = { accounts: [account], busy: false, configuration_error: null };
  let resolve!: (value: typeof prior) => void;
  mocks.status.mockReset().mockResolvedValueOnce(prior).mockImplementationOnce(() => new Promise((done) => { resolve = done; }))
    .mockResolvedValue({ accounts: [], busy: false, configuration_error: null });
  mocks.change.mockResolvedValue(undefined);
  render(LiveQuotaAccounts);
  await tick(); await tick();
  await user.click(screen.getByText('Live account quotas'));
  await vi.advanceTimersByTimeAsync(30_000);
  await user.click(screen.getByRole('button', { name: 'Revoke consent' }));
  await tick(); await tick();
  expect(screen.queryByText('Account A')).not.toBeInTheDocument();
  resolve(prior);
  await tick(); await tick();
  expect(screen.queryByText('Account A')).not.toBeInTheDocument();
});


it('shows provider headroom and next reset without inventing a last reset', async () => {
  const observed = new Date().toISOString();
  const nextReset = new Date(Date.now() + 3_600_000).toISOString();
  const status: LiveQuotaStatus = { busy: false, configuration_error: null, accounts: [{
    provider: 'codex', consent: { account_id: 'synthetic-account', label: 'Work', enabled: true, consented_at: observed },
    observed_at: observed, ordinary_usage_allowed: true, unavailable: null,
    buckets: [{ limit_id: 'codex', limit_name: null, spend_control_reached: false, snapshot: {
      provider: 'codex', provenance: 'live_provider', unavailable: null, windows: [{
        kind: 'burst', unit: 'percent', window_minutes: 300, used: 40, remaining: 60, limit: 100,
        unlimited: false, resets_at: nextReset, window_started_at: null, window_started_at_estimated: false,
        window_start_basis: 'unknown', observed_at: observed, confidence: 'medium', stale: false,
        unavailable: null, forecast: null,
      }],
    } }],
  }] };
  mocks.status.mockResolvedValue(status);
  render(LiveQuotaAccounts);
  expect(await screen.findByText('Last reset not reported.')).toBeInTheDocument();
  expect(screen.getByText(/60% left/)).toHaveTextContent(/Resets/);
  expect(screen.queryByText(/Reset observed|Usage decrease observed/)).not.toBeInTheDocument();
  expect(mocks.identify).not.toHaveBeenCalled();
});
