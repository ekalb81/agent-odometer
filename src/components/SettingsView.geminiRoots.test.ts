import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import SettingsView from './SettingsView.svelte';

// Issue #40: Gemini CLI's roots were the one provider's sources with no
// Settings control — they came from a hardcoded default and could not be
// moved, added to, or turned off.
//
// Gemini has no legacy flat field on `Config`, so the UI has to write the
// authoritative `providers` map. That makes the assertion that matters a
// structural one: the edit must arrive under `providers.gemini_cli`, and it
// must not flatten away the other providers' entries on the way.
const { setConfig, getTurnReceiptStatus, getHistoryRebuildStatus, configPayload } = vi.hoisted(() => {
  globalThis.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
    dispatchEvent: () => false,
  })) as unknown as typeof globalThis.matchMedia;

  const configPayload = {
    config_version: 1,
    providers: {
      codex: { live_roots: ['/codex/sessions'], archive_roots: [], session_index_path: null },
      claude_code: { live_roots: ['/claude/projects'], archive_roots: [], session_index_path: null },
      gemini_cli: { live_roots: ['/home/u/.gemini/tmp'], archive_roots: [], session_index_path: null },
    },
    session_roots: ['/codex/sessions'],
    archive_roots: [],
    session_index_path: '/codex/history.jsonl',
    claude_session_roots: ['/claude/projects'],
    defender_exclusion_receipt: null,
    performance_tracking_enabled: false,
    performance_log_max_mb: 64,
    memory_heap_tracking_enabled: false,
    instructions_enabled: false,
    instructions_tab_visible: true,
    instruction_roots: [],
    turn_receipts_enabled: false,
    turn_receipts_codex: false,
    turn_receipts_claude: false,
  };

  // `rebuildStatus.phase` is read in a `$derived`, so this one call needs a
  // real shape rather than the generic `undefined` the loop below supplies.
  const getHistoryRebuildStatus = vi.fn().mockResolvedValue({
    phase: 'idle',
    done: 0,
    total: 0,
    elapsed_ms: null,
    error: null,
  });
  return {
    setConfig: vi.fn().mockResolvedValue(undefined),
    getTurnReceiptStatus: vi.fn().mockResolvedValue(undefined),
    getHistoryRebuildStatus,
    configPayload,
  };
});

vi.mock('../lib/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../lib/ipc')>();
  const mocked: Record<string, unknown> = { ...actual, setConfig, getTurnReceiptStatus, getHistoryRebuildStatus };
  for (const [key, value] of Object.entries(actual)) {
    if (key === 'setConfig' || key === 'getTurnReceiptStatus' || key === 'getHistoryRebuildStatus' || typeof value !== 'function') {
      continue;
    }
    mocked[key] = key.startsWith('on')
      ? vi.fn().mockResolvedValue(() => {})
      : vi.fn().mockResolvedValue(undefined);
  }
  return mocked;
});

vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0-test') }));

vi.mock('../lib/stores/config', () => {
  const store = {
    set: vi.fn(),
    subscribe(run: (value: typeof configPayload) => void) {
      run(configPayload);
      return () => {};
    },
  };
  return { config: store };
});

async function saveRoots() {
  const save = await screen.findByRole('button', { name: 'Save changes' });
  await userEvent.click(save);
}

describe('SettingsView — Gemini CLI roots (issue #40)', () => {
  it('shows Gemini receipt health independently from other harnesses', async () => {
    const status = {
      requested: false, configured: false, receipt_observed: false,
      config_source: 'codex_hooks_json', config_path: '/synthetic/codex/hooks.json',
      diagnostic_code: 'hook_not_requested', detail: 'Off.',
      restart_recommended: false, trust_review_recommended: false,
      last_run_at: null, last_run_success: null, last_receipt: null, last_run_detail: null,
    };
    getTurnReceiptStatus.mockResolvedValue({
      enabled: true, executable_path: '/synthetic/odometer', codex: status, claude_code: status,
      gemini_cli: { ...status, requested: true, configured: true,
        config_source: 'gemini_settings_json', config_path: '/synthetic/gemini/settings.json',
        detail: 'Gemini hook awaits its first receipt.', restart_recommended: true },
    });
    render(SettingsView);
    expect(await screen.findByText('Gemini hook awaits its first receipt.')).toBeInTheDocument();
    expect(screen.getByText('Gemini CLI user settings.json')).toBeInTheDocument();
    expect(screen.getByText('Start a fresh Gemini CLI session to load and verify it.')).toBeInTheDocument();
  });

  it('keeps Gemini receipts opt-in and saves Gemini-only setup', async () => {
    render(SettingsView);
    const gemini = await screen.findByRole('checkbox', { name: /^Gemini CLI$/ });
    expect(gemini).not.toBeChecked();
    expect(gemini).toBeDisabled();
    await userEvent.click(screen.getByRole('checkbox', { name: /Enable turn receipts/ }));
    await userEvent.click(gemini);
    await userEvent.click(screen.getByRole('button', { name: 'Save setup' }));
    await waitFor(() => expect(setConfig).toHaveBeenCalled());
    expect(setConfig.mock.calls.at(-1)?.[0]).toMatchObject({
      turn_receipts_enabled: true,
      turn_receipts_codex: false,
      turn_receipts_claude: false,
      turn_receipts_gemini: true,
    });
  });

  beforeEach(() => {
    setConfig.mockClear().mockResolvedValue(undefined);
    getTurnReceiptStatus.mockReset().mockResolvedValue(undefined);
  });

  it('shows the configured Gemini CLI root', async () => {
    render(SettingsView);
    expect(await screen.findByText('/home/u/.gemini/tmp')).toBeInTheDocument();
  });

  it('saves an added root into the authoritative providers map', async () => {
    render(SettingsView);
    await screen.findByText('/home/u/.gemini/tmp');

    const input = screen.getByPlaceholderText('/absolute/path/to/.gemini/tmp');
    await userEvent.type(input, '/second/gemini/tmp{Enter}');
    await saveRoots();

    await waitFor(() => expect(setConfig).toHaveBeenCalled());
    const payload = setConfig.mock.calls.at(-1)?.[0];
    expect(payload.providers.gemini_cli.live_roots).toEqual([
      '/home/u/.gemini/tmp',
      '/second/gemini/tmp',
    ]);
    // The other providers' entries must survive an edit that only touched
    // Gemini — a whole-map replacement would drop them.
    expect(payload.providers.codex.live_roots).toEqual(['/codex/sessions']);
    expect(payload.providers.claude_code.live_roots).toEqual(['/claude/projects']);
  });

  it('saves an emptied root list rather than silently keeping the default', async () => {
    // Removing every root is how a user turns Gemini CLI off. If the save
    // dropped the empty list, the backend would reseed the default root and
    // the setting would appear not to work.
    render(SettingsView);
    await screen.findByText('/home/u/.gemini/tmp');

    await userEvent.click(screen.getByRole('button', { name: 'Remove /home/u/.gemini/tmp' }));
    await saveRoots();

    await waitFor(() => expect(setConfig).toHaveBeenCalled());
    const payload = setConfig.mock.calls.at(-1)?.[0];
    expect(payload.providers.gemini_cli.live_roots).toEqual([]);
  });
});
