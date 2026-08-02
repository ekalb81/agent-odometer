import { describe, expect, it } from 'vitest';
import { MutationAccumulator, RangeDataCache } from './rangeData';
import type { RangeTotals, TokenTotals } from './types';

function totals(total: number): RangeTotals {
  const tokens: TokenTotals = {
    input_tokens: total,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_output_tokens: 0,
    total_tokens: total,
  };
  return {
    tokens,
    buckets: [],
    tool_metrics: {
      calls: 0, reads: 0, searches: 0, mutations: 0, commands: 0, other: 0,
      successes: 0, failures: 0, unknown: 0, mutation_targets: 0,
      one_shot_mutations: 0, retry_count: 0, duration_ms: 0, output_bytes: 0,
    },
    tool_metrics_by_model: {},
    optimization_findings_count: 0,
  };
}

describe('MutationAccumulator', () => {
  it('forces a full fetch before any history is observed', () => {
    const acc = new MutationAccumulator();
    expect(acc.drain()).toEqual({ changedIds: null, removedIds: [] });
    // After the first drain the accumulator tracks deltas.
    expect(acc.drain()).toEqual({ changedIds: [], removedIds: [] });
  });

  it('accumulates batches across observes and is idempotent per generation', () => {
    const acc = new MutationAccumulator();
    acc.drain();
    acc.observe({ generation: 1, changed: ['a'], removed: [], replaced: false });
    acc.observe({ generation: 1, changed: ['a'], removed: [], replaced: false });
    acc.observe({ generation: 2, changed: ['b'], removed: ['c'], replaced: false });
    const drained = acc.drain();
    expect(drained.changedIds?.sort()).toEqual(['a', 'b']);
    expect(drained.removedIds).toEqual(['c']);
    expect(acc.drain()).toEqual({ changedIds: [], removedIds: [] });
  });

  it('a removal supersedes a pending change for the same id and vice versa', () => {
    const acc = new MutationAccumulator();
    acc.drain();
    acc.observe({ generation: 1, changed: ['a'], removed: [], replaced: false });
    acc.observe({ generation: 2, changed: [], removed: ['a'], replaced: false });
    expect(acc.drain()).toEqual({ changedIds: [], removedIds: ['a'] });
    acc.observe({ generation: 3, changed: [], removed: ['b'], replaced: false });
    acc.observe({ generation: 4, changed: ['b'], removed: [], replaced: false });
    expect(acc.drain()).toEqual({ changedIds: ['b'], removedIds: [] });
  });

  it('a replace clears pending deltas and forces a full fetch', () => {
    const acc = new MutationAccumulator();
    acc.drain();
    acc.observe({ generation: 1, changed: ['a'], removed: ['b'], replaced: false });
    acc.observe({ generation: 2, changed: [], removed: [], replaced: true });
    expect(acc.drain()).toEqual({ changedIds: null, removedIds: [] });
  });
});

describe('RangeDataCache', () => {
  const key = 'range:2026-08-02';

  it('plans a full fetch when empty, on key change, and on unknown history', () => {
    const cache = new RangeDataCache();
    expect(cache.plan({ rangesKey: key, ids: ['a'], changedIds: [], removedIds: [] }))
      .toEqual({ mode: 'full' });
    cache.applyFull(key, ['a'], [{ a: totals(1) }]);
    expect(cache.plan({ rangesKey: 'other', ids: ['a'], changedIds: [], removedIds: [] }))
      .toEqual({ mode: 'full' });
    expect(cache.plan({ rangesKey: key, ids: ['a'], changedIds: null, removedIds: [] }))
      .toEqual({ mode: 'full' });
  });

  it('plans none when nothing relevant changed', () => {
    const cache = new RangeDataCache();
    cache.applyFull(key, ['a', 'b'], [{ a: totals(1) }]);
    expect(cache.plan({ rangesKey: key, ids: ['a', 'b'], changedIds: [], removedIds: [] }))
      .toEqual({ mode: 'none' });
    // A change to a session that was never covered and is not wanted is a no-op.
    expect(cache.plan({ rangesKey: key, ids: ['a', 'b'], changedIds: ['zz'], removedIds: [] }))
      .toEqual({ mode: 'none' });
  });

  it('plans deltas for changed known ids and newly wanted ids', () => {
    const cache = new RangeDataCache();
    cache.applyFull(key, ['a', 'b'], [{ a: totals(1), b: totals(2) }]);
    const plan = cache.plan({
      rangesKey: key,
      ids: ['a', 'b', 'new'],
      changedIds: ['b'],
      removedIds: [],
    });
    expect(plan.mode).toBe('delta');
    expect((plan as { fetchIds: string[] }).fetchIds.sort()).toEqual(['b', 'new']);
  });

  it('merges deltas, reusing untouched RangeTotals identities and dropping absent ids', () => {
    const cache = new RangeDataCache();
    const a = totals(1);
    const b = totals(2);
    cache.applyFull(key, ['a', 'b'], [{ a, b }]);
    const b2 = totals(20);
    const merged = cache.applyDelta(['b', 'gone'], [], [{ b: b2 }]);
    expect(merged[0].a).toBe(a); // identity preserved for untouched sessions
    expect(merged[0].b).toBe(b2);
    expect(merged[0]).not.toHaveProperty('gone');
    // A fetched id with no usage any more is removed from the cached map.
    const shrunk = cache.applyDelta(['b'], [], [{}]);
    expect(shrunk[0]).not.toHaveProperty('b');
    expect(shrunk[0].a).toBe(a);
  });

  it('applies removals without a fetch and forgets removed coverage', () => {
    const cache = new RangeDataCache();
    cache.applyFull(key, ['a', 'b'], [{ a: totals(1), b: totals(2) }]);
    const plan = cache.plan({ rangesKey: key, ids: ['a'], changedIds: [], removedIds: ['b'] });
    expect(plan.mode).toBe('delta');
    expect((plan as { fetchIds: string[] }).fetchIds).toEqual([]);
    const merged = cache.applyDelta([], ['b'], null);
    expect(merged[0]).not.toHaveProperty('b');
    // b is no longer covered: if it comes back into the wanted set it is fetched.
    const revived = cache.plan({ rangesKey: key, ids: ['a', 'b'], changedIds: [], removedIds: [] });
    expect(revived.mode).toBe('delta');
    expect((revived as { fetchIds: string[] }).fetchIds).toEqual(['b']);
  });

  it('a removal wins over a change in the same cycle', () => {
    const cache = new RangeDataCache();
    cache.applyFull(key, ['a', 'b'], [{ a: totals(1), b: totals(2) }]);
    const plan = cache.plan({
      rangesKey: key,
      ids: ['a'],
      changedIds: ['b'],
      removedIds: ['b'],
    });
    expect(plan.mode).toBe('delta');
    expect((plan as { fetchIds: string[] }).fetchIds).toEqual([]);
  });

  it('covers multiple ranges in one merge', () => {
    const cache = new RangeDataCache();
    const a0 = totals(1);
    const a1 = totals(2);
    cache.applyFull(key, ['a', 'b'], [{ a: a0 }, { a: a1, b: totals(3) }]);
    const merged = cache.applyDelta(['b'], [], [{ b: totals(30) }, {}]);
    expect(merged[0].b.tokens.total_tokens).toBe(30);
    expect(merged[1]).not.toHaveProperty('b');
    expect(merged[0].a).toBe(a0);
    expect(merged[1].a).toBe(a1);
  });

  it('invalidate drops results and coverage', () => {
    const cache = new RangeDataCache();
    cache.applyFull(key, ['a'], [{ a: totals(1) }]);
    cache.invalidate();
    expect(cache.current()).toBeNull();
    expect(cache.plan({ rangesKey: key, ids: ['a'], changedIds: [], removedIds: [] }))
      .toEqual({ mode: 'full' });
  });
});
