import type { ExecutionRecord, SessionSummary } from './types';

export const MAX_BOARD_SESSIONS = 8;
export const MAX_BOARD_RECORDS = 100;
export interface ActivityBlock {
  recordId: string; resultId: string | null; label: string;
  start: number | null; end: number | null; unresolved: string | null;
}
export function timestamp(value: string | null): number | null {
  if (!value) return null;
  const result = Date.parse(value);
  return Number.isFinite(result) ? result : null;
}
/** Explicit same-provider identity only. Cycles and duplicate parents remain unresolved. */
export function parentRelations(sessions: SessionSummary[]): Map<string, string | null> {
  const result = new Map<string, string | null>();
  for (const session of sessions) {
    const candidates = sessions.filter(other => other.harness === session.harness && other.id === session.parent_thread_id);
    result.set(session.storage_id, candidates.length === 1 && candidates[0].storage_id !== session.storage_id ? candidates[0].storage_id : null);
  }
  for (const session of sessions) {
    const seen = new Set<string>();
    let id: string | null = session.storage_id;
    while (id !== null) {
      if (seen.has(id)) { for (const cyclic of seen) result.set(cyclic, null); break; }
      seen.add(id); id = result.get(id) ?? null;
    }
  }
  return result;
}

/** Pair only a unique call/result with recorded timestamps in the visited bounded pages. */
export function activityBlocks(records: ExecutionRecord[]): ActivityBlock[] {
  const calls = new Map<string, { record: ExecutionRecord; name: string | null }[]>();
  const results = new Map<string, ExecutionRecord[]>();
  for (const record of records) for (const block of record.blocks) {
    if (!block.call_id) continue;
    if (block.kind === 'tool_call') calls.set(block.call_id, [...(calls.get(block.call_id) ?? []), { record, name: block.name }]);
    if (['tool_result', 'tool_error'].includes(block.kind)) results.set(block.call_id, [...(results.get(block.call_id) ?? []), record]);
  }
  return records.flatMap(record => {
    const tools = record.blocks.filter(block => block.kind === 'tool_call');
    if (tools.length === 0) return [{ recordId: record.record_id, resultId: null, label: record.role ?? 'Recorded activity', start: timestamp(record.timestamp), end: null, unresolved: record.issue ?? (timestamp(record.timestamp) === null ? 'Timestamp unavailable' : null) }];
    return tools.map(tool => {
      const start = timestamp(record.timestamp);
      const match = tool.call_id && calls.get(tool.call_id)?.length === 1 && results.get(tool.call_id)?.length === 1 ? results.get(tool.call_id)![0] : null;
      const end = timestamp(match?.timestamp ?? null);
      const valid = start !== null && end !== null && end >= start;
      return { recordId: record.record_id, resultId: valid ? match!.record_id : null,
        label: tool.name ?? 'Tool call', start, end: valid ? end : null,
        unresolved: record.issue ?? (valid ? null : 'Tool relationship or timing unresolved in visited pages') };
    });
  }).slice(0, MAX_BOARD_RECORDS);
}
