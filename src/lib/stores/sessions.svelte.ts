import type { Session } from '../types';

interface TrackedSession extends Session {
  /** Epoch ms of the last upsert — used to drive the pulse animation. */
  lastUpdatedAt: number;
}

function createSessionsStore() {
  let map = $state<Map<string, TrackedSession>>(new Map());

  /** Replace the entire collection (used at startup after listSessions). */
  function replaceAll(list: Session[]): void {
    const next = new Map<string, TrackedSession>();
    for (const s of list) {
      next.set(s.id, { ...s, lastUpdatedAt: 0 });
    }
    map = next;
  }

  /** Insert or update a single session (called on session-updated events). */
  function upsert(s: Session): void {
    const next = new Map(map);
    next.set(s.id, { ...s, lastUpdatedAt: Date.now() });
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
    [...map.values()].sort(
      (a, b) =>
        new Date(b.last_event_at).getTime() - new Date(a.last_event_at).getTime(),
    ),
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
    remove,
  };
}

export const sessionsStore = createSessionsStore();
