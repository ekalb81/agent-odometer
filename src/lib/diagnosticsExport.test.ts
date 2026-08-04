import { describe, expect, it } from 'vitest';
import { buildDiagnosticsExport, diagnosticsExportJson } from './diagnosticsExport';
import type { DiagnosticsReport, ProviderDiagnostic } from './types';

function provider(overrides: Partial<ProviderDiagnostic> = {}): ProviderDiagnostic {
  return {
    id: 'codex',
    display_name: 'Codex',
    registered: true,
    state: 'ready',
    reasons: [],
    notices: [{ code: 'healthy', message: 'No issues detected for this provider.' }],
    capabilities: { archived_sources: true, session_index: true },
    roots: [
      { kind: 'live', path: 'C:\\Users\\dev\\.codex\\sessions', exists: true, is_default: true },
      { kind: 'archive', path: 'C:\\Users\\dev\\.codex\\archived_sessions', exists: false, is_default: true },
    ],
    discovery: { discovered_files: 10, parsed_files: 10, skipped_files: 0, parse_failures: 0, cache_hits: 8, cache_misses: 2 },
    ledger: { history_store_available: true, durable_sessions: 10, available_sessions: 10, collision_sessions: 0 },
    pricing: { models_observed: 1, models_priced: 1, unpriced_models_used: [], fallback_models_used: [], fallback_used: false, rates_fetched_at: null, rates_stale: false },
    retention: { level: 'none', supports_archive: true, archive_roots_configured: 1 },
    quota: { status: 'not_available', reason_code: 'quota_source_not_implemented', message: 'Not implemented yet.' },
    ...overrides,
  };
}

function report(overrides: Partial<DiagnosticsReport> = {}): DiagnosticsReport {
  return {
    generated_at: '2026-08-03T00:00:00Z',
    source_configuration_valid: true,
    cache_cold_reason: null,
    last_scan_at: '2026-08-03T00:00:00Z',
    providers: [provider()],
    ...overrides,
  };
}

describe('buildDiagnosticsExport', () => {
  it('redacts every root path by default, keeping kind/exists/is_default', () => {
    const redacted = buildDiagnosticsExport(report());
    const roots = redacted.providers[0].roots;
    expect(roots[0].path).toBe('<live-root-1>');
    expect(roots[1].path).toBe('<archive-root-2>');
    expect(roots[0].exists).toBe(true);
    expect(roots[1].exists).toBe(false);
    expect(roots[0].is_default).toBe(true);
  });

  it('never mutates the original report', () => {
    const original = report();
    const originalPath = original.providers[0].roots[0].path;
    buildDiagnosticsExport(original);
    expect(original.providers[0].roots[0].path).toBe(originalPath);
  });

  it('keeps exact paths only with the explicit local-only opt-in', () => {
    const kept = buildDiagnosticsExport(report(), { includePaths: true });
    expect(kept.providers[0].roots[0].path).toBe('C:\\Users\\dev\\.codex\\sessions');
  });

  it('leaves every non-path field untouched', () => {
    const source = report({
      providers: [
        provider({
          state: 'degraded',
          reasons: [{ code: 'root_missing', message: 'One or more configured roots for this provider do not exist on disk.' }],
        }),
      ],
    });
    const redacted = buildDiagnosticsExport(source);
    expect(redacted.providers[0].state).toBe('degraded');
    expect(redacted.providers[0].reasons).toEqual(source.providers[0].reasons);
    expect(redacted.providers[0].discovery).toEqual(source.providers[0].discovery);
    expect(redacted.providers[0].pricing).toEqual(source.providers[0].pricing);
    expect(redacted.source_configuration_valid).toBe(true);
  });

  it('redacts every provider independently, with per-provider root numbering', () => {
    const multi = report({
      providers: [
        provider(),
        provider({ id: 'claude_code', display_name: 'Claude Code', roots: [{ kind: 'live', path: '/home/dev/.claude/projects', exists: true, is_default: true }] }),
      ],
    });
    const redacted = buildDiagnosticsExport(multi);
    expect(redacted.providers[0].roots[0].path).toBe('<live-root-1>');
    expect(redacted.providers[1].roots[0].path).toBe('<live-root-1>');
  });
});

describe('diagnosticsExportJson', () => {
  it('produces pretty-printed, redacted JSON', () => {
    const json = diagnosticsExportJson(report());
    expect(json).not.toContain('.codex');
    expect(json).toContain('<live-root-1>');
    const parsed = JSON.parse(json) as DiagnosticsReport;
    expect(parsed.providers[0].id).toBe('codex');
    expect(json).toContain('\n');
  });

  it('honors includePaths for a local-only export', () => {
    const json = diagnosticsExportJson(report(), { includePaths: true });
    expect(json).toContain('.codex\\\\sessions');
  });
});
