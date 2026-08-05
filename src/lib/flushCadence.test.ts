import { describe, expect, it } from 'vitest';
import {
  FLUSH_BASE_MS,
  FLUSH_ELEVATED_MS,
  FLUSH_SUSTAINED_MS,
  computeFlushDelay,
  recordFlush,
} from './flushCadence';

describe('computeFlushDelay', () => {
  it('uses the base delay for isolated changes', () => {
    expect(computeFlushDelay([], 10_000)).toBe(FLUSH_BASE_MS);
    expect(computeFlushDelay([1_000], 10_000)).toBe(FLUSH_BASE_MS);
  });

  it('elevates under a burst and sustains under continuous streaming', () => {
    const now = 100_000;
    const burst = [now - 1_200, now - 900, now - 600, now - 300];
    expect(computeFlushDelay(burst, now)).toBe(FLUSH_ELEVATED_MS);
    const streaming = Array.from({ length: 10 }, (_, i) => now - 7_500 + i * 700);
    expect(computeFlushDelay(streaming, now)).toBe(FLUSH_SUSTAINED_MS);
  });

  it('stays sustained at the sustained cadence (no oscillation back to base)', () => {
    const now = 200_000;
    // Flushing once per second fills the 8s window with exactly the threshold.
    const steady = Array.from({ length: 8 }, (_, i) => now - 8_000 + 1 + i * 1_000);
    expect(computeFlushDelay(steady, now)).toBe(FLUSH_SUSTAINED_MS);
  });

  it('decays back to the base delay after a quiet period', () => {
    const now = 300_000;
    const old = Array.from({ length: 10 }, (_, i) => now - 20_000 + i * 300);
    expect(computeFlushDelay(old, now)).toBe(FLUSH_BASE_MS);
  });
});

describe('recordFlush', () => {
  it('prunes entries outside the sustained window and caps history length', () => {
    const history: number[] = [];
    for (let i = 0; i < 100; i++) recordFlush(history, i * 100);
    expect(history.length).toBeLessThanOrEqual(32);
    expect(history[0]).toBeGreaterThanOrEqual(9_900 - 8_000);
    recordFlush(history, 60_000);
    expect(history).toEqual([60_000]);
  });
});
