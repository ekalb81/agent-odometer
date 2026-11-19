vi.mock('./lib/liveQuota', () => ({ getLiveQuotaStatus: async () => ({ accounts: [], busy: false, configuration_error: null }), liveQuotaTrayLabel: () => null, onLiveQuotaUpdated: async () => () => {} }));
import { cleanup, fireEvent, render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import App from './App.svelte';
import { rates } from './lib/stores/rates';
import { sessionsStore } from './lib/stores/sessions.svelte';
import { scanStore } from './lib/stores/scan.svelte';
import { config } from './lib/stores/config';
import { historyStore } from './lib/stores/history.svelte';
import { get } from 'svelte/store';
import { RangeDataCache } from './lib/rangeData';
import { zeroToolMetrics } from './lib/sessionProjection';
import type { Config, HistoryStatus, RateCard, RangeTotals, ScanStatus, SessionSummary } from './lib/types';

const mocks = vi.hoisted(() => ({
  ranges: vi.fn(), quota: vi.fn(), tray: vi.fn(), compute: vi.fn(),
  ratesUpdated: null as null | ((card: RateCard) => void),
  trayProvider: null as null | ((provider: string) => void),
  startupConfig: vi.fn(), list: vi.fn(), scan: vi.fn(),
  sessionUpdated: null as null | ((session: SessionSummary) => void),
  sessionRemoved: null as null | ((id: string) => void),
  configUpdated: null as null | ((value: Config) => void),
  historyProgress: null as null | ((value: HistoryStatus) => void),
  scanProgress: null as null | ((status: ScanStatus) => void),
}));

// Keep the real App lifecycle, effects, queue and cache; child views and
// unrelated startup sources are outside this tray orchestration test.
vi.mock('./components/SessionsView.svelte', () => ({ default: () => {} }));
vi.mock('./components/SettingsView.svelte', () => ({ default: () => {} }));
vi.mock('./components/InstructionsView.svelte', () => ({ default: () => {} }));
vi.mock('./components/Filters.svelte', () => ({ default: () => {} }));
vi.mock('./lib/stores/theme.svelte', () => ({}));
vi.mock('./lib/trayTotals', () => ({ computeScopedTrayTotals: mocks.compute }));
vi.mock('./lib/ipc', async (importOriginal) => ({
  ...await importOriginal<typeof import('./lib/ipc')>(),
  getConfig: mocks.startupConfig,
  listSessions: mocks.list,
  getScanStatus: mocks.scan,
  getHistoryStatus: () => new Promise(() => {}),
  getRates: () => new Promise(() => {}),
  listProviders: async () => [],
  onSessionUpdated: async (callback: (session: SessionSummary) => void) => { mocks.sessionUpdated = callback; return () => { mocks.sessionUpdated = null; }; },
  onSessionRemoved: async (callback: (id: string) => void) => { mocks.sessionRemoved = callback; return () => { mocks.sessionRemoved = null; }; },
  onScanProgress: async (callback: (status: ScanStatus) => void) => { mocks.scanProgress = callback; return () => { mocks.scanProgress = null; }; },
  onHistoryProgress: async (callback: (value: HistoryStatus) => void) => { mocks.historyProgress = callback; return () => { mocks.historyProgress = null; }; },
  onInstructionScanProgress: async () => () => {},
  onConfigUpdated: async (callback: (value: Config) => void) => { mocks.configUpdated = callback; return () => { mocks.configUpdated = null; }; },
  onOpenSettings: async () => () => {},
  onQuotaPolicyUpdated: async () => () => {},
  onTrayProviderSelected: async (callback: (provider: string) => void) => { mocks.trayProvider = callback; return () => { mocks.trayProvider = null; }; },
  onRatesUpdated: async (callback: (card: RateCard) => void) => {
    mocks.ratesUpdated = callback;
    return () => { mocks.ratesUpdated = null; };
  },
  sessionsInRanges: mocks.ranges,
  getQuotaSnapshots: mocks.quota,
  setTrayTotals: mocks.tray,
}));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

// The compute spy observes precisely which backend result and rate-card
// identity App permits through its queue. A separate case uses the real
// projection to prove those authoritative values reach the tray.
const first = { version: 1, fetched_at: null } as RateCard;
const replacement = { ...first };
const oldResult: Record<string, RangeTotals>[] = [{ synthetic: {} as RangeTotals }];
const newResult: Record<string, RangeTotals>[] = [{ synthetic: {} as RangeTotals }];

async function flush() {
  await tick();
  await vi.advanceTimersByTimeAsync(0);
  await tick();
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.clearAllMocks();
  mocks.startupConfig.mockImplementation(() => new Promise(() => {}));
  mocks.list.mockResolvedValue([]);
  mocks.scan.mockImplementation(() => new Promise(() => {}));
  scanStore.set({ done: 0, total: 2, complete: false, elapsed_ms: null, cold_reason: null });
  mocks.ranges.mockResolvedValue(newResult);
  mocks.quota.mockResolvedValue([]);
  mocks.tray.mockResolvedValue(undefined);
  mocks.compute.mockReturnValue({ tokens: 'synthetic' });
  sessionsStore.replaceAll([{
    storage_id: 'synthetic', harness: 'codex', started_at: '2026-01-01T00:00:00Z',
    last_event_at: '2026-01-01T00:00:00Z',
  } as SessionSummary]);
  rates.set(first);
});

afterEach(() => {
  cleanup();
  rates.set(null);
  sessionsStore.replaceAll([]);
  vi.clearAllTimers();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('App tray pricing refresh', () => {
  beforeEach(() => {
    mocks.startupConfig.mockResolvedValue(get(config));
    mocks.list.mockResolvedValue([...sessionsStore.map.values()]);
  });

  it('rejects an in-flight prior provider and publishes the explicit native tray selection', async () => {
    const pending = deferred<[]>();
    mocks.quota.mockReturnValueOnce(pending.promise);
    render(App); await flush();
    mocks.trayProvider!('claude_code'); await flush();
    await vi.advanceTimersByTimeAsync(250);
    pending.resolve([]); await flush();
    expect(mocks.compute.mock.lastCall?.[3]).toBe('claude_code');
    expect(mocks.tray.mock.lastCall?.[0]).toMatchObject({ provider: 'claude_code' });
    const calls = mocks.tray.mock.calls.length;
    mocks.trayProvider!('arbitrary-provider'); await flush();
    expect(mocks.tray).toHaveBeenCalledTimes(calls);
  });
  it('clears obsolete money while retaining tokens until replacement server prices return', async () => {
    const actual = await vi.importActual<typeof import('./lib/trayTotals')>('./lib/trayTotals');
    mocks.compute.mockImplementation(actual.computeScopedTrayTotals);
    const result = (cost: number): Record<string, RangeTotals>[] => [{ synthetic: {
      tokens: { input_tokens: 100, cached_input_tokens: 0, cache_creation_input_tokens: 0,
        output_tokens: 0, reasoning_output_tokens: 0, total_tokens: 100 },
      buckets: [], tool_metrics: zeroToolMetrics(), tool_metrics_by_model: {}, optimization_findings_count: 0,
      pricing: {
        plan: { total: cost, by_model: [], missing_models: [], unpriced_models: [] },
        api: { total: cost + 2, by_model: [], missing_models: [], unpriced_models: [] },
      },
    } }];
    const pending = deferred<Record<string, RangeTotals>[]>();
    mocks.ranges.mockResolvedValueOnce(result(101)).mockReturnValueOnce(pending.promise);
    render(App);
    await flush();
    expect(mocks.tray.mock.lastCall?.[0]).toMatchObject({ codex_credits: '101.00', codex_api_usd: '$103.00' });
    const publishCount = mocks.tray.mock.calls.length;
    rates.set(replacement);
    await flush();
    expect(mocks.ranges).toHaveBeenCalledTimes(2);
    expect(mocks.tray).toHaveBeenCalledTimes(publishCount + 1);
    expect(mocks.tray.mock.lastCall?.[0]).toMatchObject({
      tokens: '100', codex_credits: 'unavailable', codex_api_usd: 'unavailable · missing direct rate',
    });
    pending.resolve(result(211));
    await flush();
    expect(mocks.tray.mock.lastCall?.[0]).toMatchObject({ codex_credits: '211.00', codex_api_usd: '$213.00' });
  });

  it('refetches the whole batch after a rates event with unchanged sessions and card version', async () => {
    render(App);
    await flush();
    expect(mocks.ranges).toHaveBeenCalledTimes(1);
    const generation = sessionsStore.mutationLog.generation;
    mocks.ratesUpdated!(replacement);
    await flush();
    expect(sessionsStore.mutationLog.generation).toBe(generation);
    expect(mocks.ranges).toHaveBeenCalledTimes(2);
    for (const call of mocks.ranges.mock.calls) { expect(call[1]).toEqual(['synthetic']); expect(call[2]).toEqual(['synthetic']); }
    expect(mocks.compute.mock.lastCall?.[2]).toBe(replacement);
  });

  it('discards an old range completion before it can repopulate the cache or update the tray', async () => {
    const pending = deferred<Record<string, RangeTotals>[]>();
    mocks.ranges.mockReturnValueOnce(pending.promise);
    const apply = vi.spyOn(RangeDataCache.prototype, 'applyFull');
    render(App);
    await flush();
    rates.set(replacement);
    await flush();
    pending.resolve(oldResult);
    await flush();
    expect(apply).toHaveBeenCalledTimes(1);
    expect(apply.mock.calls[0][2]).toBe(newResult);
    expect(mocks.tray.mock.calls.filter(([value]) => value.tokens !== 'verifying')).toHaveLength(1);
    expect(mocks.compute.mock.lastCall?.[1]).toBe(newResult[0]);
    expect(mocks.compute.mock.lastCall?.[2]).toBe(replacement);
  });

  it.each(['resolve', 'reject'] as const)('discards a stale quota %s before publishing tray totals', async (outcome) => {
    const pending = deferred<[]>();
    mocks.quota.mockReturnValueOnce(pending.promise);
    render(App);
    await flush();
    expect(mocks.quota).toHaveBeenCalledTimes(1);
    rates.set(replacement);
    await flush();
    expect(mocks.tray.mock.calls.filter(([value]) => value.tokens !== 'verifying')).toHaveLength(1);
    expect(mocks.compute.mock.lastCall?.[1]).toEqual({ synthetic: { pricing: undefined } });
    if (outcome === 'resolve') pending.resolve([]);
    else pending.reject(new Error('synthetic stale quota failure'));
    await flush();
    expect(mocks.tray.mock.calls.filter(([value]) => value.tokens !== 'verifying')).toHaveLength(2);
    expect(mocks.compute.mock.lastCall?.[1]).toBe(newResult[0]);
    expect(mocks.compute.mock.lastCall?.[2]).toBe(replacement);
    expect(mocks.ranges).toHaveBeenCalledTimes(2);
  });

  it('does not invalidate the replacement epoch when an obsolete range request rejects', async () => {
    const pending = deferred<Record<string, RangeTotals>[]>();
    mocks.ranges.mockReturnValueOnce(pending.promise);
    const invalidate = vi.spyOn(RangeDataCache.prototype, 'invalidate');
    render(App);
    await flush();
    rates.set(replacement);
    await flush();
    const invalidations = invalidate.mock.calls.length;
    pending.reject(new Error('synthetic stale range failure'));
    await flush();
    expect(invalidate).toHaveBeenCalledTimes(invalidations);
    expect(mocks.tray.mock.calls.filter(([value]) => value.tokens !== 'verifying')).toHaveLength(1);
    expect(mocks.compute.mock.lastCall?.[2]).toBe(replacement);
  });
  it('withholds cached raw totals on a combined rate and accounting mutation', async () => {
    render(App); await flush();
    mocks.compute.mockClear(); mocks.tray.mockClear();
    const pending = deferred<Record<string, RangeTotals>[]>();
    mocks.ranges.mockImplementationOnce(() => pending.promise);
    sessionsStore.applyMutations([{ ...sessionsStore.map.get('synthetic')!, thread_name: 'Updated synthetic' }], []);
    rates.set(replacement);
    await flush();
    expect(mocks.compute).not.toHaveBeenCalled();
    expect(mocks.tray.mock.lastCall?.[0]).toMatchObject({ tokens: 'verifying' });
    pending.resolve(newResult); await flush();
    expect(mocks.compute).toHaveBeenCalledTimes(1);
  });
  it('revalidates a full cached scope when the fetch subset is empty', async () => {
    render(App); await flush();
    mocks.ranges.mockClear();
    sessionsStore.remove('outside-scope'); await flush();
    await vi.advanceTimersByTimeAsync(250); await flush();
    expect(mocks.ranges.mock.lastCall?.[1]).toEqual([]);
    expect(mocks.ranges.mock.lastCall?.[2]).toEqual(['synthetic']);
  });

});

describe('App scan completion', () => {
  const complete: ScanStatus = { done: 2, total: 2, complete: true, elapsed_ms: 10, cold_reason: null };

  it('withholds completion and pricing after a list failure, then replays events on retry', async () => {
    const original = sessionsStore.map.get('synthetic')!;
    sessionsStore.replaceAll([]);
    const retry = deferred<SessionSummary[]>();
    mocks.startupConfig.mockResolvedValue(get(config));
    mocks.scan.mockResolvedValue(complete);
    mocks.list.mockRejectedValueOnce(new Error('synthetic list failure')).mockReturnValueOnce(retry.promise);
    render(App); await flush();
    mocks.sessionUpdated!({ ...original, thread_name: 'Updated during outage' });
    await vi.advanceTimersByTimeAsync(500);
    expect(scanStore.status.complete).toBe(false);
    expect(sessionsStore.map.size).toBe(0);
    expect(mocks.ranges).not.toHaveBeenCalled();
    expect(mocks.tray.mock.lastCall?.[0].tokens).toBe('unavailable');
    await fireEvent.click(screen.getByRole('button', { name: 'Retry session list' }));
    mocks.sessionRemoved!('removed');
    retry.resolve([original, { ...original, storage_id: 'removed' }]); await flush();
    expect(scanStore.status.complete).toBe(true);
    expect(sessionsStore.map.get('synthetic')?.thread_name).toBe('Updated during outage');
    expect(sessionsStore.map.has('removed')).toBe(false);
    expect(mocks.ranges.mock.lastCall?.[2]).toEqual(['synthetic']);
  });

  it('withholds prior data after a provider root change and ignores a retry superseded by history readiness', async () => {
    const previousConfig = get(config);
    const previousHistory = historyStore.status;
    const original = sessionsStore.map.get('synthetic')!;
    const retry = deferred<SessionSummary[]>();
    mocks.startupConfig.mockResolvedValue(previousConfig);
    mocks.scan.mockResolvedValue(complete);
    mocks.list.mockResolvedValueOnce([original]).mockRejectedValueOnce(new Error('replacement unavailable'))
      .mockReturnValueOnce(retry.promise).mockResolvedValueOnce([{ ...original, storage_id: 'restored' }]);
    const view = render(App);
    try {
      await flush();
      expect(scanStore.status.complete).toBe(true);
      mocks.ranges.mockClear();
      mocks.configUpdated!({ ...previousConfig, providers: { ...previousConfig.providers,
        gemini_cli: { live_roots: ['replacement'], archive_roots: [], session_index_path: null },
      } });
      await flush(); await vi.advanceTimersByTimeAsync(500);
      expect(scanStore.status.complete).toBe(false);
      expect(sessionsStore.map.has('synthetic')).toBe(true);
      expect(mocks.ranges).not.toHaveBeenCalled();
      expect(mocks.tray.mock.lastCall?.[0].tokens).toBe('unavailable');
      await fireEvent.click(screen.getByRole('button', { name: 'Retry session list' }));
      mocks.historyProgress!({ ...previousHistory, status: 'ready' }); await flush();
      retry.reject(new Error('superseded retry')); await flush();
      expect(screen.queryByRole('button', { name: 'Retry session list' })).toBeNull();
      expect(scanStore.status.complete).toBe(true);
      expect([...sessionsStore.map.keys()]).toEqual(['restored']);
    } finally {
      view.unmount();
      config.set(previousConfig);
      historyStore.set(previousHistory);
    }
  });

  it('applies the final ordered mutation batch before publishing scan completion', async () => {
    const original = sessionsStore.map.get('synthetic')!;
    mocks.startupConfig.mockResolvedValue(get(config));
    mocks.list.mockResolvedValue([original]);
    render(App); await flush();
    mocks.sessionUpdated!({ ...original, thread_name: 'Final synthetic' });
    mocks.sessionUpdated!({ ...original, storage_id: 'removed' });
    mocks.sessionRemoved!('removed');
    const published: string[] = [];
    const set = scanStore.set.bind(scanStore);
    vi.spyOn(scanStore, 'set').mockImplementation(status => {
      if (status.complete) published.push(sessionsStore.map.get('synthetic')!.thread_name!);
      set(status);
    });
    mocks.scanProgress!(complete);
    expect(published).toEqual(['Final synthetic']);
    expect(sessionsStore.map.has('removed')).toBe(false);
    expect(scanStore.status.complete).toBe(true);
  });

  it.each([
    { source: 'event', superseded: false },
    { source: 'snapshot', superseded: false },
    { source: 'event', superseded: true },
    { source: 'snapshot', superseded: true },
  ])('holds completed $source until snapshot replay (new scan: $superseded)', async ({ source, superseded }) => {
    const original = sessionsStore.map.get('synthetic')!;
    const snapshot = deferred<SessionSummary[]>();
    mocks.startupConfig.mockResolvedValue(get(config));
    mocks.list.mockReturnValue(snapshot.promise);
    if (source === 'snapshot') mocks.scan.mockResolvedValue(complete);
    render(App); await flush();
    mocks.sessionUpdated!({ ...original, thread_name: 'Final synthetic' });
    if (source === 'event') mocks.scanProgress!(complete);
    await flush();
    expect(scanStore.status.complete).toBe(false);
    if (superseded) mocks.scanProgress!({ ...complete, done: 0, complete: false });
    snapshot.resolve([original]); await flush();
    expect(scanStore.status.complete).toBe(!superseded);
    if (!superseded) expect(sessionsStore.map.get('synthetic')!.thread_name).toBe('Final synthetic');
    mocks.scanProgress!(complete);
    expect(scanStore.status.complete).toBe(true);
    expect(sessionsStore.map.get('synthetic')!.thread_name).toBe('Final synthetic');
  });
});
