// Delta-refresh bookkeeping for sessions_in_ranges consumers.
//
// The tray, analytics band, and date-scoped table each keep per-range rollup
// maps fetched from the backend. Live watcher activity mutates one or two
// sessions per flush, so refetching every session's rollups on each flush is
// O(corpus) work repeated ~1/s for hours. These helpers let a consumer fetch
// only the sessions whose rollups can have changed and merge them into its
// cached results.
//
// Correctness rests on one invariant: a session's rollup for any window can
// only change when that session is upserted (every transcript change parses
// into an upsert). Cached rollups for unchanged sessions therefore stay valid
// even while an open-ended window's "to" bound advances with the clock. The
// one drift this admits — a session carrying an event timestamped in the
// future relative to an earlier fetch — self-corrects on that session's next
// upsert or on the next full refetch (range-key change, snapshot replace).

import type { RangeTotals } from './types';

/** One coalesced store flush, published by the sessions store. */
export interface SessionMutationLog {
  generation: number;
  changed: string[];
  removed: string[];
  /** True when the whole collection was replaced (startup, config reload). */
  replaced: boolean;
}

/** Accumulates mutation batches between debounced fetch cycles. */
export class MutationAccumulator {
  private lastGeneration = -1;
  private changed = new Set<string>();
  private removed = new Set<string>();
  private replaced = true; // Unknown history — first drain forces a full fetch.

  /** Fold one mutation log into the pending set. Idempotent per generation. */
  observe(log: SessionMutationLog): void {
    if (log.generation === this.lastGeneration) return;
    this.lastGeneration = log.generation;
    if (log.replaced) {
      this.replaced = true;
      this.changed.clear();
      this.removed.clear();
      return;
    }
    for (const id of log.changed) {
      this.changed.add(id);
      this.removed.delete(id);
    }
    for (const id of log.removed) {
      this.removed.add(id);
      this.changed.delete(id);
    }
  }

  /** Take the pending work. `changedIds: null` means "history unknown — refetch". */
  drain(): { changedIds: string[] | null; removedIds: string[] } {
    const out = {
      changedIds: this.replaced ? null : [...this.changed],
      removedIds: [...this.removed],
    };
    this.replaced = false;
    this.changed.clear();
    this.removed.clear();
    return out;
  }
}

export interface RangePlanInput {
  /** Signature of the requested range layout; any change forces a full fetch. */
  rangesKey: string;
  /** Every session id the consumer wants covered. */
  ids: readonly string[];
  /** Ids mutated since the last cycle; null forces a full fetch. */
  changedIds: readonly string[] | null;
  /** Ids removed from the store since the last cycle. */
  removedIds: readonly string[];
}

export type RangePlan =
  | { mode: 'full' }
  | { mode: 'delta'; fetchIds: string[] }
  | { mode: 'none' };

/**
 * Cached per-range rollup maps plus the coverage set they were fetched for.
 * `plan` decides how much fetching a cycle needs; `applyFull`/`applyDelta`
 * merge fetch results and return fresh Record identities while reusing the
 * RangeTotals objects of untouched sessions (so identity-keyed price caches
 * survive merges).
 */
export class RangeDataCache {
  private rangesKey: string | null = null;
  private known = new Set<string>();
  private results: Record<string, RangeTotals>[] | null = null;

  plan(input: RangePlanInput): RangePlan {
    if (
      this.results === null ||
      this.rangesKey !== input.rangesKey ||
      input.changedIds === null
    ) {
      return { mode: 'full' };
    }
    const fetch = new Set<string>();
    for (const id of input.changedIds) {
      if (this.known.has(id)) fetch.add(id);
    }
    for (const id of input.ids) {
      if (!this.known.has(id)) fetch.add(id);
    }
    for (const id of input.removedIds) fetch.delete(id);
    if (fetch.size === 0 && input.removedIds.length === 0) return { mode: 'none' };
    return { mode: 'delta', fetchIds: [...fetch] };
  }

  current(): Record<string, RangeTotals>[] | null {
    return this.results;
  }

  applyFull(
    rangesKey: string,
    ids: readonly string[],
    results: Record<string, RangeTotals>[],
  ): Record<string, RangeTotals>[] {
    this.rangesKey = rangesKey;
    this.known = new Set(ids);
    this.results = results;
    return results;
  }

  /**
   * Merge a delta fetch. A fetched id absent from a range's result map has no
   * usage in that window any more and is removed from the cached map.
   */
  applyDelta(
    fetchIds: readonly string[],
    removedIds: readonly string[],
    fetched: Record<string, RangeTotals>[] | null,
  ): Record<string, RangeTotals>[] {
    if (this.results === null) {
      throw new Error('applyDelta requires a populated cache');
    }
    const next = this.results.map((previous, rangeIndex) => {
      const merged: Record<string, RangeTotals> = { ...previous };
      const source = fetched?.[rangeIndex];
      for (const id of fetchIds) {
        const totals = source?.[id];
        if (totals !== undefined) merged[id] = totals;
        else delete merged[id];
      }
      for (const id of removedIds) delete merged[id];
      return merged;
    });
    for (const id of fetchIds) this.known.add(id);
    for (const id of removedIds) this.known.delete(id);
    this.results = next;
    return next;
  }

  /** Drop everything; the next plan will be a full fetch. */
  invalidate(): void {
    this.rangesKey = null;
    this.known.clear();
    this.results = null;
  }
}
