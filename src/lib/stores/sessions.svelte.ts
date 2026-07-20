import type { SessionSummary } from '../types';

export interface TrackedSession extends SessionSummary {
  /** Epoch ms of the last upsert — used to drive the pulse animation. */
  lastUpdatedAt: number;
  /** Precomputed sort keys: Date parsing inside comparators is expensive
   *  at thousands of comparisons per re-sort. */
  startedMs: number;
  lastEventMs: number;
}

function track(s: SessionSummary, lastUpdatedAt: number): TrackedSession {
  return {
    ...s,
    lastUpdatedAt,
    startedMs: new Date(s.started_at).getTime(),
    lastEventMs: new Date(s.last_event_at).getTime(),
  };
}

function createSessionsStore() {
  let map = $state<Map<string, TrackedSession>>(new Map());

  /** Replace the entire collection (used at startup after listSessions). */
  function replaceAll(list: SessionSummary[]): void {
    const next = new Map<string, TrackedSession>();
    for (const s of list) {
      next.set(s.id, track(s, 0));
    }
    map = next;
  }

  /** Insert or update a single session (called on session-updated events). */
  function upsert(s: SessionSummary): void {
    upsertMany([s]);
  }

  /** Batched insert/update — one map clone per batch instead of per event,
   *  which matters during the initial scan's event flood. */
  function upsertMany(list: SessionSummary[]): void {
    if (list.length === 0) return;
    const now = Date.now();
    const next = new Map(map);
    for (const s of list) {
      next.set(s.id, track(s, now));
    }
    map = next;
  }

  /** Remove a session by id (called on session-removed events). */
  function remove(id: string): void {
    const next = new Map(map);
    next.delete(id);
    map = next;
  }

  /** All sessions sorted by last_event_at descending. */
  const sorted = $derived(
    [...map.values()].sort((a, b) => b.lastEventMs - a.lastEventMs),
  );

  return {
    get map() {
      return map;
    },
    get sorted() {
      return sorted;
    },
    replaceAll,
    upsert,
    upsertMany,
    remove,
  };
}

export const sessionsStore = createSessionsStore();
