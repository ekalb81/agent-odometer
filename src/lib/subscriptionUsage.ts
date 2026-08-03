// Pure helpers for rendering provider-reported subscription-usage windows
// (SubscriptionUsage.svelte). Kept dependency-free so they're easy to unit
// test against fixed clocks instead of real time.

const MINUTES_PER_HOUR = 60;
const WEEKLY_MINUTES = 10_080; // 7 days
const WEEKLY_TOLERANCE_MINUTES = 30;
const HOUR_TOLERANCE_MINUTES = 3;

/** Human label for a provider-reported rate-limit window, e.g. 300 -> '5h',
 *  10080 -> 'Weekly'. Snapshots occasionally land a few minutes off an exact
 *  bucket, so labeling tolerates small drift before falling back to a raw
 *  hour/minute figure. */
export function windowLabel(windowMinutes: number): string {
  if (Math.abs(windowMinutes - WEEKLY_MINUTES) <= WEEKLY_TOLERANCE_MINUTES) return 'Weekly';
  const hours = windowMinutes / MINUTES_PER_HOUR;
  const roundedHours = Math.round(hours);
  if (
    roundedHours >= 1 &&
    Math.abs(windowMinutes - roundedHours * MINUTES_PER_HOUR) <= HOUR_TOLERANCE_MINUTES
  ) {
    return `${roundedHours}h`;
  }
  if (windowMinutes >= MINUTES_PER_HOUR) return `${Math.round(hours * 10) / 10}h`;
  return `${Math.round(windowMinutes)}m`;
}

/** Percent of the window still available, clamped to [0, 100] — providers
 *  have been observed to report values slightly outside that range. */
export function remainingPercent(usedPercent: number): number {
  return Math.min(100, Math.max(0, 100 - usedPercent));
}

/** Short countdown to a window reset ('3h 12m', 'due now'), or null when no
 *  reset time was reported. */
export function resetCountdown(resetsAt: string | null, nowMs: number): string | null {
  if (!resetsAt) return null;
  const resetMs = Date.parse(resetsAt);
  if (Number.isNaN(resetMs)) return null;
  const diffMs = resetMs - nowMs;
  if (diffMs <= 0) return 'due now';
  const totalMinutes = Math.ceil(diffMs / 60_000);
  const hours = Math.floor(totalMinutes / MINUTES_PER_HOUR);
  const minutes = totalMinutes % MINUTES_PER_HOUR;
  return hours === 0 ? `${minutes}m` : `${hours}h ${minutes}m`;
}
