import { describe, expect, it } from 'vitest';
import { sessionsStore } from './sessions.svelte';
import type { SessionSummary, TokenTotals } from '../types';

const zero: TokenTotals = {
  input_tokens: 0, cached_input_tokens: 0, cache_creation_input_tokens: 0, output_tokens: 0,
  reasoning_output_tokens: 0, total_tokens: 0,
};

function summary(id: string): SessionSummary {
  return {
    id,
    storage_id: id,
    harness: 'codex',
    thread_name: null,
    forked_from_id: null,
    parent_thread_id: null,
    agent_path: null,
    agent_nickname: null,
    file_path: `${id}.jsonl`,
    source_availability: 'present',
    archived: false,
    started_at: '2026-08-01T00:00:00Z',
    last_event_at: '2026-08-01T01:00:00Z',
    working_directory: null,
    originator: null,
    source: null,
    cli_version: null,
    model_provider: null,
    model: null,
    service_tier: null,
    plan_type: null,
    credits_unlimited: null,
    credits_balance: null,
    context_window: null,
    total_turns: 0,
    first_user_message: null,
    tokens_total: zero,
    buckets: [],
    category_totals: {},
    optimization_findings_count: 0,
  } as unknown as SessionSummary;
}

describe('sessionsStore mutation log', () => {
  it('reports batch mutations, removals, and replacement with increasing generations', () => {
    sessionsStore.replaceAll([summary('a')]);
    const afterReplace = sessionsStore.mutationLog;
    expect(afterReplace.replaced).toBe(true);

    sessionsStore.applyMutations([summary('b'), summary('c')], ['a']);
    const afterBatch = sessionsStore.mutationLog;
    expect(afterBatch.generation).toBeGreaterThan(afterReplace.generation);
    expect(afterBatch.replaced).toBe(false);
    expect(afterBatch.changed.sort()).toEqual(['b', 'c']);
    expect(afterBatch.removed).toEqual(['a']);

    sessionsStore.upsert(summary('d'));
    expect(sessionsStore.mutationLog.changed).toEqual(['d']);
    expect(sessionsStore.mutationLog.replaced).toBe(false);

    sessionsStore.remove('d');
    expect(sessionsStore.mutationLog.removed).toEqual(['d']);
    expect(sessionsStore.map.has('d')).toBe(false);

    // Empty batches do not advance the log.
    const before = sessionsStore.mutationLog.generation;
    sessionsStore.applyMutations([], []);
    expect(sessionsStore.mutationLog.generation).toBe(before);
  });
});
