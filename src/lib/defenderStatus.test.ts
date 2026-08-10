import { describe, expect, it } from 'vitest';
import {
  configuredDefenderRoots,
  defenderReceiptStatus,
  isWindowsDefenderSurface,
  normalizeDefenderRoot,
} from './defenderStatus';
import type { Config, DefenderExclusionReceipt } from './types';

function config(receipt: DefenderExclusionReceipt | null = null): Config {
  return {
    config_version: 0,
    providers: {},
    session_roots: ['C:\\Users\\dev\\.codex\\sessions'],
    archive_roots: ['C:\\Users\\dev\\.codex\\archived_sessions'],
    session_index_path: 'C:\\Users\\dev\\.codex\\session_index.jsonl',
    claude_session_roots: ['C:\\Users\\dev\\.claude\\projects'],
    defender_exclusion_receipt: receipt,
    performance_tracking_enabled: false,
    performance_log_max_mb: 64,
    memory_heap_tracking_enabled: false,
    instructions_enabled: false,
    instructions_tab_visible: true,
    instruction_roots: [],
    turn_receipts_enabled: false,
    turn_receipts_codex: true,
    turn_receipts_claude: true,
  };
}

function receipt(configuredRoots: string[]): DefenderExclusionReceipt {
  return {
    version: 1,
    configured_roots: configuredRoots,
    verified_roots: configuredRoots,
    verified_at: '2026-07-29T12:00:00Z',
  };
}

describe('Defender receipt status', () => {
  it('normalizes Windows case, separators, trailing slashes, and duplicates', () => {
    const current = config();
    current.archive_roots = [
      'c:/users/dev/.CODEX/sessions/',
      'C:\\Users\\dev\\.codex\\archived_sessions\\',
    ];

    expect(normalizeDefenderRoot('C:/DATA/')).toBe('c:\\data');
    expect(configuredDefenderRoots(current)).toEqual([
      'c:\\users\\dev\\.claude\\projects',
      'c:\\users\\dev\\.codex\\archived_sessions',
      'c:\\users\\dev\\.codex\\sessions',
    ]);
  });

  it('distinguishes never-requested, current, partial, stale, and unknown-version receipts', () => {
    const current = config();
    expect(defenderReceiptStatus(current)).toBe('never_requested');

    current.defender_exclusion_receipt = receipt([
      'c:/users/dev/.claude/projects',
      'C:\\Users\\dev\\.codex\\archived_sessions\\',
      'c:\\users\\DEV\\.codex\\sessions',
    ]);
    expect(defenderReceiptStatus(current)).toBe('current');

    current.defender_exclusion_receipt.verified_roots = [
      'c:\\users\\DEV\\.codex\\sessions',
    ];
    expect(defenderReceiptStatus(current)).toBe('partial');

    current.session_roots = ['C:\\other\\sessions'];
    expect(defenderReceiptStatus(current)).toBe('stale');

    current.defender_exclusion_receipt.version = 99;
    expect(defenderReceiptStatus(current)).toBe('stale');
  });

  it('recognizes real Windows and only documented browser fixtures', () => {
    expect(isWindowsDefenderSurface('Windows WebView', null)).toBe(true);
    expect(isWindowsDefenderSurface('Linux', 'defender-slow')).toBe(true);
    expect(isWindowsDefenderSurface('Linux', 'defender-error')).toBe(true);
    expect(isWindowsDefenderSurface('Linux', 'default')).toBe(false);
  });
});
