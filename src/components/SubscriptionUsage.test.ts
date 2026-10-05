import { render, screen, waitFor, cleanup } from '@testing-library/svelte';
import { beforeEach, afterEach, it, expect, vi } from 'vitest';
import SubscriptionUsage from './SubscriptionUsage.svelte';
import { sessionsStore } from '../lib/stores/sessions.svelte';
const mocks = vi.hoisted(() => ({ usage: vi.fn(), ranges: vi.fn(), snapshots: vi.fn() }));
vi.mock('../lib/ipc', () => ({ getSubscriptionUsage: mocks.usage, sessionsInRanges: mocks.ranges, getQuotaSnapshots: mocks.snapshots }));
vi.mock('./QuotaBudgets.svelte', () => ({ default: () => {} }));
vi.mock('./LiveQuotaAccounts.svelte', () => ({ default: () => {} }));
vi.mock('./LiveAccountAlerts.svelte', () => ({ default: () => {} }));
beforeEach(() => {
  mocks.usage.mockReset().mockResolvedValue([{ harness: 'codex', captured_at: new Date().toISOString(), primary: null, secondary: null, plan_type: 'Synthetic plan' }]);
  mocks.ranges.mockReset().mockResolvedValue([{ a: { tokens: { total_tokens: 901 } } }, {}, {}]);
  mocks.snapshots.mockReset().mockResolvedValue([]);
});
afterEach(cleanup);
it('clears trailing numbers on identity failure while preserving separate quota observations', async () => {
  render(SubscriptionUsage, { sessionIds: ['a', 'cached-sibling'] });
  await screen.findByText(/15m 901/);
  mocks.ranges.mockRejectedValueOnce('accounting_identity_ambiguous');
  sessionsStore.remove('unrelated');
  await screen.findByText(/ambiguous accounting identities/);
  expect(screen.queryByText(/15m 901/)).not.toBeInTheDocument();
  expect(screen.getByText('Synthetic plan')).toBeInTheDocument();
  expect(mocks.ranges.mock.calls.at(-1)?.[2]).toEqual(['a', 'cached-sibling']);
  expect(mocks.usage).toHaveBeenCalledTimes(1);
});
it('rejects obsolete scope successes and errors without overwriting the current result', async () => {
  let finish!: (value: unknown) => void;
  mocks.ranges.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  const view = render(SubscriptionUsage, { sessionIds: ['a'] });
  await waitFor(() => expect(mocks.ranges).toHaveBeenCalledTimes(1));
  mocks.ranges.mockResolvedValue([{ b: { tokens: { total_tokens: 42 } } }, {}, {}]);
  await view.rerender({ sessionIds: ['b'] });
  await screen.findByText(/15m 42/);
  finish([{ a: { tokens: { total_tokens: 999 } } }, {}, {}]);
  await waitFor(() => expect(screen.queryByText(/15m 999/)).not.toBeInTheDocument());
  expect(screen.getByText(/15m 42/)).toBeInTheDocument();
  view.unmount();
});
