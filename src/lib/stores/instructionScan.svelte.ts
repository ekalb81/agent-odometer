import type { InstructionScanProgress } from '../types';

/** Instruction-inventory progress shared by the tab and persistent status bar. */
function createInstructionScanStore() {
  let status = $state<InstructionScanProgress | null>(null);
  let ignoredThroughScanId = 0;

  return {
    get status() {
      return status;
    },
    set(next: InstructionScanProgress) {
      if (next.scan_id <= ignoredThroughScanId) return;
      if (status && next.scan_id < status.scan_id) return;
      status = next;
    },
    clearCurrent() {
      if (status) ignoredThroughScanId = Math.max(ignoredThroughScanId, status.scan_id);
      status = null;
    },
    clearThrough(scanId: number) {
      ignoredThroughScanId = Math.max(ignoredThroughScanId, scanId);
      if (status && status.scan_id <= ignoredThroughScanId) status = null;
    },
  };
}

export const instructionScanStore = createInstructionScanStore();
