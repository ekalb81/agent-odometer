import { recordFrontendPerformance } from './ipc';

let enabled = false;
let generation = 0;

interface PendingMeasurement {
  generation: number;
  operation: string;
  durationMs: number;
  success: boolean;
  metadata: Record<string, string>;
}

const MAX_PENDING_MEASUREMENTS = 128;
const pending: PendingMeasurement[] = [];
let draining = false;

export function configurePerformanceTracking(value: boolean): void {
  if (enabled === value) return;
  enabled = value;
  generation += 1;
  if (!enabled) pending.length = 0;
}

export function performanceTrackingEnabled(): boolean {
  return enabled;
}

export async function measureAsync<T>(
  operation: string,
  task: () => Promise<T>,
  metadata: Record<string, string | number | boolean> = {},
): Promise<T> {
  if (!enabled) return task();
  const measurementGeneration = generation;
  const started = performance.now();
  try {
    const result = await task();
    emit(operation, performance.now() - started, true, metadata, measurementGeneration);
    return result;
  } catch (error) {
    emit(operation, performance.now() - started, false, metadata, measurementGeneration);
    throw error;
  }
}

export function measureSync<T>(
  operation: string,
  task: () => T,
  metadata: Record<string, string | number | boolean> = {},
): T {
  if (!enabled) return task();
  const measurementGeneration = generation;
  const started = performance.now();
  try {
    const result = task();
    emit(operation, performance.now() - started, true, metadata, measurementGeneration);
    return result;
  } catch (error) {
    emit(operation, performance.now() - started, false, metadata, measurementGeneration);
    throw error;
  }
}

export function measureNextPaint(
  operation: string,
  started: number,
  metadata: Record<string, string | number | boolean> = {},
): void {
  if (!enabled) return;
  const measurementGeneration = generation;
  requestAnimationFrame(() => {
    requestAnimationFrame(() => emit(
      operation,
      performance.now() - started,
      true,
      metadata,
      measurementGeneration,
    ));
  });
}

function emit(
  operation: string,
  durationMs: number,
  success: boolean,
  metadata: Record<string, string | number | boolean>,
  measurementGeneration: number,
): void {
  if (!enabled || measurementGeneration !== generation) return;
  const normalized = Object.fromEntries(
    Object.entries(metadata).map(([key, value]) => [key, String(value)]),
  );
  if (pending.length >= MAX_PENDING_MEASUREMENTS) return;
  pending.push({
    generation: measurementGeneration,
    operation,
    durationMs,
    success,
    metadata: normalized,
  });
  void drain();
}

async function drain(): Promise<void> {
  if (draining) return;
  draining = true;
  try {
    while (enabled && pending.length > 0) {
      const item = pending.shift()!;
      if (item.generation !== generation) continue;
      try {
        await recordFrontendPerformance(
          item.operation,
          item.durationMs,
          item.success,
          item.metadata,
        );
      } catch {
        // Performance reporting must never affect application behavior.
      }
    }
  } finally {
    draining = false;
    if (enabled && pending.length > 0) void drain();
  }
}
