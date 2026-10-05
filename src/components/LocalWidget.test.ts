import { render, screen, waitFor } from '@testing-library/svelte';
import { act } from '@testing-library/svelte';
import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import LocalWidget from './LocalWidget.svelte';
import type { WidgetSettings, WidgetSnapshot } from '../lib/types';

const mocks = vi.hoisted(() => ({ getWidgetSettings: vi.fn(), getWidgetSnapshot: vi.fn(), onWidgetSettingsUpdated: vi.fn(), onWidgetDataChanged: vi.fn(), onRatesUpdated: vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
vi.mock('../lib/stores/theme.svelte', () => ({ themeStore: {} }));
const enabled: WidgetSettings = { version: 1, revision: 1, preferences: { visible: true, provider: 'codex', kind: 'usage', always_on_top: false } };
const usage: WidgetSnapshot = { settings: enabled, computed_at: '2026-01-01T00:00:00Z', quota: null, usage: {
  session_count: 1, total_tokens: 123, latest_activity_at: null, plan_amount: 10, plan_currency: 'credits', api_amount_usd: 2, estimate_partial: true, scan_complete: false,
} };
let settingsEvent: (value: WidgetSettings) => void;
let dataEvent: () => void;
let ratesEvent: () => void;
let unsubscribe: ReturnType<typeof vi.fn>;
beforeEach(() => {
  vi.resetAllMocks(); unsubscribe = vi.fn();
  mocks.getWidgetSettings.mockResolvedValue(structuredClone(enabled));
  mocks.getWidgetSnapshot.mockResolvedValue(structuredClone(usage));
  mocks.onWidgetSettingsUpdated.mockImplementation(async callback => { settingsEvent = callback; return unsubscribe; });
  mocks.onWidgetDataChanged.mockImplementation(async callback => { dataEvent = callback; return [unsubscribe]; });
  mocks.onRatesUpdated.mockImplementation(async callback => { ratesEvent = callback; return unsubscribe; });
});
afterEach(() => vi.useRealTimers());
it('does not query while disabled and removes all listeners on unmount', async () => {
  mocks.getWidgetSettings.mockResolvedValue({ ...enabled, preferences: { ...enabled.preferences, visible: false } });
  const view = render(LocalWidget);
  await screen.findByText(/Widget disabled/);
  expect(mocks.getWidgetSnapshot).not.toHaveBeenCalled();
  view.unmount(); expect(unsubscribe).toHaveBeenCalledTimes(3);
  dataEvent(); ratesEvent(); settingsEvent(enabled);
  expect(mocks.getWidgetSnapshot).not.toHaveBeenCalled();
});
it('shows explicit partial coverage, recorded activity and fallback pricing separately', async () => {
  render(LocalWidget);
  await screen.findByText('123 tokens');
  expect(screen.getByText(/Partial snapshot/)).toBeInTheDocument();
  expect(screen.getByText(/Partial or fallback estimate/)).toBeInTheDocument();
  expect(screen.getByText(/Latest source activity: no recorded activity/)).toBeInTheDocument();
  expect(screen.getByText(/Reset evidence: not applicable/)).toBeInTheDocument();
  expect(screen.getByText(/Snapshot computed:/)).toHaveTextContent(usage.computed_at);
});
it('keeps stale values and distinguishes recorded zero, unlimited and unavailable', async () => {
  const window = { kind: 'burst' as const, unit: 'percent' as const, used: 100, remaining: 0, unlimited: false, observed_at: '2025-12-30T00:00:00Z', resets_at: '2025-12-31T00:00:00Z', stale: true, unavailable: null };
  mocks.getWidgetSnapshot.mockResolvedValue({ ...usage, usage: null, quota: { provenance: 'transcript_derived', unavailable: null, windows_omitted: 2, windows: [window, { ...window, kind: 'weekly', unlimited: true }, { ...window, kind: 'daily', remaining: null, used: null }] } });
  render(LocalWidget);
  await screen.findByText('Recorded 0% remaining');
  expect(screen.getByText('Unlimited')).toBeInTheDocument();
  expect(screen.getByText('Remaining unavailable')).toBeInTheDocument();
  expect(screen.getAllByText(/Stale observation/)).toHaveLength(3);
  expect(screen.getAllByText(/Reset evidence: 2025-12-31/)).toHaveLength(3);
  expect(screen.getByText(/2 further windows omitted/)).toBeInTheDocument();
});
it('invalidates prices immediately and rejects superseded reads while coalescing event bursts', async () => {
  render(LocalWidget); await screen.findByText('123 tokens');
  vi.useFakeTimers();
  let finish!: (value: WidgetSnapshot) => void;
  mocks.getWidgetSnapshot.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
  await act(() => { dataEvent(); dataEvent(); dataEvent(); });
  expect(mocks.getWidgetSnapshot).toHaveBeenCalledTimes(1);
  await act(() => vi.advanceTimersByTimeAsync(5000));
  expect(mocks.getWidgetSnapshot).toHaveBeenCalledTimes(2);
  await act(() => ratesEvent());
  expect(screen.getByText('123 tokens')).toBeInTheDocument();
  expect(screen.getByText('Plan estimate: unavailable')).toBeInTheDocument();
  await act(() => finish({ ...usage, usage: { ...usage.usage!, total_tokens: 999 } }));
  expect(screen.queryByText('999 tokens')).not.toBeInTheDocument();
  await act(() => vi.advanceTimersByTimeAsync(5000));
  expect(mocks.getWidgetSnapshot).toHaveBeenCalledTimes(3);
  expect(screen.getByText('123 tokens')).toBeInTheDocument();
});
it('rejects old errors and snapshots when the provider or visibility changes', async () => {
  let reject!: (error: Error) => void;
  mocks.getWidgetSnapshot.mockImplementationOnce(() => new Promise((_resolve, fail) => { reject = fail; }));
  render(LocalWidget); await waitFor(() => expect(mocks.getWidgetSnapshot).toHaveBeenCalledTimes(1));
  await act(() => settingsEvent({ ...enabled, revision: 2, preferences: { ...enabled.preferences, visible: false, provider: 'claude_code' } }));
  await act(() => reject(new Error('private old failure')));
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  expect(screen.getByText(/Widget disabled/)).toBeInTheDocument();
  expect(screen.queryByText('123 tokens')).not.toBeInTheDocument();
});
it('does not allow a late initial settings read to undo a settings event', async () => {
  let finish!: (value: WidgetSettings) => void;
  mocks.getWidgetSettings.mockImplementation(() => new Promise(resolve => { finish = resolve; }));
  render(LocalWidget); await waitFor(() => expect(mocks.getWidgetSettings).toHaveBeenCalled());
  await act(() => settingsEvent({ ...enabled, revision: 2, preferences: { ...enabled.preferences, visible: false } }));
  await act(() => finish(enabled));
  expect(screen.getByText(/Widget disabled/)).toBeInTheDocument();
  expect(mocks.getWidgetSnapshot).not.toHaveBeenCalled();
});
