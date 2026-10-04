import type { TranscriptCursor, TranscriptPage, TranscriptRequest } from './types';

export interface TranscriptExportOptions {
  toolCalls: boolean;
  toolResults: boolean;
  reasoning: boolean;
  redactPhrases: string[];
}
export interface TranscriptExportPreview {
  html: string;
  complete: boolean;
  records: number;
  omitted: number;
  redactions: number;
  limited: boolean;
}
const MAX_PAGES = 40;
const MAX_HTML_BYTES = 4 * 1024 * 1024;
const encoder = new TextEncoder();
const escape = (value: string) => value.replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[c]!);

/** ponytail: heuristic redaction is not exhaustive; add provider-specific rules when demonstrated gaps require them. Review remains mandatory. Never log input. */
export function redactTranscriptText(text: string, phrases: string[]): { text: string; count: number } {
  let count = 0;
  const replace = (pattern: RegExp, label: string) => {
    text = text.replace(pattern, () => { count++; return `[${label} redacted]`; });
  };
  // Remove the whole material, not just the header or first line.
  replace(/-----BEGIN [^-\r\n]*PRIVATE KEY-----[\s\S]*?(?:-----END [^-\r\n]*PRIVATE KEY-----|$)/g, 'private key');
  replace(/\b(?:sk-(?:proj-)?[A-Za-z0-9_-]{8,}|(?:gh[pousr]_|github_pat_)[A-Za-z0-9_]{8,}|AKIA[A-Z0-9]{16})\b/g, 'credential');
  replace(/\bBearer\s+[^\s"'<>,;]+/gi, 'authorization');
  replace(/["']?(?:api[-_]?key|password|passwd|secret|client_secret|access_token|refresh_token|token)["']?\s*[:=]\s*(?:"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'|[^\s,;}]+)/gi, 'credential field');
  replace(/(?:https?|file):\/\/[^\s<>"']+/gi, 'URL');
  replace(/(?:[A-Za-z]:[\\/]|\\\\)[^\r\n<>"'|]+/g, 'local path');
  replace(/(?:^|(?<=[\s("'=]))\/(?!\/)[^\s<>"',;)]+/gm, 'local path');
  // Encoded attachments never belong in a transcript artifact, even after tool opt-in.
  replace(/data:[^\s"'<>]+/gi, 'attachment');
  replace(/["'](?:data|base64|b64_json)["']\s*:\s*"(?:\\.|[^"\\])*"/gi, 'attachment field');
  replace(/[A-Za-z0-9+/_=-]{160,}/g, 'encoded content');
  for (const phrase of phrases) {
    if (!phrase) continue;
    const parts = text.split(phrase);
    count += parts.length - 1;
    text = parts.join('[review phrase redacted]');
  }
  return { text, count };
}

type Entry = { anchor: string; role: string; time: string; blocks: { kind: string; html: string; call: string | null; name: string; anchor: string }[] };

/** Bounded, ephemeral projection of Rust-parsed presentation blocks. No raw JSON parsing. */
export async function previewTranscriptExport(
  sessionId: string,
  options: TranscriptExportOptions,
  read: (request: TranscriptRequest) => Promise<TranscriptPage>,
  cancelled: () => boolean = () => false,
): Promise<TranscriptExportPreview> {
  if (options.redactPhrases.length > 100 || options.redactPhrases.some(p => p.length > 256)) {
    throw new Error('Use at most 100 review phrases, each at most 256 characters.');
  }
  const entries: Entry[] = [];
  const calls = new Map<string, string>();
  const results = new Map<string, string>();
  const cursors = new Set<string>();
  let cursor: TranscriptCursor | null = null;
  let records = 0, omitted = 0, redactions = 0, bytes = 0;
  let complete = false, limited = false, gap = false;
  const clean = (value: string) => {
    const redacted = redactTranscriptText(value, options.redactPhrases);
    redactions += redacted.count;
    return escape(redacted.text);
  };
  outer: for (let pageIndex = 0; pageIndex < MAX_PAGES; pageIndex++) {
    if (cancelled()) throw new Error('Preview cancelled.');
    const page = await read({ session_id: sessionId, cursor, max_records: 25, max_bytes: 131_072 });
    if (cancelled()) throw new Error('Preview cancelled.');
    gap ||= page.issues.length > 0 || !['available', 'partial'].includes(page.availability);
    for (const record of page.records) {
      records++;
      gap ||= record.issue != null;
      const view = record.presentation;
      const role = ['user', 'assistant', 'system', 'tool'].includes(view?.role ?? '') ? view!.role! : 'record';
      const date = view?.timestamp ? new Date(view.timestamp) : null;
      const entry: Entry = { anchor: `record-${records}`, role, time: date && Number.isFinite(date.valueOf()) ? date.toISOString() : '', blocks: [] };
      for (const [blockIndex, block] of (view?.blocks ?? []).entries()) {
        const toolCall = block.kind === 'tool_call';
        const toolResult = ['tool_result', 'tool_error'].includes(block.kind);
        const allowed = (block.kind === 'text' && ['user', 'assistant'].includes(role)) || (toolCall && options.toolCalls) || (toolResult && options.toolResults) || (block.kind === 'reasoning' && options.reasoning);
        if (!allowed) { omitted++; continue; }
        const html = clean(block.text);
        const name = clean((block.name ?? '').slice(0, 100));
        // Include conservative markup overhead so many tiny blocks are bounded too.
        const size = encoder.encode(html + name).length + 400;
        if (bytes + size > MAX_HTML_BYTES) { limited = true; break outer; }
        bytes += size;
        const anchor = `${entry.anchor}-block-${blockIndex}`;
        entry.blocks.push({ kind: block.kind, html, call: block.call_id, name, anchor });
      }
      if (entry.blocks.length) entries.push(entry);
      else if (!view?.blocks.length) omitted++;
    }
    if (!page.next_cursor) { complete = page.source_complete && !gap; break; }
    const key = JSON.stringify(page.next_cursor);
    if (cursors.has(key)) { limited = true; break; }
    cursors.add(key);
    cursor = page.next_cursor;
    if (pageIndex === MAX_PAGES - 1) limited = true;
  }
  complete &&= !limited;
  for (const entry of entries) for (const block of entry.blocks) {
    if (!block.call) continue;
    if (block.kind === 'tool_call') calls.set(block.call, block.anchor);
    if (['tool_result', 'tool_error'].includes(block.kind)) results.set(block.call, block.anchor);
  }
  const coverage = complete ? 'Source read to its end' : 'Incomplete source excerpt';
  const warning = 'Automatic redaction is not exhaustive. Review every included section before sharing. Unknown records, attachments, and excluded content are omitted. This is an offline transcript excerpt, not an accounting report.';
  const index = entries.map(e => `<li><a href="#${e.anchor}">${e.role} · ${e.anchor}${e.time ? ` · ${e.time}` : ''}</a></li>`).join('');
  const content = entries.map(e => `<article id="${e.anchor}"><h2>${e.role} · ${e.anchor}</h2>${e.time ? `<p>${e.time}</p>` : ''}${e.blocks.map(b => {
    const peer = b.call ? (b.kind === 'tool_call' ? results.get(b.call) : calls.get(b.call)) : null;
    return `<section id="${b.anchor}"><h3>${b.kind.replaceAll('_', ' ')}${b.name ? ` · ${b.name}` : ''}</h3>${peer ? `<a href="#${peer}">Related tool ${b.kind === 'tool_call' ? 'result' : 'call'}</a>` : ''}<pre>${b.html}</pre></section>`;
  }).join('')}<a href="#index">Back to index</a></article>`).join('');
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-src 'none'"><title>Odometer transcript excerpt</title><style>body{font:16px/1.5 system-ui,sans-serif;max-width:960px;margin:auto;padding:24px;color:#202020;background:#fafafa}article{border-top:1px solid #aaa;margin-top:24px;padding-top:16px}pre{white-space:pre-wrap;overflow-wrap:anywhere;background:#fff;padding:16px;border:1px solid #ddd}a{color:#234fbd}h2{font-size:1.25rem}h3{font-size:1rem}.notice{border-left:4px solid #b66d00;padding:12px;background:#fff4df}nav{max-height:300px;overflow:auto}</style></head><body><h1>Odometer transcript excerpt</h1><p class="notice"><strong>${coverage}</strong>. ${warning}</p><p>${records} source records read. ${omitted} records or blocks omitted. ${redactions} redactions. ${limited ? 'Preview safety limit reached.' : ''}</p><p>Included: conversation text${options.toolCalls ? ', tool calls' : ''}${options.toolResults ? ', tool results' : ''}${options.reasoning ? ', reasoning' : ''}. Tool payloads may contain sensitive information.</p><nav id="index" aria-label="Record index"><ol>${index}</ol></nav>${content || '<p>No selected content is available.</p>'}</body></html>`;
  return { html, complete, records, omitted, redactions, limited };
}
