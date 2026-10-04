import type { HistoryStatus, RangeTotals } from './types';

export type CalendarZone = 'local' | 'utc';
export type ActivityMetric = 'tokens' | 'tool_calls';
export interface CalendarDay {
  date: string;
  month: string;
  day: number;
  weekday: number;
  from: string;
  to: string;
}
export interface ActivityDay extends CalendarDay {
  tokens: number;
  tool_calls: number;
  sessionIds: string[];
  metricSessionIds: Record<ActivityMetric, string[]>;
}

/** Local calendar arithmetic deliberately uses setDate, not 24-hour steps:
 * local days can contain 23 or 25 hours across daylight-saving transitions. */
export function calendarDays(from: string | null, to: string | null, zone: CalendarZone, now = Date.now()): CalendarDay[] {
  const end = new Date(to ?? now);
  const start = from ? new Date(from) : new Date(end);
  const utc = zone === 'utc';
  const midnight = (value: Date) => utc ? value.setUTCHours(0, 0, 0, 0) : value.setHours(0, 0, 0, 0);
  const advance = (value: Date, days: number) => utc ? value.setUTCDate(value.getUTCDate() + days) : value.setDate(value.getDate() + days);
  if (!from) { midnight(start); advance(start, -89); }
  if (!Number.isFinite(start.getTime()) || !Number.isFinite(end.getTime()) || start > end) {
    throw new Error('Choose a valid date range with the start before the end.');
  }
  const cursor = new Date(start); midnight(cursor);
  const days: CalendarDay[] = [];
  while (cursor <= end) {
    if (days.length === 366) throw new Error('Choose a date range of at most 366 calendar days.');
    const next = new Date(cursor); advance(next, 1);
    const year = utc ? cursor.getUTCFullYear() : cursor.getFullYear();
    const month = String((utc ? cursor.getUTCMonth() : cursor.getMonth()) + 1).padStart(2, '0');
    const day = utc ? cursor.getUTCDate() : cursor.getDate();
    days.push({ date: `${year}-${month}-${String(day).padStart(2, '0')}`, month: `${year}-${month}`, day,
      weekday: utc ? cursor.getUTCDay() : cursor.getDay(),
      from: new Date(Math.max(cursor.getTime(), start.getTime())).toISOString(),
      to: new Date(Math.min(next.getTime() - 1, end.getTime())).toISOString() });
    cursor.setTime(next.getTime());
  }
  return days;
}

/** Sum Rust query facts only; absent keys in a successful range are recorded
 * zero. Coverage is separately required before calling this an activity zero. */
export function activityDays(days: CalendarDay[], ranges: Record<string, RangeTotals>[]): ActivityDay[] {
  if (days.length !== ranges.length) throw new Error('Activity response is incomplete. Retry the range.');
  return days.map((day, index) => {
    const records = Object.entries(ranges[index]);
    return { ...day,
      tokens: records.reduce((sum, [, range]) => sum + range.tokens.total_tokens, 0),
      tool_calls: records.reduce((sum, [, range]) => sum + range.tool_metrics.calls, 0),
      sessionIds: records.filter(([, range]) => range.tokens.total_tokens > 0 || range.tool_metrics.calls > 0).map(([id]) => id),
      metricSessionIds: {
        tokens: records.filter(([, range]) => range.tokens.total_tokens > 0).map(([id]) => id),
        tool_calls: records.filter(([, range]) => range.tool_metrics.calls > 0).map(([id]) => id),
      } };
  });
}

// #38's additive coverage fields also permit old IPC payloads. Missing
// provenance is unknown coverage, never a reason to fabricate complete zero.
export function calendarEvidence(history: Pick<HistoryStatus, 'status'> & {
  coverage_complete?: boolean | null;
  failure?: { message: string } | null;
}, scanComplete: boolean): { state: 'pending' | 'unavailable' | 'partial' | 'complete'; message: string } {
  if (!scanComplete || history.status === 'pending') return { state: 'pending', message: 'Activity is preparing; the session scan and history archive must finish first.' };
  if (history.status !== 'ready' || history.coverage_complete == null) return { state: 'unavailable', message: history.failure?.message ?? 'History coverage is unavailable. Activity cannot be reported as zero.' };
  if (!history.coverage_complete) return { state: 'partial', message: 'Partial history: these are recorded totals only. Missing events are unknown; recorded zero does not prove no activity.' };
  return { state: 'complete', message: 'Recorded history is intact. These activity totals describe observed events, not productivity.' };
}

/** Preserve inclusive millisecond bounds when drilling into datetime-local
 * filters; rounding an end to 23:59 would silently omit its last minute. */
export function calendarFilterValue(iso: string): string {
  const date = new Date(iso);
  const pad = (value: number, width = 2) => String(value).padStart(width, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}.${pad(date.getMilliseconds(), 3)}`;
}
