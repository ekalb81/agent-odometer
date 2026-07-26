import type { EventCorrelation, ExternalEvent } from './types';

export const RAPID_REVERT_MS = 5 * 60_000;
const MAX_TIMER_DELAY_MS = 2_147_000_000;

function sizeChange(event: ExternalEvent): [number, number] | null {
  const match = event.metadata.safe_diff?.match(/size ([\d,]+) -> ([\d,]+) bytes/);
  return match ? [Number(match[1].replaceAll(',', '')), Number(match[2].replaceAll(',', ''))] : null;
}

function surfaceKey(event: ExternalEvent): string | null {
  const pathId = event.metadata.path_id;
  if (!pathId) return null;
  return `${event.metadata.harness ?? ''}\u0000${event.scope ?? ''}\u0000${pathId}`;
}

/** Label rapid size reversals per watched surface. Unrelated file changes may
 * interleave without hiding a legitimate same-file pattern. */
export function rapidRevertLabels(source: ExternalEvent[]): Map<string, string> {
  const labels = new Map<string, string>();
  const previousBySurface = new Map<string, ExternalEvent>();
  const ordered = [...source].sort((a, b) => Date.parse(a.timestamp) - Date.parse(b.timestamp));

  for (const event of ordered) {
    const key = surfaceKey(event);
    if (!key) continue;
    const before = previousBySurface.get(key);
    previousBySurface.set(key, event);
    if (!before) continue;

    const elapsed = Date.parse(event.timestamp) - Date.parse(before.timestamp);
    const beforeSize = sizeChange(before);
    const afterSize = sizeChange(event);
    if (elapsed < 0 || elapsed > RAPID_REVERT_MS || !beforeSize || !afterSize
      || beforeSize[0] !== afterSize[1] || beforeSize[1] !== afterSize[0]) {
      continue;
    }

    const duration = elapsed < 60_000
      ? `${Math.max(1, Math.round(elapsed / 1_000))}s`
      : `${Math.round(elapsed / 60_000)}m`;
    labels.set(before.id, `Rapid size-revert pattern · reverted ${duration} later`);
    labels.set(event.id, `Rapid size-revert pattern · restores the prior size after ${duration}`);
  }

  return labels;
}

export function comparisonReady(item: EventCorrelation): boolean {
  return item.after_window_complete && item.sample_ready;
}

export function readyUsageContext(item: EventCorrelation): string | null {
  if (!comparisonReady(item)) return null;
  const tokensPerTurn = (observation: EventCorrelation['before']) =>
    observation.turn_count > 0 ? observation.tokens.total_tokens / observation.turn_count : null;
  const minutesPerSession = (observation: EventCorrelation['before']) =>
    observation.session_count > 0 ? observation.session_duration_ms / observation.session_count / 60_000 : null;
  const beforeTokens = tokensPerTurn(item.before);
  const afterTokens = tokensPerTurn(item.after);
  const beforeMinutes = minutesPerSession(item.before);
  const afterMinutes = minutesPerSession(item.after);
  const missingSides = (beforeValue: number, afterValue: number, noun: string) => {
    if (beforeValue === 0 && afterValue === 0) return `no qualifying ${noun} on either side`;
    if (beforeValue === 0) return `no qualifying ${noun} before the change`;
    return `no qualifying ${noun} after the change`;
  };
  const tokenText = beforeTokens === null || afterTokens === null
    ? `tokens/turn unavailable (${missingSides(item.before.turn_count, item.after.turn_count, 'turns')})`
    : `tokens/turn ${Math.round(beforeTokens).toLocaleString()} → ${Math.round(afterTokens).toLocaleString()}`;
  const durationText = beforeMinutes === null || afterMinutes === null
    ? `session length unavailable (${missingSides(item.before.session_count, item.after.session_count, 'sessions')})`
    : `avg session ${beforeMinutes.toFixed(0)}m → ${afterMinutes.toFixed(0)}m`;
  return `${tokenText} · ${durationText}`;
}

/** Return the next backend readiness boundary so a long-running companion can
 * refresh a previously incomplete comparison without waiting for a new event. */
export function nextCorrelationBoundaryDelay(
  items: EventCorrelation[],
  now = Date.now(),
): number | null {
  let next = Number.POSITIVE_INFINITY;
  for (const item of items) {
    if (item.after_window_complete) continue;
    const end = Date.parse(item.after_window_end);
    if (Number.isFinite(end) && end > now) next = Math.min(next, end);
  }
  if (!Number.isFinite(next)) return null;
  return Math.min(MAX_TIMER_DELAY_MS, Math.max(0, next - now) + 100);
}
