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

  it('withholds the same-target report and numeric targets while refreshed scope is pending', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    compareToolImpact.mockResolvedValue(result('grep', 2_000));
    const { rerender } = render(ToolImpact, { props: { ...BASE_PROPS } });
    await waitFor(() => expect(screen.getByText('Tokens / turn')).toBeTruthy());
    const panel = screen.getByText(/Tool impact comparison/).parentElement as HTMLDetailsElement;
    panel.open = true;
    listToolImpactTargets.mockReturnValue(new Promise(() => {}));
    await rerender({ ...BASE_PROPS, sessionIds: ['a', 'b'] });
    await waitFor(() => expect(listToolImpactTargets).toHaveBeenCalledTimes(2));
    expect(screen.queryByText('Tokens / turn')).toBeNull();
    expect(screen.queryByText(/grep · 20 turns/)).toBeNull();
    expect(screen.getByText(LOADING_TARGETS)).toBeTruthy();
    expect(panel.open).toBe(true);
  });

  it('withholds an old same-target report when the open-ended minute refresh fails proof', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    compareToolImpact.mockResolvedValue(result('grep', 2_000));
    const { rerender } = render(ToolImpact, { props: { ...BASE_PROPS } });
    await waitFor(() => expect(screen.getByText('Tokens / turn')).toBeTruthy());
    compareToolImpact.mockRejectedValue(new Error('accounting_identity_ambiguous: private detail'));
    await rerender({ ...BASE_PROPS, to: '2026-08-07T00:01:00.000Z' });
    await screen.findByText(/ambiguous accounting identities/i);
    expect(screen.queryByText('Tokens / turn')).toBeNull();
    expect(screen.queryByText(/private detail/)).toBeNull();
    expect((screen.getByLabelText('Tool impact target') as HTMLSelectElement).value).toBe('tool:grep');
  });

  it('rejects superseded successful and failed comparisons after a newer minute completes', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    let oldSuccess!: (value: ToolImpactResult) => void;
    let oldFailure!: (reason: Error) => void;
    compareToolImpact.mockImplementationOnce(() => new Promise(resolve => { oldSuccess = resolve; }));
    const { rerender } = render(ToolImpact, { props: { ...BASE_PROPS } });
    await waitFor(() => expect(compareToolImpact).toHaveBeenCalledTimes(1));
    compareToolImpact.mockImplementationOnce(() => new Promise((_resolve, reject) => { oldFailure = reject; }));
    await rerender({ ...BASE_PROPS, to: '2026-08-07T00:01:00.000Z' });
    await waitFor(() => expect(compareToolImpact).toHaveBeenCalledTimes(2));
    compareToolImpact.mockResolvedValue(result('grep', 9_000));
    await rerender({ ...BASE_PROPS, to: '2026-08-07T00:02:00.000Z' });
    await screen.findByText('900');
    oldSuccess(result('grep', 2_000));
    oldFailure(new Error('accounting_identity_unverified: old proof'));
    await waitFor(() => expect(screen.getByText('900')).toBeTruthy());
    expect(screen.queryByText('200')).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  it('clears numeric target counts and comparison after a current target proof fails', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    compareToolImpact.mockResolvedValue(result('grep', 2_000));
    const { rerender } = render(ToolImpact, { props: { ...BASE_PROPS } });
    await screen.findByText('Tokens / turn');
    listToolImpactTargets.mockRejectedValue(new Error('accounting_identity_unverified: private detail'));
    await rerender({ ...BASE_PROPS, to: '2026-08-07T00:01:00.000Z' });
    await screen.findByText(/accounting identity verification is incomplete/i);
    expect(screen.queryByText('Tokens / turn')).toBeNull();
    expect(screen.queryByText(/grep · 20 turns/)).toBeNull();
    expect(screen.queryByText(/private detail/)).toBeNull();
  });

  it('shows a failed first load as the section body, with nothing to fall back on', async () => {
    listToolImpactTargets.mockResolvedValue([target('grep', 'grep')]);
    compareToolImpact.mockRejectedValue(new Error('backend unavailable'));

    render(ToolImpact, { props: { ...BASE_PROPS } });

    await waitFor(() => expect(screen.getByText(/complete accounting scope could not be verified/i)).toBeTruthy());
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
