import { describe, expect, it, vi } from 'vitest';
import {
  collectSessionExportTree,
  sessionExportContent,
  sessionExportFileName,
} from './sessionExport';

const sessions = [
  { id: 'root-session', parent_thread_id: null },
  { id: 'child-a', parent_thread_id: 'root-session' },
  { id: 'grandchild', parent_thread_id: 'child-a' },
  { id: 'child-b', parent_thread_id: 'root-session' },
  { id: 'unrelated', parent_thread_id: null },
];

describe('single-session export helpers', () => {
  it('returns only the requested session unless descendants are requested', () => {
    expect(collectSessionExportTree(sessions, 'root-session', false).map((session) => session.id))
      .toEqual(['root-session']);
    expect(collectSessionExportTree(sessions, 'root-session', true).map((session) => session.id))
      .toEqual(['root-session', 'child-a', 'grandchild', 'child-b']);
    expect(collectSessionExportTree(sessions, 'missing', true)).toEqual([]);
  });

  it('guards against malformed parent cycles', () => {
    const cycle = [
      { id: 'a', parent_thread_id: 'b' },
      { id: 'b', parent_thread_id: 'a' },
    ];
    expect(collectSessionExportTree(cycle, 'a', true).map((session) => session.id))
      .toEqual(['a', 'b']);
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
