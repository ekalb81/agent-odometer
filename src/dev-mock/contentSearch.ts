// Synthetic presentation responses only. Production matching and source reads
// belong to Rust; these fixtures do not establish backend search correctness.
import type { RetainedSearchLanding, TranscriptSearchHit, TranscriptSearchPage, TranscriptSearchRequest, TranscriptSearchTarget } from '../lib/types';
export function contentSearchFixture(request: TranscriptSearchRequest, retained = false): TranscriptSearchPage {
  const retainedPhase = request.cursor?.position.phase === 'retained';
  const prompt = 'Update the greeting in the synthetic demo.';
  const output = 'export const greeting = "Hello";';
  const hit = (target: TranscriptSearchTarget, text: string, content_kind: string): TranscriptSearchHit => ({ target, content_kind, snippet: { text, match_start: text.indexOf('greeting'), match_end: text.indexOf('greeting') + 8, truncated_before: false, truncated_after: false } });
  if (retained && !retainedPhase) return { phase: 'source', hits: [], issues: ['source_missing'], source_complete: false, retained_complete: false, scanned_records: 0, scanned_messages: 0, next_cursor: request.scope.conversation ? { session_id: request.session_id, query: request.query, scope: request.scope, position: { phase: 'retained', session_identity: 'synthetic-lineage', snapshot_revision: 'synthetic-snapshot', next_turn: 0, incomplete: false } } : null };
  const hits: TranscriptSearchHit[] = [];
  if (request.query === 'greeting') {
    if (retainedPhase) hits.push(hit({ kind: 'retained_turn', session_id: request.session_id, session_identity: 'synthetic-lineage', snapshot_revision: 'synthetic-snapshot', turn_id: 'synthetic-turn', field: 'user_message' }, prompt, 'user_message'));
    else {
      if (request.scope.conversation) hits.push(hit({ kind: 'source_record', session_id: request.session_id, record_id: 'synthetic:0', block_index: 0 }, prompt, 'text'));
      if (request.scope.tool_results) hits.push(hit({ kind: 'source_record', session_id: request.session_id, record_id: 'synthetic:2', block_index: 0 }, output, 'tool_result'));
    }
  }
  return { phase: retainedPhase ? 'retained' : 'source', hits, next_cursor: null, issues: retainedPhase ? ['retained_prompt_and_final_reply_only', 'retained_matches_may_overlap_source'] : [], source_complete: !retainedPhase, retained_complete: retainedPhase, scanned_records: retainedPhase ? 0 : 4, scanned_messages: retainedPhase ? 2 : 0 };
}
export function retainedLandingFixture(target: TranscriptSearchTarget): RetainedSearchLanding {
  if (target.kind !== 'retained_turn' || target.session_identity !== 'synthetic-lineage' || target.snapshot_revision !== 'synthetic-snapshot' || target.turn_id !== 'synthetic-turn' || target.field !== 'user_message') throw new Error('retained_target_unavailable');
  return { target, text: 'Update the greeting in the synthetic demo.', truncated: false };
}
