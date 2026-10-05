import { describe, expect, it } from 'vitest';
import { activityBlocks, parentRelations } from './executionBoard';
import type { ExecutionRecord, SessionSummary } from './types';
const session = (id: string, parent: string | null, harness = 'codex') => ({ id, storage_id: `${harness}:${id}`, harness, parent_thread_id: parent }) as SessionSummary;
const record = (id: string, kind: string, time: string | null, call = 'same'): ExecutionRecord => ({ record_id: id, timestamp: time, role: 'assistant', issue: null, blocks: [{ kind, call_id: call, name: 'Read' }] });
describe('execution evidence', () => {
  it('resolves only unique same-provider parent links and rejects cycles', () => {
    expect(parentRelations([session('a', 'b'), session('b', 'a')]).get('codex:a')).toBeNull();
    expect(parentRelations([session('a', 'b'), session('b', null, 'claude_code')]).get('codex:a')).toBeNull();
    expect(parentRelations([session('a', 'b'), session('b', null)]).get('codex:a')).toBe('codex:b');
    const duplicate = { ...session('b', null), storage_id: 'another-b' };
    expect(parentRelations([session('a', 'b'), session('b', null), duplicate]).get('codex:a')).toBeNull();
  });
  it('keeps exact anchors and pairs only unique recorded timestamps', () => {
    const call = record('call-anchor', 'tool_call', '2026-01-01T00:00:00Z');
    const result = record('result-anchor', 'tool_result', '2026-01-01T00:00:02Z');
    expect(activityBlocks([call, result])[0]).toMatchObject({ recordId: 'call-anchor', resultId: 'result-anchor', end: Date.parse(result.timestamp!) });
    expect(activityBlocks([call, result, { ...call, record_id: 'duplicate' }])[0].unresolved).toContain('unresolved');
    expect(activityBlocks([call, { ...result, timestamp: null }])[0].end).toBeNull();
    expect(activityBlocks([call, { ...result, timestamp: '2025-01-01T00:00:00Z' }])[0].end).toBeNull();
  });
});
