import { describe, expect, it } from 'vitest';
import { remainingPercent, resetCountdown, windowLabel } from './subscriptionUsage';

describe('windowLabel', () => {
  it('labels a 5-hour window, tolerating small drift', () => {
    expect(windowLabel(300)).toBe('5h');
    expect(windowLabel(301)).toBe('5h');
    expect(windowLabel(298)).toBe('5h');
  });

  it('labels a weekly window, tolerating small drift', () => {
    expect(windowLabel(10_080)).toBe('Weekly');
    expect(windowLabel(10_079)).toBe('Weekly');
    expect(windowLabel(10_100)).toBe('Weekly');
  });

  it('falls back to a raw hour figure for non-canonical windows', () => {
    expect(windowLabel(120)).toBe('2h');
    expect(windowLabel(90)).toBe('1.5h');
  });

  it('falls back to minutes under an hour', () => {
    expect(windowLabel(45)).toBe('45m');
    expect(windowLabel(1)).toBe('1m');
  });
});

describe('remainingPercent', () => {
  it('subtracts used percent from 100', () => {
    expect(remainingPercent(63)).toBe(37);
    expect(remainingPercent(0)).toBe(100);
  });

  it('clamps to the 0-100 range', () => {
    expect(remainingPercent(140)).toBe(0);
    expect(remainingPercent(-10)).toBe(100);
  });
});

describe('resetCountdown', () => {
  const now = Date.parse('2026-07-29T12:00:00.000Z');

  it('returns null when no reset time was reported', () => {
    expect(resetCountdown(null, now)).toBeNull();
  });

  it('returns null for an unparseable timestamp', () => {
    expect(resetCountdown('not-a-date', now)).toBeNull();
  });

  it('formats hours and minutes remaining', () => {
    expect(resetCountdown('2026-07-29T15:12:00.000Z', now)).toBe('3h 12m');
  });

  it('omits the hour part under an hour', () => {
    expect(resetCountdown('2026-07-29T12:09:00.000Z', now)).toBe('9m');
  });

  it('reports due now once the reset time has passed', () => {
    expect(resetCountdown('2026-07-29T11:59:00.000Z', now)).toBe('due now');
    expect(resetCountdown('2026-07-29T12:00:00.000Z', now)).toBe('due now');
  });
});
