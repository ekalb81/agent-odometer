import { describe, expect, it, vi } from 'vitest';
import type { TranscriptPage, TurnInfo } from './types';
import { retainedHandoff, sourceHandoff, prepareHandoff } from './transcriptHandoff';

const turn = (index: number): TurnInfo => ({ index, turn_id: `private-turn-${index}`, status: 'completed', started_at: '2026-07-29T12:00:00Z', completed_at: '2026-07-29T12:01:00Z', user_message: `Selected prompt ${index}`, last_agent_message: 'password="private-secret"\nC:\\Users\\Private\\repo\n```\n<script>unsafe</script>', tool_metrics: { calls: 3, successes: 2, failures: 1 } } as TurnInfo);
function page(): TranscriptPage {
  return { provider: 'codex', availability: 'available', source_complete: true, next_cursor: null, issues: [], records: [{
    id: 'exact-record', byte_offset: 100, byte_length: 200, raw_json: 'DO_NOT_SERIALIZE_RAW', kind: 'response_item', message_id: 'PRIVATE_MESSAGE', issue: null,
    presentation: { role: 'assistant', timestamp: '2026-07-29T12:01:00Z', blocks: [
      { kind: 'text', text: 'Chosen source answer', call_id: null, name: null, edit: null },
      { kind: 'tool_result', text: 'SECRET_TOOL_OUTPUT', call_id: 'PRIVATE_CALL', name: null, edit: null },
      { kind: 'reasoning', text: 'PRIVATE_REASONING', call_id: null, name: null, edit: null },
    ] },
  }] };
}
describe('explicit handoff selection', () => {
  it('exports only chosen retained fields, redacts them, and contains untrusted markup inside literal content', () => {
    const selection = retainedHandoff({ turns: [turn(1), turn(2)], source_availability: 'missing' });
    const result = prepareHandoff(selection, [selection.records[1].key], 'My next step: PHRASE', ['PHRASE']);
    expect(result.markdown).toContain('original source missing');
    expect(result.markdown).toContain('Recorded turn state: completed. Tool calls: 3; successes: 2; failures: 1');
    expect(result.markdown).toContain('User-written task state');
    expect(result.markdown).not.toContain('Selected prompt');
    for (const secret of ['private-secret', 'C:\\Users', 'PHRASE', 'private-turn-']) expect(result.markdown + result.html).not.toContain(secret);
    expect(result.markdown).toContain('````text');
    const document = new DOMParser().parseFromString(result.html, 'text/html');
    expect(document.querySelector('pre')!.textContent).toBe(result.markdown);
    expect(document.querySelector('script, img, iframe, a')).toBeNull();
    expect(result.redactions).toBe(3);
  });
  it('bounds large retained sessions and refuses empty, stale, duplicate, or overbroad selections', () => {
    const selection = retainedHandoff({ turns: Array.from({ length: 2000 }, (_, i) => turn(i)), source_availability: 'present' });
    expect(selection.records).toHaveLength(100);
    expect(selection.records[0].reference).toContain('1950');
    expect(() => prepareHandoff(selection, [], '', [])).toThrow('1–30');
    expect(() => prepareHandoff(selection, ['foreign-session-record'], '', [])).toThrow('selection changed');
    expect(() => prepareHandoff(selection, [selection.records[0].key, selection.records[0].key], '', [])).toThrow('selection changed');
    expect(() => prepareHandoff(selection, selection.records.slice(0, 31).map((record) => record.key), '', [])).toThrow('1–30');
    expect(() => prepareHandoff(selection, [selection.records[0].key], 'x'.repeat(5001), [])).toThrow('limit');
  });
  it('reads only the selected storage identity and omits tool/reasoning/raw fields by default', async () => {
    const read = vi.fn().mockResolvedValue(page());
    const selection = await sourceHandoff('codex:selected-child', false, read);
    expect(read).toHaveBeenCalledWith(expect.objectContaining({ session_id: 'codex:selected-child' }));
    expect(selection.records).toHaveLength(1);
    expect(selection.records[0].sourceAnchor).toBe('exact-record');
    const result = prepareHandoff(selection, [selection.records[0].key], '', []);
    for (const secret of ['SECRET_TOOL_OUTPUT', 'PRIVATE_REASONING', 'DO_NOT_SERIALIZE_RAW', 'PRIVATE_CALL', 'PRIVATE_MESSAGE', 'exact-record']) expect(result.markdown + result.html).not.toContain(secret);
    expect(result.markdown).toContain('Source record at byte 100');
    expect((await sourceHandoff('codex:selected-child', true, read)).records).toHaveLength(2);
  });
  it('keeps unavailable/truncated/repeated-cursor reads honest and discards cancelled source responses', async () => {
    const missing = await sourceHandoff('only', false, async () => ({ ...page(), availability: 'missing', records: [], source_complete: false }));
    expect(missing.coverage).toContain('Incomplete original source');
    expect(missing.records).toEqual([]);
    const large = page(); large.records[0].presentation!.blocks[0].text = 'x'.repeat(20_000);
    const truncated = await sourceHandoff('only', false, async () => large);
    expect(truncated.records[0].truncated).toBe(true);
    expect(truncated.records[0].text).toHaveLength(16_000);
    const looping = { ...page(), next_cursor: { source_id: 'same' } } as TranscriptPage;
    const read = vi.fn().mockResolvedValue(looping);
    const repeated = await sourceHandoff('only', false, read);
    expect(repeated.coverage).toContain('limit was reached');
    expect(repeated.records).toHaveLength(1);
    expect((await sourceHandoff('only', false, async () => ({ ...page(), availability: 'partial' }))).coverage).toContain('Incomplete original source');
    expect(read).toHaveBeenCalledTimes(2);
    let cancelled = false;
    await expect(sourceHandoff('only', false, async () => { cancelled = true; return page(); }, () => cancelled)).rejects.toThrow('cancelled');
  });
});
