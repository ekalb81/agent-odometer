import { describe, expect, it, vi } from 'vitest';
import {
  collectSessionExportTree,
  sessionExportContent,
  sessionExportFileName,
} from './sessionExport';

const sessions = [
  { id: 'root-session', storage_id: 'codex:thread:root-session', harness: 'codex' as const, parent_thread_id: null },
  { id: 'child-a', storage_id: 'codex:thread:child-a', harness: 'codex' as const, parent_thread_id: 'root-session' },
  { id: 'grandchild', storage_id: 'codex:thread:grandchild', harness: 'codex' as const, parent_thread_id: 'child-a' },
  { id: 'child-b', storage_id: 'codex:thread:child-b', harness: 'codex' as const, parent_thread_id: 'root-session' },
  { id: 'unrelated', storage_id: 'codex:thread:unrelated', harness: 'codex' as const, parent_thread_id: null },
];

describe('single-session export helpers', () => {
  it('returns only the requested session unless descendants are requested', () => {
    expect(collectSessionExportTree(sessions, 'codex:thread:root-session', false).map((session) => session.id))
      .toEqual(['root-session']);
    expect(collectSessionExportTree(sessions, 'codex:thread:root-session', true).map((session) => session.id))
      .toEqual(['root-session', 'child-a', 'grandchild', 'child-b']);
    expect(collectSessionExportTree(sessions, 'missing', true)).toEqual([]);
  });

  it('guards against malformed parent cycles', () => {
    const cycle = [
      { id: 'a', storage_id: 'codex:thread:a', harness: 'codex' as const, parent_thread_id: 'b' },
      { id: 'b', storage_id: 'codex:thread:b', harness: 'codex' as const, parent_thread_id: 'a' },
    ];
    expect(collectSessionExportTree(cycle, 'codex:thread:a', true).map((session) => session.id))
      .toEqual(['a', 'b']);
  });

  it('does not attach descendants when a provider parent id is ambiguous', () => {
    const colliding = [
      { id: 'duplicate', storage_id: 'codex:thread:duplicate:first', harness: 'codex' as const, parent_thread_id: null },
      { id: 'duplicate', storage_id: 'codex:thread:duplicate:second', harness: 'codex' as const, parent_thread_id: null },
      { id: 'child', storage_id: 'codex:thread:child', harness: 'codex' as const, parent_thread_id: 'duplicate' },
    ];

    expect(collectSessionExportTree(colliding, 'codex:thread:duplicate:first', true)
      .map((session) => session.storage_id))
      .toEqual(['codex:thread:duplicate:first']);
    expect(collectSessionExportTree(colliding, 'codex:thread:child', true)
      .map((session) => session.storage_id))
      .toEqual(['codex:thread:child']);
  });

  it('serializes equivalent rows to the selected format', () => {
    const rows = [{ id: 'root', parent_thread_id: null, total_tokens: 12 }];
    const toCsv = vi.fn(() => 'id,parent_thread_id,total_tokens\r\nroot,,12\r\n');

    expect(sessionExportContent(rows, 'json', toCsv)).toContain('"total_tokens": 12');
    expect(toCsv).not.toHaveBeenCalled();
    expect(sessionExportContent(rows, 'csv', toCsv)).toBe('id,parent_thread_id,total_tokens\r\nroot,,12\r\n');
    expect(toCsv).toHaveBeenCalledWith(rows);
  });

  it('builds privacy-safe filenames from the harness and short session id', () => {
    expect(sessionExportFileName('codex', '1234567890abcdef', 0, 'json'))
      .toBe('odometer-codex-12345678.json');
    expect(sessionExportFileName('claude_code', 'abcdef1234567890', 3, 'csv'))
      .toBe('odometer-claude_code-abcdef12-with-3-children.csv');
  });
});
