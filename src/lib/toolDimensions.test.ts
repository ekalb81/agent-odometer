import { describe, expect, it } from 'vitest';
import { aggregateToolDimensions, topDimensionValues, type DimensionTotals } from './toolDimensions';
import { dimensionExportRows, rowsToCsv } from './sessionProjection';
import type { RangeTotals, ToolDimensionMetrics, TokenTotals } from './types';

const zeroTokens: TokenTotals = {
  input_tokens: 0, cached_input_tokens: 0, cache_creation_input_tokens: 0, output_tokens: 0,
  reasoning_output_tokens: 0, total_tokens: 0,
};

const zeroToolMetrics = {
  calls: 0, reads: 0, searches: 0, mutations: 0, commands: 0, other: 0,
  successes: 0, failures: 0, unknown: 0, mutation_targets: 0,
  one_shot_mutations: 0, retry_count: 0, duration_ms: 0, output_bytes: 0,
};

function dim(calls: number, failures = 0, outputBytes = 0, durationMs = 0, tokens = 0): ToolDimensionMetrics {
  return { calls, failures, output_bytes: outputBytes, duration_ms: durationMs, tokens };
}

function rangeWithDimensions(tool_dimensions: RangeTotals['tool_dimensions']): RangeTotals {
  return {
    tokens: zeroTokens,
    buckets: [],
    tool_metrics: zeroToolMetrics,
    tool_metrics_by_model: {},
    optimization_findings_count: 0,
    tool_dimensions,
  };
}

describe('aggregateToolDimensions', () => {
  it('sums matching (kind, value) entries across every session in the map', () => {
    const totals = aggregateToolDimensions({
      s1: rangeWithDimensions({
        mcp_server: { alpha: dim(2, 0, 100, 50) },
        language: { rust: dim(1) },
      }),
      s2: rangeWithDimensions({
        mcp_server: { alpha: dim(3, 1, 200, 75), beta: dim(1) },
      }),
    });
    expect(totals.mcp_server?.alpha).toEqual(dim(5, 1, 300, 125));
    expect(totals.mcp_server?.beta).toEqual(dim(1));
    expect(totals.language?.rust).toEqual(dim(1));
  });

  it('leaves a dimension kind absent when no session in view reports it', () => {
    const totals = aggregateToolDimensions({
      s1: rangeWithDimensions({ language: { python: dim(4) } }),
    });
    expect(totals.mcp_server).toBeUndefined();
    expect(totals.shell_family).toBeUndefined();
  });

  it('tolerates sessions with no tool_dimensions at all and a null/undefined map', () => {
    expect(aggregateToolDimensions(null)).toEqual({});
    expect(aggregateToolDimensions(undefined)).toEqual({});
    const totals = aggregateToolDimensions({
      s1: rangeWithDimensions(undefined),
      s2: rangeWithDimensions({ language: { go: dim(2) } }),
    });
    expect(totals).toEqual({ language: { go: dim(2) } });
  });
});

describe('topDimensionValues', () => {
  const totals: DimensionTotals = {
    shell_family: {
      git: dim(9),
      npm: dim(15),
      other: dim(3),
    },
    context_source: {
      conversation_cache: dim(0, 0, 0, 0, 500),
      unknown: dim(0, 0, 0, 0, 900),
    },
  };

  it('ranks by calls descending for call-shaped dimensions', () => {
    const rows = topDimensionValues(totals, 'shell_family');
    expect(rows.map((row) => row.value)).toEqual(['npm', 'git', 'other']);
  });

  it('ranks by tokens descending for context_source', () => {
    const rows = topDimensionValues(totals, 'context_source');
    expect(rows.map((row) => row.value)).toEqual(['unknown', 'conversation_cache']);
  });

  it('returns an empty array for a kind with no data, never throws', () => {
    expect(topDimensionValues(totals, 'mcp_server')).toEqual([]);
  });

  it('respects the limit', () => {
    const many: DimensionTotals = {
      language: Object.fromEntries(Array.from({ length: 10 }, (_, i) => [`lang${i}`, dim(i)])),
    };
    expect(topDimensionValues(many, 'language', 3)).toHaveLength(3);
  });
});

describe('dimensionExportRows', () => {
  const totals: DimensionTotals = {
    mcp_server: { alpha_server: dim(2, 1, 500, 300) },
    shell_family: { other: dim(1) },
    context_source: { unknown: dim(0, 0, 0, 0, 1200) },
  };

  it('flattens every (kind, value) pair into one row with all counters', () => {
    const rows = dimensionExportRows(totals);
    expect(rows).toEqual([
      { dimension_kind: 'context_source', dimension_value: 'unknown', calls: 0, failures: 0, output_bytes: 0, duration_ms: 0, tokens: 1200 },
      { dimension_kind: 'mcp_server', dimension_value: 'alpha_server', calls: 2, failures: 1, output_bytes: 500, duration_ms: 300, tokens: 0 },
      { dimension_kind: 'shell_family', dimension_value: 'other', calls: 1, failures: 0, output_bytes: 0, duration_ms: 0, tokens: 0 },
    ]);
  });

  it('produces a stable row order (sorted by kind, then value) across calls', () => {
    const a = dimensionExportRows(totals);
    const b = dimensionExportRows(totals);
    expect(a).toEqual(b);
  });

  it('returns no rows for empty totals', () => {
    expect(dimensionExportRows({})).toEqual([]);
  });

  it('never emits a field that is not one of the bounded, argument-free label or counter fields', () => {
    // Privacy boundary: every exported row's keys are exactly this fixed
    // set — no raw command/path/output field can slip in through a future
    // ToolDimensionMetrics addition without this test failing.
    const rows = dimensionExportRows(totals);
    for (const row of rows) {
      expect(Object.keys(row).sort()).toEqual(
        ['calls', 'dimension_kind', 'dimension_value', 'duration_ms', 'failures', 'output_bytes', 'tokens'].sort(),
      );
    }
  });

  it('round-trips through rowsToCsv without special characters breaking the sheet', () => {
    const csv = rowsToCsv(dimensionExportRows(totals));
    expect(csv).toContain('dimension_kind,dimension_value,calls,failures,output_bytes,duration_ms,tokens');
    expect(csv).toContain('mcp_server,alpha_server,2,1,500,300,0');
  });
});
