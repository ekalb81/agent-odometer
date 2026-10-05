import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import { tick } from 'svelte';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { historyStore } from '../lib/stores/history.svelte';
import { sessionsStore } from '../lib/stores/sessions.svelte';
import { rates } from '../lib/stores/rates';
import { zeroToolMetrics, zeroTotals } from '../lib/sessionProjection';
import type { CorrelationResult, ExternalEvent, RateCard, SessionSummary } from '../lib/types';
import GitOutcomes from './GitOutcomes.svelte';

const { correlateEvents, listExternalEvents, scanGitOutcomes } = vi.hoisted(() => ({
  correlateEvents: vi.fn(), listExternalEvents: vi.fn(), scanGitOutcomes: vi.fn(),
}));
vi.mock('../lib/ipc', () => ({ correlateEvents, listExternalEvents, scanGitOutcomes }));

const session = { id: 'synthetic', storage_id: 'codex:thread:synthetic', started_at: '2026-01-01T00:00:00Z', last_event_at: '2026-01-01T01:00:00Z' } as SessionSummary;
const event: ExternalEvent = { id: 'git-fixture', timestamp: session.started_at, scope: null, source: 'git', kind: 'commit', metadata: { session_id: session.storage_id } };
const observation = { session_count: 3, turn_count: 3, session_duration_ms: 180_000, tokens: zeroTotals(), buckets_by_harness: {}, tool_metrics: zeroToolMetrics() };
function result(token_delta = 321): CorrelationResult {
  return { results: [{ event, before: observation, after: observation, after_window_end: '2026-01-08T00:00:00Z', after_window_complete: true, minimum_session_count: 3, sample_ready: true, token_delta, session_delta: 0, confounding_event_ids: [], warnings: [] }] };
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
async function scan() { await fireEvent.click(screen.getByRole('button', { name: 'Evaluate local repositories' })); }
async function rendered() {
  render(GitOutcomes);
  await tick();
  await fireEvent.click(screen.getByText('Local git outcomes'));
  await scan();
  await waitFor(() => expect(screen.getByText(/7d token delta \+321/)).toBeInTheDocument());
}

describe('GitOutcomes accounting invalidation', () => {
  beforeEach(() => {
    vi.resetAllMocks();
    sessionsStore.replaceAll([session]);
    historyStore.set({ status: 'ready', step: null, step_index: null, step_total: null, items_done: null, items_total: null, elapsed_ms: null });
    rates.set(null);
    scanGitOutcomes.mockResolvedValue([{ session_id: session.storage_id, repository_scope: null, kind: 'kept', commit_ids: ['synthetic-commit'], evidence: 'Synthetic commit remains reachable' }]);
    listExternalEvents.mockResolvedValue([event]);
    correlateEvents.mockResolvedValue(result());
  });

  it('clears a previous comparison before rescanning and withholds it after a guarded failure', async () => {
    await rendered();
    const pending = deferred<CorrelationResult>();
    correlateEvents.mockReturnValueOnce(pending.promise);
    await scan();
    expect(screen.queryByText(/7d token delta/)).not.toBeInTheDocument();
    pending.reject(new Error('accounting_identity_ambiguous: private source path'));
    await waitFor(() => expect(screen.getByTestId('accounting-git-outcomes-status')).toHaveTextContent(/ambiguous accounting identities/));
    expect(screen.queryByText(/private source path/)).not.toBeInTheDocument();
    expect(screen.getByText(/Synthetic commit remains reachable/)).toBeInTheDocument();
    expect(screen.queryByText(/7d token delta/)).not.toBeInTheDocument();
  });

  it.each(['session', 'history', 'rate', 'window'] as const)('invalidates numeric evidence on a %s change without rescanning repositories', async (change) => {
    await rendered();
    if (change === 'session') sessionsStore.upsert({ ...session, last_event_at: '2026-01-01T02:00:00Z' });
    if (change === 'history') historyStore.set({ ...historyStore.status, status: 'pending' });
    if (change === 'rate') rates.set({ version: 1 } as RateCard);
    if (change === 'window') await fireEvent.input(screen.getByRole('spinbutton'), { target: { value: '48' } });
    await tick();
    expect(screen.queryByText(/7d token delta/)).not.toBeInTheDocument();
    expect(screen.getByText(/Synthetic commit remains reachable/)).toBeInTheDocument();
    expect(scanGitOutcomes).toHaveBeenCalledTimes(1);
  });

  it.each(['success', 'failure'] as const)('rejects a superseded comparison %s after a newer evaluation completes', async (completion) => {
    const pending = deferred<CorrelationResult>();
    correlateEvents.mockReturnValueOnce(pending.promise);
    render(GitOutcomes);
    await tick();
    await scan();
    await waitFor(() => expect(correlateEvents).toHaveBeenCalledTimes(1));
    sessionsStore.upsert({ ...session, last_event_at: '2026-01-01T02:00:00Z' });
    await tick();
    correlateEvents.mockResolvedValue(result(902));
    await scan();
    await waitFor(() => expect(screen.getByText(/7d token delta \+902/)).toBeInTheDocument());
    if (completion === 'success') pending.resolve(result(123));
    else pending.reject(new Error('accounting_identity_unverified: stale failure'));
    await tick();
    expect(screen.getByText(/7d token delta \+902/)).toBeInTheDocument();
    expect(screen.queryByText(/7d token delta \+123/)).not.toBeInTheDocument();
    expect(screen.queryByTestId('accounting-git-outcomes-status')).not.toBeInTheDocument();
  });
});
