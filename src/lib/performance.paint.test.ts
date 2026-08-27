import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const { recordFrontendPerformance } = vi.hoisted(() => ({
  recordFrontendPerformance: vi.fn().mockResolvedValue(undefined),
}));

vi.mock('./ipc', () => ({ recordFrontendPerformance }));

import { configurePerformanceTracking, measureNextPaint } from './performance';

/**
 * `measureNextPaint` defers its emit behind two nested
 * `requestAnimationFrame` callbacks and then drains asynchronously. The stub
 * below runs rAF callbacks synchronously, so settling only has to flush the
 * drain's microtasks.
 */
const frameCallbacks: FrameRequestCallback[] = [];

async function settlePaint(): Promise<void> {
  // Two nested frames, then the drain's microtasks. Deferred rather than
  // run inline, so a test can change UI state between the call and the
  // paint — which is the whole behaviour under test.
  for (let frame = 0; frame < 2; frame += 1) {
    const queued = frameCallbacks.splice(0, frameCallbacks.length);
    for (const callback of queued) callback(performance.now());
  }
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
}

describe('measureNextPaint metadata (issue #184)', () => {
  beforeEach(() => {
    frameCallbacks.length = 0;
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
      frameCallbacks.push(callback);
      return frameCallbacks.length;
    });
    recordFrontendPerformance.mockClear();
    configurePerformanceTracking(true);
  });

  afterEach(() => {
    configurePerformanceTracking(false);
    vi.unstubAllGlobals();
  });

  it('evaluates a metadata thunk after the paint, not at call time', async () => {
    // The whole point for `rows_rendered`: the caller runs *before* the
    // paint it measures, so reading UI state eagerly would capture what the
    // paint started from rather than what it produced.
    let rows = 0;
    measureNextPaint('frontend.session_batch_paint', performance.now(), () => ({
      rows_rendered: rows,
    }));
    rows = 64; // the paint renders the rows

    await settlePaint();

    expect(recordFrontendPerformance).toHaveBeenCalledTimes(1);
    const [, , , metadata] = recordFrontendPerformance.mock.calls[0];
    expect(metadata.rows_rendered).toBe('64');
  });

  it('still accepts a plain metadata object', async () => {
    measureNextPaint('frontend.session_list_paint', performance.now(), { rows: 12 });

    await settlePaint();

    const [operation, , , metadata] = recordFrontendPerformance.mock.calls[0];
    expect(operation).toBe('frontend.session_list_paint');
    expect(metadata.rows).toBe('12');
  });

  it('records nothing when tracking is disabled', async () => {
    configurePerformanceTracking(false);
    const metadata = vi.fn(() => ({ rows_rendered: 1 }));

    measureNextPaint('frontend.session_batch_paint', performance.now(), metadata);
    await settlePaint();

    expect(recordFrontendPerformance).not.toHaveBeenCalled();
    // And the thunk is never run, so publishing state into it costs nothing
    // on the default path where performance tracking is off.
    expect(metadata).not.toHaveBeenCalled();
  });
});
