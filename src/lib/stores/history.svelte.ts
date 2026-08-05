import type { HistoryStatus } from '../types';

/**
 * Durable-history open/migration progress, shared between App (which feeds
 * it from get_history_status / "history-progress") and the status bar
 * (#116). Starts 'pending' — matching the backend's initial state — so a
 * listener that mounts before the first status arrives shows the loading
 * indicator rather than silently defaulting to "ready".
 */
function createHistoryStore() {
  let status = $state<HistoryStatus>({
    status: 'pending',
    step: null,
    step_index: null,
    step_total: null,
    items_done: null,
    items_total: null,
    elapsed_ms: null,
  });

  return {
    get status() {
      return status;
    },
    set(next: HistoryStatus) {
      status = next;
    },
  };
}

export const historyStore = createHistoryStore();
