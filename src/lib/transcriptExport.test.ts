import { describe, expect, it, vi } from 'vitest';
import { previewTranscriptExport, redactTranscriptText, type TranscriptExportOptions } from './transcriptExport';
import type { TranscriptBlock, TranscriptCursor, TranscriptPage, TranscriptRecord } from './types';
import redactionPolicy from './fixtures/transcriptRedaction.json';
const options: TranscriptExportOptions = { toolCalls: false, toolResults: false, reasoning: false, redactPhrases: [] };
const block = (kind: string, text: string, call_id: string | null = null): TranscriptBlock => ({ kind, text, call_id, name: null, edit: null });
const record = (id: string, blocks: TranscriptBlock[], role: string | null = 'assistant'): TranscriptRecord => ({ id, byte_offset: 0, byte_length: 20, raw_json: 'PRIVATE RAW RECORD', kind: 'response_item', message_id: null, issue: null, presentation: { role, timestamp: null, blocks } });
const cursor: TranscriptCursor = { session_id: 'synthetic', source_id: 'source', generation: 'g', offset: 100, record_start: 100, partial: false };
const page = (records: TranscriptRecord[], next_cursor: TranscriptCursor | null = null): TranscriptPage => ({ provider: 'codex', availability: 'available', issues: [], records, next_cursor, source_complete: !next_cursor });
function doc(html: string) { return new DOMParser().parseFromString(html, 'text/html'); }

describe('sanitized offline transcript export', () => {
  it('matches the shared synthetic redaction policy used before dataset persistence', () => {
    for (const { input, phrases, text, count } of redactionPolicy.vectors) expect(redactTranscriptText(input, phrases)).toEqual({ text, count });
  });
  it('defaults to escaped conversation only and never includes raw, tool, reasoning, or unknown content', async () => {
    const read = vi.fn().mockResolvedValue(page([record('one', [block('text', '<img src="https://example.invalid/leak" onerror="alert(1)">Hello & bye'), block('tool_call', 'private command'), block('tool_result', 'private output'), block('reasoning', 'private reasoning'), block('source_block', 'binary attachment')]), record('reasoning', [block('text', 'unclassified reasoning')], null)]));
    const preview = await previewTranscriptExport('synthetic', options, read);
    const output = doc(preview.html);
    expect(preview.complete).toBe(true);
    expect(output.querySelectorAll('img,script,object,iframe,form')).toHaveLength(0);
    expect(output.querySelector('pre')?.textContent).toContain('<img');
    for (const secret of ['PRIVATE RAW', 'private command', 'private output', 'private reasoning', 'binary attachment', 'unclassified reasoning', 'https://example.invalid']) expect(preview.html).not.toContain(secret);
    expect(output.querySelector('meta[http-equiv]')?.getAttribute('content')).toContain("default-src 'none'");
    expect(read).toHaveBeenCalledWith(expect.objectContaining({ session_id: 'synthetic', max_records: 25, max_bytes: 131_072 }));
  });

  it('redacts synthetic secret material, OS paths, attachment encodings, URLs, and exact review phrases', () => {
    const input = ['api_key="fake secret with spaces"', 'Bearer synthetic-bearer', 'sk-proj-syntheticABCDEFG', '-----BEGIN PRIVATE KEY-----\nsynthetic key\n-----END PRIVATE KEY-----', 'C:\\Users\\Synthetic\\secret.txt', '/home/synthetic/private.txt', 'https://example.invalid/token?secret=private', '{"data":"binary bytes"}', 'a'.repeat(200), 'private project name'].join('\n');
    const output = redactTranscriptText(input, ['private project name']);
    for (const value of ['fake secret', 'synthetic-bearer', 'syntheticABCDEFG', 'synthetic key', 'Synthetic', '/home', 'example.invalid', 'binary bytes', 'a'.repeat(200), 'private project name']) expect(output.text).not.toContain(value);
    expect(output.count).toBeGreaterThanOrEqual(10);
  });

  it('links opted-in tool pairs across pages without exposing original identifiers', async () => {
    const read = vi.fn().mockResolvedValueOnce(page([record('first', [block('tool_call', 'safe command', 'private-call-id')])], cursor)).mockResolvedValueOnce(page([record('second', [block('tool_result', 'safe result', 'private-call-id')])]));
    const result = await previewTranscriptExport('synthetic', { ...options, toolCalls: true, toolResults: true }, read);
    const output = doc(result.html);
    expect(output.body.textContent).toContain('safe result');
    expect(output.body.textContent).toContain('safe command');
    expect(result.html).not.toContain('private-call-id');
    const links = [...output.querySelectorAll<HTMLAnchorElement>('a[href]')];
    expect(links.length).toBeGreaterThan(3);
    for (const link of links) expect(output.getElementById(link.getAttribute('href')!.slice(1))).not.toBeNull();
  });

  it('carries earlier omissions to final-page completeness and handles missing or replaced sources', async () => {
    const first = { ...page([], cursor), issues: ['oversized_record'] };
    const read = vi.fn().mockResolvedValueOnce(first).mockResolvedValueOnce(page([record('last', [block('text', 'Available tail')])]));
    expect((await previewTranscriptExport('synthetic', options, read)).complete).toBe(false);
    for (const availability of ['missing', 'cursor_invalid'] as const) {
      const result = await previewTranscriptExport('synthetic', options, async () => ({ ...page([]), availability, source_complete: false }));
      expect(result.complete).toBe(false);
      expect(result.html).toContain('Incomplete source excerpt');
    }
  });

  it('bounds reads, encoded output, repeated cursors, and cancels late results', async () => {
    let n = 0;
    const result = await previewTranscriptExport('synthetic', options, async () => page([], { ...cursor, offset: ++n }));
    expect(n).toBe(40); expect(result.limited).toBe(true); expect(result.complete).toBe(false);
    const repeated = vi.fn().mockResolvedValue(page([], cursor));
    expect((await previewTranscriptExport('synthetic', options, repeated)).limited).toBe(true);
    expect(repeated).toHaveBeenCalledTimes(2);
    const oversized = await previewTranscriptExport('synthetic', options, async () => page([record('huge', [block('text', '<'.repeat(1_100_000))])]));
    expect(oversized.limited).toBe(true); expect(oversized.html.length).toBeLessThan(8 * 1024 * 1024);
    let cancel = false;
    await expect(previewTranscriptExport('synthetic', options, async () => { cancel = true; return page([]); }, () => cancel)).rejects.toThrow('cancelled');
    await expect(previewTranscriptExport('synthetic', { ...options, redactPhrases: ['x'.repeat(257)] }, repeated)).rejects.toThrow('256');
  });
});
