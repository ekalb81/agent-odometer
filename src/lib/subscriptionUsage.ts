// Pure helpers for rendering provider-reported subscription-usage windows
// (SubscriptionUsage.svelte). Kept dependency-free so they're easy to unit
// test against fixed clocks instead of real time.
//
// The quota-window helpers below (issue #43) format numbers the backend
// already computed (src-tauri/src/quota.rs owns pace/forecast/budget math);
// nothing here re-derives a pace, a projection, or a budget-crossing
// decision — only presentation.

import type { QuotaBudget, QuotaForecast, QuotaSnapshot, QuotaUnavailableReason, QuotaWindow } from './types';

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

/** Human label for a QuotaWindow (issue #43): reuses windowLabel for a
 *  known window_minutes, otherwise falls back to the window's own kind
 *  (e.g. a credit balance has no window_minutes at all). */
export function quotaWindowLabel(window: Pick<QuotaWindow, 'kind' | 'window_minutes'>): string {
  if (window.window_minutes != null) return windowLabel(window.window_minutes);
  switch (window.kind) {
    case 'credit_balance':
      return 'Credits';
    case 'daily':
      return 'Daily';
    case 'weekly':
      return 'Weekly';
    case 'monthly':
      return 'Monthly';
    case 'burst':
      return 'Burst';
    default:
      return 'Window';
  }
}

const UNAVAILABLE_LABELS: Record<QuotaUnavailableReason, string> = {
  no_quota_source: 'does not report quota in transcripts',
  no_observation: 'no snapshot captured yet',
  clock_skew: 'system clock skew detected — unable to compute',
  provider_outage: 'provider outage',
  auth_expired: 'authentication expired',
  rate_limited: 'quota check was itself rate-limited',
  offline: 'offline',
};

/** Short, honest text for an unavailable window/snapshot. Never implies a
 *  number exists — see quota.rs's honesty contract. */
export function quotaUnavailableLabel(reason: QuotaUnavailableReason): string {
  return UNAVAILABLE_LABELS[reason];
}

/** One-line pace/exhaustion summary for an already-computed QuotaForecast,
 *  or null when there is nothing to show (no forecast — e.g. below the
 *  minimum-evidence threshold — is a legitimate, silent state, not an
 *  error). */
export function forecastSummary(forecast: QuotaForecast | null, nowMs: number): string | null {
  if (!forecast) return null;
  const pace = `${forecast.pace_per_hour >= 10 ? forecast.pace_per_hour.toFixed(0) : forecast.pace_per_hour.toFixed(1)}%/hr`;
  if (!forecast.projected_exhaustion_at) return `pace ${pace}`;
  const countdown = resetCountdown(forecast.projected_exhaustion_at, nowMs);
  return countdown ? `pace ${pace} · exhausts in ${countdown}` : `pace ${pace}`;
}

/** `reserve_deficit_percent` is signed (positive = burning faster than an
 *  even pace to reset; negative = a cushion) — rendered as a short label
 *  rather than a bare signed number so it reads without a legend. */
export function reserveDeficitLabel(reserveDeficitPercent: number): string {
  const magnitude = Math.round(Math.abs(reserveDeficitPercent));
  if (magnitude === 0) return 'on pace';
  return reserveDeficitPercent > 0
    ? `${magnitude}pt behind even pace`
    : `${magnitude}pt ahead of even pace`;
}

export type BudgetStatus = 'ok' | 'warning' | 'exceeded';

/** Advisory-only (hard enforcement is issue #46) status for a budget given
 *  its already-computed current value. Warning starts at 80% of the
 *  threshold so it is visible before the crossing that would fire an
 *  alert. */
export function budgetStatus(currentValue: number, budget: Pick<QuotaBudget, 'threshold'>): BudgetStatus {
  if (currentValue >= budget.threshold) return 'exceeded';
  if (currentValue >= budget.threshold * 0.8) return 'warning';
  return 'ok';
}

/** Tray text for the first provider with an available percent window (a
 *  provider order match is used deliberately over "most urgent" so the
 *  tray label doesn't jump between providers as usage fluctuates). Returns
 *  null when nothing is available to show, so the tray keeps its existing
 *  placeholder rather than a fabricated figure. */
export function quotaTrayLabel(snapshots: QuotaSnapshot[]): string | null {
  for (const snapshot of snapshots) {
    const window = snapshot.windows.find((w) => w.unit === 'percent' && w.unavailable === null && w.used != null);
    if (!window || window.used == null) continue;
    const remaining = remainingPercent(window.used);
    const label = quotaWindowLabel(window);
    const staleSuffix = window.stale ? ' (stale)' : '';
    return `${snapshot.provider} ${label} ${remaining.toFixed(0)}% left${staleSuffix}`;
  }
  return null;
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
