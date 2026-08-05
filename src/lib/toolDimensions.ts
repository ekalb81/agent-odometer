// Issue #44 open-set tool/context dimension aggregation. Backend-emitted
// per-session `RangeTotals.tool_dimensions` are summed here across whatever
// sessions are currently in view — never by reparsing a transcript or tool
// observation in the frontend, per AGENTS.md's "emit during parsing, never
// reparse in the UI" rule for this feature.

import type { RangeTotals, ToolDimensionKind, ToolDimensionMetrics } from './types';

export type DimensionTotals = Partial<Record<ToolDimensionKind, Record<string, ToolDimensionMetrics>>>;

function zeroMetrics(): ToolDimensionMetrics {
  return { calls: 0, failures: 0, output_bytes: 0, duration_ms: 0, tokens: 0 };
}

function addMetrics(target: ToolDimensionMetrics, value: ToolDimensionMetrics): void {
  target.calls += value.calls;
  target.failures += value.failures;
  target.output_bytes += value.output_bytes;
  target.duration_ms += value.duration_ms;
  target.tokens += value.tokens;
}

/** Sums `tool_dimensions` across every session's `RangeTotals` in view. A
 * dimension kind absent from every session stays absent in the result — the
 * caller must consult provider capability flags to distinguish "no data in
 * this window" from "this provider cannot supply the dimension". */
export function aggregateToolDimensions(
  totals: Record<string, RangeTotals> | null | undefined,
): DimensionTotals {
  const out: DimensionTotals = {};
  if (!totals) return out;
  for (const range of Object.values(totals)) {
    const dimensions = range.tool_dimensions;
    if (!dimensions) continue;
    for (const [kind, values] of Object.entries(dimensions) as [ToolDimensionKind, Record<string, ToolDimensionMetrics>][]) {
      const bucket = (out[kind] ??= {});
      for (const [value, metrics] of Object.entries(values)) {
        const entry = (bucket[value] ??= zeroMetrics());
        addMetrics(entry, metrics);
      }
    }
  }
  return out;
}

/** Top-N (value, metrics) pairs for one dimension kind, sorted by `calls`
 * descending (or `tokens` descending for `context_source`, which has no
 * `calls`). */
export function topDimensionValues(
  totals: DimensionTotals,
  kind: ToolDimensionKind,
  limit = 8,
): { value: string; metrics: ToolDimensionMetrics }[] {
  const bucket = totals[kind];
  if (!bucket) return [];
  const rank = (metrics: ToolDimensionMetrics) => (kind === 'context_source' ? metrics.tokens : metrics.calls);
  return Object.entries(bucket)
    .map(([value, metrics]) => ({ value, metrics }))
    .sort((a, b) => rank(b.metrics) - rank(a.metrics))
    .slice(0, limit);
}
