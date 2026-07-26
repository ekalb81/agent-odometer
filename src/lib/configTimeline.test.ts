import { describe, expect, it } from 'vitest';
import {
  nextCorrelationBoundaryDelay,
  rapidRevertLabels,
  readyUsageContext,
} from './configTimeline';
import { zeroToolMetrics, zeroTotals } from './sessionProjection';
import type { CorrelationObservation, EventCorrelation, ExternalEvent } from './types';

function event(
  id: string,
  timestamp: string,
  pathId: string | null,
  safeDiff: string,
): ExternalEvent {
  return {
    id,
    timestamp,
    scope: 'project:fixture',
    source: 'config',
    kind: 'modified',
    metadata: {
      harness: 'codex',
      safe_diff: safeDiff,
      ...(pathId ? { path_id: pathId } : {}),
    },
  };
}

function observation(sessionCount: number, turnCount: number, tokens: number): CorrelationObservation {
  return {
    session_count: sessionCount,
    turn_count: turnCount,
    session_duration_ms: sessionCount * 60_000,
    tokens: { ...zeroTotals(), total_tokens: tokens },
    buckets_by_harness: {},
    tool_metrics: zeroToolMetrics(),
  };
}

function correlation(overrides: Partial<EventCorrelation> = {}): EventCorrelation {
  return {
    event: event('change', '2026-01-01T00:00:00Z', 'surface', 'size 10 -> 20 bytes'),
    before: observation(3, 3, 300),
    after: observation(3, 3, 600),
    after_window_end: '2026-01-08T00:00:00Z',
    after_window_complete: true,
    minimum_session_count: 3,
    sample_ready: true,
    token_delta: 300,
    session_delta: 0,
    confounding_event_ids: [],
    warnings: [],
    ...overrides,
  };
}

describe('configuration timeline helpers', () => {
  it('detects same-surface reversals across interleaved unrelated changes', () => {
    const first = event('a-before', '2026-01-01T00:00:00Z', 'a', 'size 100 -> 200 bytes');
    const unrelated = event('b-change', '2026-01-01T00:00:30Z', 'b', 'size 10 -> 20 bytes');
    const reverted = event('a-after', '2026-01-01T00:01:00Z', 'a', 'size 200 -> 100 bytes');

    const labels = rapidRevertLabels([reverted, unrelated, first]);

    expect(labels.get('a-before')).toContain('reverted 1m later');
    expect(labels.get('a-after')).toContain('restores the prior size after 1m');
    expect(labels.has('b-change')).toBe(false);
  });

  it('ignores events without a surface and non-reversing or late changes', () => {
    const withoutSurface = event('missing', '2026-01-01T00:00:00Z', null, 'size 1 -> 2 bytes');
    const first = event('first', '2026-01-01T00:00:00Z', 'a', 'size 100 -> 200 bytes');
    const mismatch = event('mismatch', '2026-01-01T00:00:30Z', 'a', 'size 200 -> 150 bytes');
    const late = event('late', '2026-01-01T00:10:00Z', 'a', 'size 150 -> 200 bytes');

    expect(rapidRevertLabels([withoutSurface, first, mismatch, late])).toEqual(new Map());
  });

  it('hides every comparative usage metric until the sample is complete and ready', () => {
    expect(readyUsageContext(correlation({ after_window_complete: false }))).toBeNull();
    expect(readyUsageContext(correlation({ sample_ready: false }))).toBeNull();
    expect(readyUsageContext(correlation())).toBe('tokens/turn 100 → 200 · avg session 1m → 1m');
  });

  it('explains missing ready-sample denominators without inventing averages', () => {
    const item = correlation({
      before: observation(3, 0, 0),
      after: observation(3, 0, 0),
    });
    expect(readyUsageContext(item)).toContain('no qualifying turns on either side');
  });

  it('schedules the earliest incomplete future boundary and ignores completed samples', () => {
    const now = Date.parse('2026-01-01T00:00:00Z');
    const later = correlation({ after_window_end: '2026-01-01T00:00:02Z', after_window_complete: false });
    const sooner = correlation({ after_window_end: '2026-01-01T00:00:01Z', after_window_complete: false });
    const complete = correlation({ after_window_end: '2026-01-01T00:00:00.500Z', after_window_complete: true });

    expect(nextCorrelationBoundaryDelay([later, complete, sooner], now)).toBe(1_100);
    expect(nextCorrelationBoundaryDelay([complete], now)).toBeNull();
  });
});
