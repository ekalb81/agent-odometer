import { describe, expect, it } from 'vitest';
import {
  budgetStatus,
  forecastSummary,
  quotaTrayLabel,
  quotaUnavailableLabel,
  quotaWindowLabel,
  remainingPercent,
  reserveDeficitLabel,
  resetCountdown,
  windowLabel,
} from './subscriptionUsage';
import type { QuotaForecast, QuotaSnapshot, QuotaWindow } from './types';

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

function quotaWindow(overrides: Partial<QuotaWindow> = {}): QuotaWindow {
  return {
    kind: 'burst',
    unit: 'percent',
    window_minutes: 300,
    used: 42,
    remaining: 58,
    limit: 100,
    unlimited: false,
    resets_at: null,
    window_started_at: null,
    window_started_at_estimated: false,
    observed_at: '2026-07-29T11:00:00.000Z',
    confidence: 'high',
    stale: false,
    unavailable: null,
    forecast: null,
    ...overrides,
  };
}

describe('quotaWindowLabel', () => {
  it('reuses windowLabel when window_minutes is known', () => {
    expect(quotaWindowLabel(quotaWindow({ window_minutes: 300 }))).toBe('5h');
  });

  it('falls back to the window kind when window_minutes is absent', () => {
    expect(quotaWindowLabel(quotaWindow({ kind: 'credit_balance', window_minutes: null }))).toBe('Credits');
    expect(quotaWindowLabel(quotaWindow({ kind: 'other', window_minutes: null }))).toBe('Window');
  });
});

describe('quotaUnavailableLabel', () => {
  it('never claims a number exists for any unavailable reason', () => {
    for (const reason of [
      'no_quota_source',
      'no_observation',
      'clock_skew',
      'provider_outage',
      'auth_expired',
      'rate_limited',
      'offline',
    ] as const) {
      const label = quotaUnavailableLabel(reason);
      expect(label.length).toBeGreaterThan(0);
      expect(label).not.toMatch(/^0/);
    }
  });
});

describe('forecastSummary', () => {
  const now = Date.parse('2026-07-29T12:00:00.000Z');

  it('returns null when there is no forecast (below minimum evidence)', () => {
    expect(forecastSummary(null, now)).toBeNull();
  });

  it('formats pace alone when there is no projected exhaustion', () => {
    const forecast: QuotaForecast = {
      pace_per_hour: 3.4,
      projected_exhaustion_at: null,
      reserve_deficit_percent: 0,
      evidence_points: 6,
    };
    expect(forecastSummary(forecast, now)).toBe('pace 3.4%/hr');
  });

  it('includes the exhaustion countdown when projected', () => {
    const forecast: QuotaForecast = {
      pace_per_hour: 12,
      projected_exhaustion_at: '2026-07-29T15:12:00.000Z',
      reserve_deficit_percent: 10,
      evidence_points: 6,
    };
    expect(forecastSummary(forecast, now)).toBe('pace 12%/hr · exhausts in 3h 12m');
  });
});

describe('reserveDeficitLabel', () => {
  it('reports on pace for a value near zero', () => {
    expect(reserveDeficitLabel(0)).toBe('on pace');
  });

  it('reports behind for a positive deficit (burning faster than even pace)', () => {
    expect(reserveDeficitLabel(12.4)).toBe('12pt behind even pace');
  });

  it('reports ahead for a negative deficit (a reserve/cushion)', () => {
    expect(reserveDeficitLabel(-8.2)).toBe('8pt ahead of even pace');
  });
});

describe('budgetStatus', () => {
  const budget = { threshold: 80 };

  it('is ok below the warning threshold', () => {
    expect(budgetStatus(50, budget)).toBe('ok');
  });

  it('is warning at or above 80% of the threshold', () => {
    expect(budgetStatus(64, budget)).toBe('warning');
  });

  it('is exceeded at or above the threshold itself', () => {
    expect(budgetStatus(80, budget)).toBe('exceeded');
    expect(budgetStatus(95, budget)).toBe('exceeded');
  });
});

describe('quotaTrayLabel', () => {
  function snapshot(overrides: Partial<QuotaSnapshot> = {}): QuotaSnapshot {
    return {
      provider: 'codex',
      provenance: 'transcript_derived',
      windows: [quotaWindow()],
      unavailable: null,
      ...overrides,
    };
  }

  it('returns null when nothing is available anywhere', () => {
    expect(
      quotaTrayLabel([
        snapshot({ provider: 'claude_code', windows: [], unavailable: 'no_quota_source' }),
      ]),
    ).toBeNull();
  });

  it('formats the first provider with an available percent window', () => {
    const label = quotaTrayLabel([snapshot()]);
    expect(label).toBe('codex 5h 58% left');
  });

  it('flags a stale reading rather than presenting it as fresh', () => {
    const label = quotaTrayLabel([snapshot({ windows: [quotaWindow({ stale: true })] })]);
    expect(label).toBe('codex 5h 58% left (stale)');
  });

  it('skips a credit-only window (not a percent unit) and an unavailable one', () => {
    const label = quotaTrayLabel([
      snapshot({
        provider: 'claude_code',
        windows: [quotaWindow({ unit: 'credits', kind: 'credit_balance', used: null, remaining: 10 })],
      }),
      snapshot({ provider: 'codex', windows: [quotaWindow({ unavailable: 'clock_skew', used: null })] }),
      snapshot({ provider: 'gemini_cli', windows: [quotaWindow({ used: 10, remaining: 90 })] }),
    ]);
    expect(label).toBe('gemini_cli 5h 90% left');
  });
});
