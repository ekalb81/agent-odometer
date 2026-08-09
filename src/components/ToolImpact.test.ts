import { render, screen, waitFor } from '@testing-library/svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import type {
  TokenTotals,
  ToolImpactCohort,
  ToolImpactResult,
  ToolImpactTarget,
  ToolMetrics,
} from '../lib/types';
import ToolImpact from './ToolImpact.svelte';

const { compareToolImpact, listToolImpactTargets, writeExport } = vi.hoisted(() => ({
  compareToolImpact: vi.fn(),
  listToolImpactTargets: vi.fn(),
  writeExport: vi.fn(),
}));

vi.mock('../lib/ipc', () => ({ compareToolImpact, listToolImpactTargets, writeExport }));

function tokens(total: number): TokenTotals {
  return {
    input_tokens: total,
    cached_input_tokens: 0,
    cache_creation_input_tokens: 0,
    output_tokens: 0,
    reasoning_output_tokens: 0,
    total_tokens: total,
  };
}

function toolMetrics(): ToolMetrics {
  return {
    calls: 10,
    reads: 0,
    searches: 0,
    mutations: 0,
    commands: 0,
    other: 10,
    successes: 10,
    failures: 0,
    unknown: 0,
    mutation_targets: 0,
    one_shot_mutations: 0,
    retry_count: 0,
    duration_ms: 0,
    output_bytes: 0,
  };
}

function cohort(turnCount: number, totalTokens: number): ToolImpactCohort {
  return {
    turn_count: turnCount,
    session_count: 1,
    completed_turn_count: turnCount,
    duration_sample_count: turnCount,
    total_duration_ms: turnCount * 1_000,
    ttft_sample_count: turnCount,
    total_ttft_ms: turnCount * 100,
    tokens: tokens(totalTokens),
    buckets: [],
    tool_metrics: toolMetrics(),
  };
}

function target(key: string, label: string): ToolImpactTarget {
  return { kind: 'tool', key, label, turn_count: 20, call_count: 40 };
}

function result(targetKey: string, observedTokens: number): ToolImpactResult {
  return {
    target_kind: 'tool',
    target_key: targetKey,
    observed: cohort(20, observedTokens),
    baseline: cohort(20, 1_000),
    matched_observed: cohort(10, observedTokens),
    matched_baseline: cohort(10, 1_000),
    matched_pairs: 10,
    warnings: [],
  };
}

const BASE_PROPS = {
  sessionIds: ['a', 'b'],
  from: '2026-08-01T00:00:00.000Z',
  to: '2026-08-07T00:00:00.000Z',
  windowLabel: 'All time',
};

const LOADING_TARGETS = /Finding observed tools and providers/;
const LOADING_COMPARISON = /Comparing observed and baseline turns/;

beforeEach(() => {
  compareToolImpact.mockReset();
  listToolImpactTargets.mockReset();
  writeExport.mockReset();
});

describe('ToolImpact', () => {
  it('shows loading placeholders only on the first load', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    compareToolImpact.mockResolvedValue(result('grep', 2_000));

    render(ToolImpact, { props: { ...BASE_PROPS } });

    expect(screen.getByText(LOADING_TARGETS)).toBeTruthy();
    await waitFor(() => expect(screen.getByText('Tokens / turn')).toBeTruthy());
  });

  // Regression: live sessions re-run both fetches in the background. Tearing the
  // rendered table down to a one-line placeholder each time made this section —
  // and everything below it in the scrolling analytics panel — visibly jump.
  it('keeps the comparison table rendered across a background refresh', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    compareToolImpact.mockResolvedValue(result('grep', 2_000));

    const { rerender } = render(ToolImpact, { props: { ...BASE_PROPS } });
    await waitFor(() => expect(screen.getByText('Tokens / turn')).toBeTruthy());

    // Both fetches stay pending, standing in for a slow in-flight refresh.
    listToolImpactTargets.mockReturnValue(new Promise(() => {}));
    compareToolImpact.mockReturnValue(new Promise(() => {}));

    // A fresh array with the same ids is exactly what a store flush produced.
    await rerender({ ...BASE_PROPS, sessionIds: ['a', 'b'] });
    await waitFor(() => expect(listToolImpactTargets).toHaveBeenCalledTimes(2));

    expect(screen.queryByText(LOADING_TARGETS)).toBeNull();
    expect(screen.queryByText(LOADING_COMPARISON)).toBeNull();
    expect(screen.getByText('Tokens / turn')).toBeTruthy();
  });

  // Regression: an error is a real state worth surfacing, but replacing the
  // table with it displaced everything below just as the loading placeholder
  // did. Once there is content, a failed refresh reports itself inline.
  it('reports a failed background refresh inline, keeping the table', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    compareToolImpact.mockResolvedValue(result('grep', 2_000));

    const { rerender } = render(ToolImpact, { props: { ...BASE_PROPS } });
    await waitFor(() => expect(screen.getByText('Tokens / turn')).toBeTruthy());

    compareToolImpact.mockRejectedValue(new Error('backend unavailable'));
    listToolImpactTargets.mockRejectedValue(new Error('backend unavailable'));
    await rerender({ ...BASE_PROPS, sessionIds: ['a', 'b'] });

    const comparisonNotice = await screen.findByText(/Showing the last successful comparison/);
    expect(comparisonNotice.getAttribute('title')).toContain('backend unavailable');
    const targetNotice = screen.getByText(/Target list is stale/);
    expect(targetNotice.getAttribute('title')).toContain('backend unavailable');
    // The numbers are still on screen, and so is the target picker.
    expect(screen.getByText('Tokens / turn')).toBeTruthy();
    expect(screen.getByLabelText('Tool impact target')).toBeTruthy();
  });

  it('shows a failed first load as the section body, with nothing to fall back on', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    compareToolImpact.mockRejectedValue(new Error('backend unavailable'));

    render(ToolImpact, { props: { ...BASE_PROPS } });

    await waitFor(() => expect(screen.getByText(/backend unavailable/)).toBeTruthy());
    expect(screen.queryByText('Tokens / turn')).toBeNull();
    expect(screen.queryByText(/Showing the last successful comparison/)).toBeNull();
  });

  it('drops the previous numbers when the compared target changes', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep'), target('bash', 'bash')]);
    compareToolImpact.mockResolvedValue(result('grep', 2_000));

    render(ToolImpact, { props: { ...BASE_PROPS } });
    await waitFor(() => expect(screen.getByText('Tokens / turn')).toBeTruthy());

    compareToolImpact.mockReturnValue(new Promise(() => {}));
    const select = screen.getByLabelText('Tool impact target') as HTMLSelectElement;
    select.value = 'tool:bash';
    select.dispatchEvent(new Event('change', { bubbles: true }));

    await waitFor(() => expect(screen.getByText(LOADING_COMPARISON)).toBeTruthy());
    expect(screen.queryByText('Tokens / turn')).toBeNull();
  });
});
