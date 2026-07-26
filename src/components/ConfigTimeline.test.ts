import { render, screen } from '@testing-library/svelte';
import { tick } from 'svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sessionsStore } from '../lib/stores/sessions.svelte';
import { zeroToolMetrics, zeroTotals } from '../lib/sessionProjection';
import type { EventCorrelation, ExternalEvent, SessionSummary } from '../lib/types';
import ConfigTimeline from './ConfigTimeline.svelte';

const { correlateEvents } = vi.hoisted(() => ({ correlateEvents: vi.fn() }));

vi.mock('../lib/ipc', () => ({ correlateEvents }));

const event: ExternalEvent = {
  id: 'change',
  timestamp: '2026-01-01T00:00:00Z',
  scope: null,
  source: 'config',
  kind: 'modified',
  metadata: { harness: 'codex', path_id: 'fixture', safe_diff: 'size 10 -> 20 bytes' },
};

function observation(tokens: number) {
  return {
    session_count: 3,
    turn_count: 3,
    session_duration_ms: 180_000,
    tokens: { ...zeroTotals(), total_tokens: tokens },
    buckets_by_harness: {},
    tool_metrics: zeroToolMetrics(),
  };
}

function correlation(overrides: Partial<EventCorrelation> = {}): EventCorrelation {
  return {
    event,
    before: observation(300),
    after: observation(600),
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

async function flushRequest() {
  await tick();
  await vi.advanceTimersByTimeAsync(250);
  await tick();
}

describe('ConfigTimeline', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime('2026-01-01T00:00:00Z');
    correlateEvents.mockReset();
    sessionsStore.replaceAll([]);
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('does not expose secondary before/after metrics while a sample is incomplete', async () => {
    correlateEvents.mockResolvedValue({
      results: [correlation({ after_window_complete: false, after_window_end: '2026-01-02T00:00:00Z' })],
    });
    render(ConfigTimeline, { props: { events: [event] } });

    await flushRequest();

    expect(screen.getByText(/Outcome deltas hidden/)).toBeInTheDocument();
    expect(screen.queryByText(/tokens\/turn/)).not.toBeInTheDocument();
    expect(screen.queryByText(/avg session/)).not.toBeInTheDocument();
  });

  it('refreshes correlations when the session corpus changes', async () => {
    correlateEvents.mockResolvedValue({ results: [correlation()] });
    render(ConfigTimeline, { props: { events: [event] } });
    await flushRequest();
    expect(correlateEvents).toHaveBeenCalledTimes(1);

    sessionsStore.upsert({
      id: 'session',
      started_at: '2026-01-01T00:00:00Z',
      last_event_at: '2026-01-01T00:01:00Z',
    } as SessionSummary);
    await flushRequest();

    expect(correlateEvents).toHaveBeenCalledTimes(2);
  });

  it('refreshes when an incomplete after-window reaches its boundary', async () => {
    correlateEvents.mockResolvedValue({
      results: [correlation({ after_window_complete: false, after_window_end: '2026-01-01T00:00:01Z' })],
    });
    render(ConfigTimeline, { props: { events: [event] } });
    await flushRequest();
    expect(correlateEvents).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(1_100);
    await flushRequest();

    expect(correlateEvents).toHaveBeenCalledTimes(2);
  });
});
