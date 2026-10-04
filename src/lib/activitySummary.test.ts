import { afterEach, describe, expect, it, vi } from 'vitest';
import { activitySummary, type ActivitySummaryOptions } from './activitySummary';
import { calendarDays, type ActivityDay } from './calendarActivity';

function options(overrides: Partial<ActivitySummaryOptions> = {}): ActivitySummaryOptions {
  const days = calendarDays('2026-07-28T12:00:00.123Z', '2026-07-29T23:59:59.999Z', 'utc').map((day, index) => ({ ...day,
    tokens: index ? 0 : 150, tool_calls: index ? 3 : 0, sessionIds: ['private-session-id'], metricSessionIds: { tokens: ['private-session-id'], tool_calls: [] },
  }));
  return { days, metric: 'tokens', zone: 'utc', harness: 'codex', selectedProject: false, coverage: 'complete', ...overrides };
}
afterEach(() => vi.unstubAllEnvs());

describe('local activity summary', () => {
  it('emits deterministic shared calendar counts, exact partial-edge bounds, and privacy-safe metadata', () => {
    const input = options();
    Object.assign(input.days[0], { prompt: 'PRIVATE_PROMPT', path: 'C:\\Users\\private', project: 'PRIVATE_PROJECT', account: 'PRIVATE_ACCOUNT' });
    const result = activitySummary(input);
    expect(activitySummary(input)).toEqual(result);
    expect(result.markdown).toContain('150 recorded tokens');
    expect(result.markdown).toContain('| 2026-07-28 | 150 |');
    expect(result.markdown).toContain('| 2026-07-29 | 0 |');
    expect(result.markdown).toContain('2026-07-28T12:00:00.123Z — 2026-07-29T23:59:59.999Z');
    expect(result.svg).toContain('Calendar timezone: UTC');
    for (const secret of ['private-session-id', 'PRIVATE_PROMPT', 'C:\\Users', 'PRIVATE_PROJECT', 'PRIVATE_ACCOUNT']) {
      expect(result.svg + result.markdown).not.toContain(secret);
    }
    const document = new DOMParser().parseFromString(result.svg, 'image/svg+xml');
    expect(document.querySelector('parsererror')).toBeNull();
    for (const element of document.querySelectorAll('*')) {
      expect(['svg', 'title', 'desc', 'rect', 'g', 'text']).toContain(element.localName);
      expect([...element.attributes].some((attribute) => attribute.name.startsWith('on') || ['href', 'src', 'style'].includes(attribute.name))).toBe(false);
    }
  });
  it('keeps tool-only activity and partial zeros distinct from missing history', () => {
    const result = activitySummary(options({ metric: 'tool_calls', coverage: 'partial', selectedProject: true }));
    expect(result.markdown).toContain('3 recorded tool calls');
    expect(result.markdown).toContain('selected project (name omitted)');
    expect(result.svg).toContain('Partial recorded history; missing activity is unknown.');
    const empty = options({ coverage: 'partial' });
    empty.days.forEach((day) => { day.tokens = 0; });
    expect(activitySummary(empty).markdown).toContain('0 recorded tokens');
    expect(activitySummary(empty).markdown).toContain('missing activity is unknown');
    expect(activitySummary(empty).markdown).not.toContain('history intact');
  });
  it('preserves a 23-hour local calendar day and its recorded bounds', () => {
    vi.stubEnv('TZ', 'America/New_York');
    const days = calendarDays('2026-03-08T05:00:00Z', '2026-03-09T03:59:59.999Z', 'local').map((day) => ({ ...day, tokens: 7, tool_calls: 0, sessionIds: [], metricSessionIds: { tokens: [], tool_calls: [] } }));
    const result = activitySummary(options({ days, zone: 'local' }));
    expect(result.markdown).toContain('Calendar timezone: America/New_York');
    expect(result.markdown).toContain('2026-03-08T05:00:00.000Z — 2026-03-09T03:59:59.999Z');
    expect(result.markdown).toContain('| 2026-03-08 | 7 |');
  });
  it('does not expose opaque provider names and rejects absent or unsafe measurements', () => {
    expect(activitySummary(options({ harness: '/private/provider' })).svg).not.toContain('/private/provider');
    expect(activitySummary(options({ harness: null })).svg).toContain('All providers');
    expect(activitySummary(options({ harness: 'all' })).svg).toContain('All providers');
    expect(() => activitySummary(options({ days: [] }))).toThrow('1–366');
    expect(() => activitySummary(options({ days: Array(367).fill(options().days[0]) as ActivityDay[] }))).toThrow('1–366');
    const invalid = options(); invalid.days[0].tokens = NaN;
    expect(() => activitySummary(invalid)).toThrow('unavailable');
    invalid.days.forEach((day) => { day.tokens = Number.MAX_SAFE_INTEGER; });
    expect(() => activitySummary(invalid)).toThrow('supported range');
  });
});
