import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor } from '@testing-library/svelte';
import LiveAccountAlerts from './LiveAccountAlerts.svelte';
import type { QuotaConfigWire } from '../lib/types';
import type { LiveQuotaStatus } from '../lib/liveQuota';
const mocks = vi.hoisted(() => ({ config: vi.fn(), status: vi.fn(), save: vi.fn(), policy: null as (() => void) | null }));
vi.mock('../lib/ipc', () => ({ getQuotaConfig: mocks.config, setQuotaConfig: mocks.save }));
vi.mock('../lib/liveQuota', () => ({ getLiveQuotaStatus: mocks.status, onLiveQuotaUpdated: vi.fn(async () => () => {}) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (_event, callback) => { mocks.policy = callback; return () => {}; }) }));
vi.mock('../lib/stores/ambient', () => ({ invalidateAmbient: vi.fn() }));
let config: QuotaConfigWire;
beforeEach(() => {
  const now = new Date().toISOString();
  config = { revision: 'r1', budgets: [], live_account_budgets: [], notifications: { enabled: true, quiet_hours: [22, 7], ambient: { attention: true, provider_incidents: false, stale_quota: false, retention_risk: false } }, max_cache_age_secs: 21600 };
  mocks.config.mockReset().mockResolvedValue(config);
  mocks.save.mockReset().mockImplementation(async next => next);
  const status: LiveQuotaStatus = { busy: false, configuration_error: null, accounts: [{ provider: 'codex', consent: { account_id: 'synthetic-account', consented_at: now, enabled: true, label: 'Synthetic account' }, observed_at: now, ordinary_usage_allowed: true, unavailable: null, buckets: [{ limit_id: 'model-limit', limit_name: 'Model limit', spend_control_reached: null, snapshot: { provider: 'codex', provenance: 'live_provider', unavailable: null, windows: [{ kind: 'burst', unit: 'percent', window_minutes: 300, used: 85, remaining: 15, limit: 100, unlimited: false, resets_at: null, window_started_at: null, window_started_at_estimated: false, window_start_basis: 'unknown', observed_at: now, confidence: 'medium', stale: false, unavailable: null, forecast: null }] } }] }] };
  mocks.status.mockReset().mockResolvedValue(status);
});
afterEach(cleanup);
it('creates disabled exact rules while preserving shared policy and revision', async () => {
  render(LiveAccountAlerts);
  const choice = await screen.findByRole('option', { name: /Synthetic account/ });
  await fireEvent.change(screen.getByLabelText('Approved account and window'), { target: { value: (choice as HTMLOptionElement).value } });
  expect(screen.getByLabelText('Enable this new account rule')).not.toBeChecked();
  await fireEvent.click(screen.getByRole('button', { name: 'Add account rule' }));
  await waitFor(() => expect(mocks.save).toHaveBeenCalledOnce());
  const saved = mocks.save.mock.calls[0][0];
  expect(saved.revision).toBe('r1'); expect(saved.notifications).toEqual(config.notifications);
  await waitFor(() => expect(screen.getByText('Configured windows already have a rule. Enable, disable, or remove the existing rule below.')).toBeInTheDocument());
  expect(screen.getByRole('button', { name: 'Add account rule' })).toBeDisabled();
  expect(saved.live_account_budgets[0]).toMatchObject({ account_id: 'synthetic-account', limit_id: 'model-limit', window_minutes: 300, enabled: false, threshold_percent: 80 });
});
it('rejects a delayed load after unmount and leaves unavailable saves disabled', async () => {
  let resolve!: (value: QuotaConfigWire) => void;
  mocks.config.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  const view = render(LiveAccountAlerts); view.unmount(); resolve(config);
  await Promise.resolve(); expect(screen.queryByRole('button', { name: 'Add account rule' })).not.toBeInTheDocument();
  mocks.config.mockRejectedValueOnce(new Error('private backend details'));
  render(LiveAccountAlerts);
  expect(await screen.findByText('Account alert settings unavailable.')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Add account rule' })).not.toBeInTheDocument();
  expect(screen.queryByText('private backend details')).not.toBeInTheDocument();
});

it('rejects an obsolete save response after an authoritative policy refresh', async () => {
  let resolve!: (value: QuotaConfigWire) => void;
  mocks.save.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
  render(LiveAccountAlerts);
  const choice = await screen.findByRole('option', { name: /Synthetic account/ });
  await fireEvent.change(screen.getByLabelText('Approved account and window'), { target: { value: (choice as HTMLOptionElement).value } });
  await fireEvent.click(screen.getByRole('button', { name: 'Add account rule' }));
  await waitFor(() => expect(mocks.save).toHaveBeenCalledOnce());
  const obsolete = mocks.save.mock.calls[0][0];
  mocks.config.mockResolvedValue({ ...config, revision: 'newer', live_account_budgets: [] });
  mocks.policy?.();
  await waitFor(() => expect(screen.getByRole('button', { name: 'Refresh account alert settings' })).not.toBeDisabled());
  resolve(obsolete); await Promise.resolve(); await Promise.resolve();
  expect(screen.queryByRole('button', { name: 'Remove account rule' })).not.toBeInTheDocument();
  expect(screen.queryByText('Account rules saved. Shared alerts and quiet hours apply.')).not.toBeInTheDocument();
});

it('keeps stale saved accounts distinguishable without claiming revoked consent is active', async () => {
  const cached = await mocks.status();
  const consent = cached.accounts[0].consent;
  cached.accounts[0].consent.enabled = false;
  cached.accounts[0].consent.label = 'Paused work account';
  config.live_account_budgets = [{ id: 'paused', account_id: consent.account_id, consented_at: consent.consented_at, limit_id: 'model-limit', window_kind: 'burst', window_minutes: 300, threshold_percent: 80, enabled: true }, { id: 'revoked', account_id: 'synthetic-revoked-account', consented_at: consent.consented_at, limit_id: 'other-limit', window_kind: 'burst', window_minutes: 300, threshold_percent: 80, enabled: true }];
  render(LiveAccountAlerts);
  expect(await screen.findByText(/Paused work account.*polling paused/)).toBeInTheDocument();
  expect(screen.getByText(/Saved account synthetic-revoked-account.*saved consent unavailable/)).toBeInTheDocument();
});
