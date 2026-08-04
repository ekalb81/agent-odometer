import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DiagnosticsReport, ProviderDiagnostic } from '../lib/types';
import DiagnosticsPanel from './DiagnosticsPanel.svelte';

const { getProviderDiagnostics, writeExport } = vi.hoisted(() => ({
  getProviderDiagnostics: vi.fn(),
  writeExport: vi.fn(),
}));

vi.mock('../lib/ipc', () => ({ getProviderDiagnostics, writeExport }));

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
      { kind: 'live', path: '/home/dev/.codex/sessions', exists: true, is_default: true },
      { kind: 'archive', path: '/home/dev/.codex/archived_sessions', exists: false, is_default: true },
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

function stubClipboard(writeText: ReturnType<typeof vi.fn>) {
  Object.defineProperty(navigator, 'clipboard', {
    value: { writeText },
    configurable: true,
    writable: true,
  });
}

beforeEach(() => {
  getProviderDiagnostics.mockReset();
  writeExport.mockReset();
  stubClipboard(vi.fn().mockResolvedValue(undefined));
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('DiagnosticsPanel', () => {
  it('runs diagnostics on demand and renders provider health', async () => {
    getProviderDiagnostics.mockResolvedValue(report());
    const user = userEvent.setup();
    render(DiagnosticsPanel);

    expect(getProviderDiagnostics).not.toHaveBeenCalled();
    await user.click(screen.getByRole('button', { name: 'Run diagnostics' }));

    expect(await screen.findByText('Codex')).toBeInTheDocument();
    expect(screen.getByText('Ready')).toBeInTheDocument();
    expect(screen.getByText('Roots: 2 (1 missing)')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Refresh' })).toBeInTheDocument();
  });

  it('surfaces a degraded provider with reasons and an invalid-config banner', async () => {
    getProviderDiagnostics.mockResolvedValue(
      report({
        source_configuration_valid: false,
        providers: [
          provider({
            id: 'claude_code',
            display_name: 'Claude Code',
            state: 'degraded',
            reasons: [
              { code: 'root_missing', message: 'One or more configured roots for this provider do not exist on disk.' },
            ],
          }),
        ],
      }),
    );
    render(DiagnosticsPanel);
    await userEvent.click(screen.getByRole('button', { name: 'Run diagnostics' }));

    expect(await screen.findByText('Degraded')).toBeInTheDocument();
    expect(screen.getByText(/\[root_missing\]/)).toBeInTheDocument();
    expect(screen.getByText(/Session-source configuration is invalid/)).toBeInTheDocument();
  });

  it('shows a fetch error and lets the user retry', async () => {
    getProviderDiagnostics.mockRejectedValueOnce(new Error('backend unavailable'));
    render(DiagnosticsPanel);
    await userEvent.click(screen.getByRole('button', { name: 'Run diagnostics' }));

    expect(await screen.findByText(/backend unavailable/)).toBeInTheDocument();

    getProviderDiagnostics.mockResolvedValueOnce(report());
    await userEvent.click(screen.getByRole('button', { name: 'Run diagnostics' }));
    expect(await screen.findByText('Codex')).toBeInTheDocument();
  });

  it('expands the roots detail with exact local paths', async () => {
    getProviderDiagnostics.mockResolvedValue(report());
    const user = userEvent.setup();
    render(DiagnosticsPanel);
    await user.click(screen.getByRole('button', { name: 'Run diagnostics' }));
    await screen.findByText('Codex');

    await user.click(screen.getByText('Roots'));
    expect(screen.getByText(/\/home\/dev\/\.codex\/sessions/)).toBeInTheDocument();
    expect(screen.getByText(/archived_sessions — missing/)).toBeInTheDocument();
  });

  it('copies redacted JSON by default and exact paths only when opted in', async () => {
    getProviderDiagnostics.mockResolvedValue(report());
    const user = userEvent.setup();
    // user-event's setup() installs its own clipboard stub; ours must be
    // applied after setup() to be the one the component actually calls.
    stubClipboard(vi.fn().mockResolvedValue(undefined));
    render(DiagnosticsPanel);
    await user.click(screen.getByRole('button', { name: 'Run diagnostics' }));
    await screen.findByText('Codex');

    await user.click(screen.getByRole('button', { name: 'Copy JSON' }));
    expect(await screen.findByText('Copied.')).toBeInTheDocument();
    const clipboardWrite = navigator.clipboard.writeText as ReturnType<typeof vi.fn>;
    const redacted = clipboardWrite.mock.calls[0][0] as string;
    expect(redacted).not.toContain('/home/dev/.codex');
    expect(redacted).toContain('<live-root-1>');

    await user.click(screen.getByRole('checkbox', { name: /Include exact paths/ }));
    await user.click(screen.getByRole('button', { name: 'Copy JSON' }));
    await waitFor(() => expect(clipboardWrite).toHaveBeenCalledTimes(2));
    const withPaths = clipboardWrite.mock.calls[1][0] as string;
    expect(withPaths).toContain('/home/dev/.codex/sessions');
  });

  it('reports a clipboard failure', async () => {
    getProviderDiagnostics.mockResolvedValue(report());
    const user = userEvent.setup();
    stubClipboard(vi.fn().mockRejectedValue(new Error('denied')));
    render(DiagnosticsPanel);
    await user.click(screen.getByRole('button', { name: 'Run diagnostics' }));
    await screen.findByText('Codex');

    await user.click(screen.getByRole('button', { name: 'Copy JSON' }));
    expect(await screen.findByText(/Could not copy/)).toBeInTheDocument();
  });

  it('exports via the native save dialog and surfaces a failure', async () => {
    getProviderDiagnostics.mockResolvedValue(report());
    writeExport.mockResolvedValueOnce(true);
    const user = userEvent.setup();
    render(DiagnosticsPanel);
    await user.click(screen.getByRole('button', { name: 'Run diagnostics' }));
    await screen.findByText('Codex');

    await user.click(screen.getByRole('button', { name: 'Export JSON…' }));
    await waitFor(() => expect(writeExport).toHaveBeenCalledWith('odometer-diagnostics', 'json', expect.any(String)));

    writeExport.mockRejectedValueOnce(new Error('dialog cancelled'));
    await user.click(screen.getByRole('button', { name: 'Export JSON…' }));
    expect(await screen.findByText(/dialog cancelled/)).toBeInTheDocument();
  });
});
