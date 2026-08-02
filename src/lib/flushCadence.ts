// Adaptive coalescing window for live session mutations.
//
// A single change should reach the UI quickly (150ms), but during sustained
// agent streaming the watcher parses arrive every ~300ms for hours; flushing
// each one repaints the list and refreshes every range consumer at ~1Hz.
// Widening the window under sustained load trades sub-second freshness —
// invisible on a usage meter — for a several-fold cut in paint and refresh
// cycles.

export const FLUSH_BASE_MS = 150;
export const FLUSH_ELEVATED_MS = 600;
export const FLUSH_SUSTAINED_MS = 1000;

const ELEVATED_WINDOW_MS = 3_000;
const ELEVATED_THRESHOLD = 4;
const SUSTAINED_WINDOW_MS = 8_000;
const SUSTAINED_THRESHOLD = 8;
const HISTORY_CAP = 32;

/** Record a completed flush, pruning entries too old to influence the delay. */
export function recordFlush(history: number[], now: number): void {
  history.push(now);
  const cutoff = now - SUSTAINED_WINDOW_MS;
  while (history.length > HISTORY_CAP || (history.length > 0 && history[0] < cutoff)) {
    history.shift();
  }
}

/** Pick the coalescing delay for the next flush from recent flush density. */
export function computeFlushDelay(history: readonly number[], now: number): number {
  let elevated = 0;
  let sustained = 0;
  for (const at of history) {
    if (at > now - SUSTAINED_WINDOW_MS) sustained += 1;
    if (at > now - ELEVATED_WINDOW_MS) elevated += 1;
  }
  if (sustained >= SUSTAINED_THRESHOLD) return FLUSH_SUSTAINED_MS;
  if (elevated >= ELEVATED_THRESHOLD) return FLUSH_ELEVATED_MS;
  return FLUSH_BASE_MS;
}
