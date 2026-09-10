import { cleanup, render } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import App from './App.svelte';
import { rates } from './lib/stores/rates';
import { sessionsStore } from './lib/stores/sessions.svelte';
import { RangeDataCache } from './lib/rangeData';
import { zeroToolMetrics } from './lib/sessionProjection';
import type { RateCard, RangeTotals, SessionSummary } from './lib/types';

const mocks = vi.hoisted(() => ({
  ranges: vi.fn(), quota: vi.fn(), tray: vi.fn(), compute: vi.fn(),
  ratesUpdated: null as null | ((card: RateCard) => void),
}));

// Keep the real App lifecycle, effects, queue and cache; child views and
// unrelated startup sources are outside this tray orchestration test.
vi.mock('./components/SessionsView.svelte', () => ({ default: () => {} }));
vi.mock('./components/SettingsView.svelte', () => ({ default: () => {} }));
vi.mock('./components/InstructionsView.svelte', () => ({ default: () => {} }));
vi.mock('./components/Filters.svelte', () => ({ default: () => {} }));
vi.mock('./lib/stores/theme.svelte', () => ({}));
vi.mock('./lib/trayTotals', () => ({ computeTrayTotals: mocks.compute }));
vi.mock('./lib/ipc', async (importOriginal) => ({
  ...await importOriginal<typeof import('./lib/ipc')>(),
  getConfig: () => new Promise(() => {}),
  onSessionUpdated: async () => () => {},
  onSessionRemoved: async () => () => {},
  onScanProgress: async () => () => {},
  onHistoryProgress: async () => () => {},
  onInstructionScanProgress: async () => () => {},
  onConfigUpdated: async () => () => {},
  onOpenSettings: async () => () => {},
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
  it('clears obsolete money while retaining tokens until replacement server prices return', async () => {
    const actual = await vi.importActual<typeof import('./lib/trayTotals')>('./lib/trayTotals');
    mocks.compute.mockImplementation(actual.computeTrayTotals);
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
    for (const call of mocks.ranges.mock.calls) expect(call).toHaveLength(1);
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
    expect(mocks.tray).toHaveBeenCalledTimes(1);
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
    expect(mocks.tray).toHaveBeenCalledTimes(1);
    expect(mocks.compute.mock.lastCall?.[1]).toEqual({ synthetic: { pricing: undefined } });
    if (outcome === 'resolve') pending.resolve([]);
    else pending.reject(new Error('synthetic stale quota failure'));
    await flush();
    expect(mocks.tray).toHaveBeenCalledTimes(2);
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
    expect(mocks.tray).toHaveBeenCalledTimes(1);
    expect(mocks.compute.mock.lastCall?.[2]).toBe(replacement);
  });
});
