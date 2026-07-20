import type { ScanStatus } from '../types';

/** Bulk-scan progress shared between App (which feeds it) and the views. */
function createScanStore() {
  let status = $state<ScanStatus>({ done: 0, total: 0, complete: false, elapsed_ms: null });

  return {
    get status() {
      return status;
    },
    set(next: ScanStatus) {
      // Never regress from complete back to in-progress except when a new
      // scan starts over (set_config rescan announces itself with done=0).
      if (status.complete && !next.complete && next.done !== 0) return;
      status = next;
    },
  };
}

export const scanStore = createScanStore();
