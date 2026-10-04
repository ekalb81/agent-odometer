import type { Session, TranscriptPage, TranscriptRequest, TranscriptCursor, TurnInfo } from './types';
import { escapeTranscriptHtml, redactTranscriptText } from './transcriptExport';

export interface HandoffRecord {
  key: string;
  reference: string;
  timestamp: string | null;
  role: string;
  text: string;
  recordedState: string;
  /** Kept only in the preparation UI for the existing exact-record inspector. */
  sourceAnchor: string | null;
  truncated: boolean;
}
export interface HandoffSelection {
  records: HandoffRecord[];
  coverage: string;
}
const MAX_RECORDS = 200;
const MAX_TEXT_CHARS = 16_000;
const MAX_SOURCE_CHARS = 1_000_000;
const stamp = (value: string | null) => value && Number.isFinite(Date.parse(value)) ? new Date(value).toISOString() : null;
const role = (value: string | null) => ['user', 'assistant', 'tool', 'system'].includes(value ?? '') ? value! : 'record';

function turnState(turn: TurnInfo): string {
  const state = ['in_progress', 'completed', 'aborted', 'rolled_back'].includes(turn.status) ? turn.status : 'unknown';
  const count = (value: number | undefined) => Number.isSafeInteger(value) && value! >= 0 ? String(value) : 'unknown';
  return `Recorded turn state: ${state}. Tool calls: ${count(turn.tool_metrics?.calls)}; successes: ${count(turn.tool_metrics?.successes)}; failures: ${count(turn.tool_metrics?.failures)}. Recorded completion does not establish acceptance or quality.`;
}

/** Only fields from the selected session are admitted; parent/subagent contents
 * are never traversed. Retained turn summaries are explicitly incomplete. */
export function retainedHandoff(session: Pick<Session, 'turns' | 'source_availability'>): HandoffSelection {
  const records: HandoffRecord[] = [];
  for (const [position, turn] of session.turns.slice(-50).entries()) {
    for (const [field, text, author, time] of [
      ['user_message', turn.user_message, 'user', turn.started_at],
      ['last_agent_message', turn.last_agent_message, 'assistant', turn.completed_at],
    ] as const) {
      if (!text) continue;
      records.push({ key: `retained:${position}:${field}`, reference: `Retained turn ${turn.index} · ${field}`,
        timestamp: stamp(time), role: author, text: text.slice(0, MAX_TEXT_CHARS), recordedState: turnState(turn),
        sourceAnchor: null, truncated: text.length > MAX_TEXT_CHARS });
    }
  }
  return { records, coverage: `Retained user and last-agent fields from at most the latest 50 turns. These are summaries, not a complete conversation. ${session.source_availability === 'missing' ? 'Session metadata reports the original source missing.' : 'Original source completeness is not established by retained fields.'} Parent and subagent sessions are excluded.` };
}

/** Reuse the existing Rust presentation reader, retaining neither raw JSON nor
 * omitted attachment/reasoning bodies. Tool payloads require explicit opt-in. */
export async function sourceHandoff(sessionId: string, tools: boolean,
  read: (request: TranscriptRequest) => Promise<TranscriptPage>, cancelled: () => boolean = () => false,
): Promise<HandoffSelection> {
  const records: HandoffRecord[] = [];
  const seen = new Set<string>();
  const recordKeys = new Set<string>();
  let cursor: TranscriptCursor | null = null;
  let chars = 0, gap = false, complete = false, limited = false;
  pages: for (let pageIndex = 0; pageIndex < 8; pageIndex++) {
    if (cancelled()) throw new Error('Handoff loading cancelled.');
    const page = await read({ session_id: sessionId, cursor, max_records: 25, max_bytes: 131_072 });
    if (cancelled()) throw new Error('Handoff loading cancelled.');
    gap ||= page.issues.length > 0 || page.availability !== 'available';
    for (const record of page.records) {
      gap ||= !!record.issue;
      const view = record.presentation;
      for (const [index, block] of (view?.blocks ?? []).entries()) {
        const isTool = ['tool_call', 'tool_result', 'tool_error'].includes(block.kind);
        if (!((block.kind === 'text' && ['user', 'assistant'].includes(view?.role ?? '')) || (tools && isTool))) continue;
        const key = `source:${record.id}:${index}`;
        if (recordKeys.has(key)) { gap = true; continue; }
        recordKeys.add(key);
        const text = block.text.slice(0, MAX_TEXT_CHARS);
        if (records.length === MAX_RECORDS || chars + text.length > MAX_SOURCE_CHARS) { limited = true; break pages; }
        chars += text.length;
        records.push({ key, reference: `Source record at byte ${record.byte_offset} · ${isTool ? block.kind : 'conversation text'}`,
          timestamp: stamp(view?.timestamp ?? null), role: role(view?.role ?? null), text,
          recordedState: 'No turn or task outcome is inferred from this record.', sourceAnchor: record.id, truncated: block.text.length > MAX_TEXT_CHARS });
      }
    }
    if (!page.next_cursor) { complete = page.source_complete && !gap; break; }
    const next = JSON.stringify(page.next_cursor);
    if (seen.has(next)) { limited = true; break; }
    seen.add(next); cursor = page.next_cursor;
    if (pageIndex === 7) limited = true;
  }
  return { records, coverage: `${complete && !limited ? 'Source read to its end.' : 'Incomplete original source: missing, changed, unreadable, or bounded records may leave gaps.'} Unknown records, attachments, reasoning, and ${tools ? 'unselected' : 'tool'} content are omitted. ${limited ? 'The bounded source-read limit was reached. ' : ''}Parent and subagent sessions are excluded.` };
}

export function prepareHandoff(selection: HandoffSelection, selectedKeys: string[], note: string, phrases: string[]): { markdown: string; html: string; redactions: number } {
  if (!selectedKeys.length || selectedKeys.length > 30) throw new Error('Select 1–30 records for a concise handoff.');
  if (note.length > 5_000 || phrases.length > 100 || phrases.some((phrase) => phrase.length > 256)) throw new Error('The note or review phrases exceed the handoff limit.');
  const keys = new Set(selectedKeys);
  const included = selection.records.filter((record) => keys.has(record.key));
  if (keys.size !== selectedKeys.length || included.length !== keys.size) throw new Error('The selection changed. Select its records again.');
  let redactions = 0;
  const clean = (text: string) => { const result = redactTranscriptText(text, phrases); redactions += result.count; return result.text; };
  // A longer fence keeps arbitrary source backticks/HTML inside literal text.
  const literal = (text: string) => { const fence = '`'.repeat(Math.max(3, ...[...text.matchAll(/`+/g)].map((match) => match[0].length + 1))); return `${fence}text\n${text}\n${fence}`; };
  const sections = included.map((record, index) => `## ${index + 1}. ${record.reference}\n\nRole: ${record.role}. Timestamp: ${record.timestamp ?? 'not recorded'}.\n\n${record.recordedState}\n\n${record.truncated ? 'Content truncated at the preparation limit.\n\n' : ''}${literal(clean(record.text))}`);
  const markdown = `# Odometer handoff\n\n${selection.coverage}\n\nOnly the explicitly selected records below are included. Retained and source records can overlap. Automatic redaction is not exhaustive; review this artifact before sharing. This is recorded context, not instructions to execute tools or proof of accepted work.\n\n${note.trim() ? `## User-written task state and next steps\n\n${literal(clean(note))}\n\n` : ''}${sections.join('\n\n')}\n`;
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'"><title>Odometer handoff</title><style>body{max-width:960px;margin:auto;padding:24px;background:#fff;color:#222;font:15px/1.5 system-ui,sans-serif}pre{white-space:pre-wrap;overflow-wrap:anywhere}</style></head><body><pre>${escapeTranscriptHtml(markdown)}</pre></body></html>`;
  return { markdown, html, redactions };
}
