import { afterEach, describe, expect, it, vi } from 'vitest';
import { activityDays, calendarDays, calendarEvidence, calendarFilterValue } from './calendarActivity';
import type { RangeTotals } from './types';

function totals(tokens: number, calls = 0): RangeTotals {
  return { tokens: { input_tokens: tokens, cached_input_tokens: 0, cache_creation_input_tokens: 0, output_tokens: 0, reasoning_output_tokens: 0, total_tokens: tokens },
    buckets: [], tool_metrics: { calls, reads: 0, searches: 0, mutations: 0, commands: 0, other: 0, successes: 0, failures: 0, unknown: 0, mutation_targets: 0, one_shot_mutations: 0, retry_count: 0, duration_ms: 0, output_bytes: 0 },
    tool_metrics_by_model: {}, optimization_findings_count: 0 };
}
afterEach(() => vi.unstubAllEnvs());
describe('calendar range facts', () => {
  it('partitions inclusive UTC bounds without duplicating midnight or dropping a partial edge day', () => {
    const days = calendarDays('2026-01-31T12:30:00.123Z', '2026-02-02T00:00:00.000Z', 'utc');
    expect(days.map((day) => day.date)).toEqual(['2026-01-31', '2026-02-01', '2026-02-02']);
    expect(days[0].from).toBe('2026-01-31T12:30:00.123Z');
    expect(days[2].from).toBe(days[2].to);
    for (let index = 1; index < days.length; index++) expect(Date.parse(days[index].from) - Date.parse(days[index - 1].to)).toBe(1);
    for (const day of days) expect(new Date(calendarFilterValue(day.to)).toISOString()).toBe(day.to);
  });
  it.each([
    ['2026-03-08T05:00:00Z', '2026-03-09T03:59:59.999Z', 23],
    ['2026-11-01T04:00:00Z', '2026-11-02T04:59:59.999Z', 25],
  ])('uses the %s local calendar day across DST', (from, to, hours) => {
    vi.stubEnv('TZ', 'America/New_York');
    const days = calendarDays(from, to, 'local');
    expect(days).toHaveLength(1);
    expect(Date.parse(days[0].to) - Date.parse(days[0].from) + 1).toBe(hours * 3_600_000);
  });
  it('reconciles sparse daily facts with the whole-range result and keeps tool-only sessions drillable', () => {
    const days = calendarDays('2026-01-01T00:00:00Z', '2026-01-03T23:59:59.999Z', 'utc');
    const ranges: Record<string, RangeTotals>[] = [{ a: totals(100), tools: totals(0, 3) }, {}, { a: totals(40), b: totals(60, 2) }];
    const series = activityDays(days, ranges);
    const wholeRange = { a: totals(140), b: totals(60, 2), tools: totals(0, 3) };
    expect(series.reduce((sum, day) => sum + day.tokens, 0)).toBe(Object.values(wholeRange).reduce((sum, value) => sum + value.tokens.total_tokens, 0));
    expect(series.reduce((sum, day) => sum + day.tool_calls, 0)).toBe(5);
    expect(series[1].tokens).toBe(0);
    expect(series[1].sessionIds).toEqual([]);
    expect(series[0].sessionIds).toEqual(['a', 'tools']);
    expect(series[0].metricSessionIds).toEqual({ tokens: ['a'], tool_calls: ['tools'] });
    expect(() => activityDays(days, [{}])).toThrow('incomplete');
  });
  it('bounds default and explicit layouts and rejects reversed or invalid dates', () => {
    expect(calendarDays(null, null, 'utc', Date.parse('2026-07-29T15:30:00Z'))).toHaveLength(90);
    expect(() => calendarDays('2025-01-01T00:00:00Z', '2026-01-02T00:00:00Z', 'utc')).toThrow('366');
    expect(() => calendarDays('invalid', null, 'utc')).toThrow('valid date range');
    expect(() => calendarDays('2026-02-02T00:00:00Z', '2026-02-01T00:00:00Z', 'utc')).toThrow('start before');
  });
  it('requires explicit coverage before an empty result can mean zero activity', () => {
    expect(calendarEvidence({ status: 'ready', coverage_complete: true }, true).state).toBe('complete');
    expect(calendarEvidence({ status: 'ready', coverage_complete: false }, true).state).toBe('partial');
    expect(calendarEvidence({ status: 'ready' }, true).state).toBe('unavailable');
    expect(calendarEvidence({ status: 'unavailable', failure: { message: 'Archive unreadable' } }, true).message).toBe('Archive unreadable');
    expect(calendarEvidence({ status: 'ready', coverage_complete: true }, false).state).toBe('pending');
  });
});
