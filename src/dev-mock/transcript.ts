// Synthetic typed inspector fixture. Production parsing stays in Rust.
import type { TranscriptBlock, TranscriptPage, TranscriptRecord, TranscriptRequest } from '../lib/types';
function record(id: string, role: string, blocks: TranscriptBlock[]): TranscriptRecord {
  return { id, byte_offset: Number(id.split(':').at(-1)) * 100, byte_length: 100, raw_json: '{"fixture":"synthetic record"}', kind: 'response_item', message_id: null, issue: null, presentation: { role, timestamp: '2026-07-29T08:00:00Z', blocks } };
}
const rows = [
  record('synthetic:0', 'user', [{ kind: 'text', text: 'Update the greeting in the synthetic demo.', name: null, call_id: null, edit: null }]),
  record('synthetic:1', 'assistant', [{ kind: 'tool_call', text: '{"file_path":"demo.ts"}', name: 'Read', call_id: 'read-1', edit: null }]),
  record('synthetic:2', 'tool', [{ kind: 'tool_result', text: 'export const greeting = "Hello";', name: null, call_id: 'read-1', edit: null }]),
  record('synthetic:3', 'assistant', [{ kind: 'tool_call', text: '', name: 'Edit', call_id: 'edit-1', edit: { path: 'demo.ts', before: 'export const greeting = "Hello";', after: 'export const greeting = "Welcome";' } }]),
];
export function transcriptFixture(request: TranscriptRequest): TranscriptPage {
  const offset = request.record_id ? rows.findIndex(row => row.id === request.record_id) : request.cursor?.offset ?? 0;
  if (offset < 0) return { provider: 'codex', availability: 'cursor_invalid', records: [], issues: ['record_anchor_changed'], next_cursor: null, source_complete: false };
  const result = rows.slice(offset, offset + 2);
  const next = offset + result.length;
  const anchored = !!request.record_id && offset > 0;
  const partial = anchored || !!request.cursor?.partial;
  return { provider: 'codex', availability: partial ? 'partial' : 'available', records: result, issues: anchored ? ['earlier_records_not_inspected'] : partial ? ['earlier_records_omitted'] : [], source_complete: !partial && next >= rows.length, next_cursor: next >= rows.length ? null : { session_id: request.session_id, source_id: 'synthetic', generation: 'fixture', offset: next, record_start: next, partial } };
}
