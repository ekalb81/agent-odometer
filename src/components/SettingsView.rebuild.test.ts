import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import SettingsView from './SettingsView.svelte';

// "Rebuild history" re-parses every archived session and then VACUUMs the
// archive. Its ONLY guard is the `window.confirm` in
// `confirmAndStartRebuild` — `commands::rebuild_history` deliberately does
// not confirm again (see the comment above `rebuildStatus` in
// SettingsView.svelte). So the assertion that matters is the negative one:
// declining must not reach the backend. A regression that fired the command
// regardless would rewrite the user's whole archive with no prompt, and
// nothing else in the suite would catch it.
//
// SettingsView is a large integration point with no existing test file, so
// every ipc.ts export is mocked generically (same approach as
// SessionsView.detailPane.test.ts) and only the handful this flow touches
// get real behaviour.
const { ipcMocks, rebuildHistory, getHistoryRebuildStatus } = vi.hoisted(() => {
  // `theme.svelte.ts` calls `window.matchMedia` at module-evaluation time, so
  // this has to land before the import graph is walked — a `beforeEach` stub
  // runs far too late. jsdom does not implement it.
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

  const rebuildHistory = vi.fn().mockResolvedValue(undefined);
  const getHistoryRebuildStatus = vi.fn().mockResolvedValue({
    phase: 'idle',
    processed: 0,
    total: 0,
    evidence: null,
    error: null,
  });
  return {
    rebuildHistory,
    getHistoryRebuildStatus,
    ipcMocks: {
      rebuildHistory,
      getHistoryRebuildStatus,
      cancelHistoryRebuild: vi.fn().mockResolvedValue(undefined),
      onHistoryRebuildProgress: vi.fn().mockResolvedValue(() => {}),
    },
  };
});

vi.mock('../lib/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../lib/ipc')>();
  const mocked: Record<string, unknown> = { ...actual, ...ipcMocks };
  for (const [key, value] of Object.entries(actual)) {
    if (key in ipcMocks || typeof value !== 'function') continue;
    mocked[key] = key.startsWith('on')
      ? vi.fn().mockResolvedValue(() => {})
      : vi.fn().mockResolvedValue(undefined);
  }
  return mocked;
});

vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('0.0.0-test') }));

async function clickRebuild() {
  const button = await screen.findByRole('button', { name: /Rebuild history/i });
  await userEvent.click(button);
  return button;
}

describe('SettingsView — history rebuild confirmation', () => {
  beforeEach(() => {
    rebuildHistory.mockClear();
    getHistoryRebuildStatus.mockClear();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('does not touch the archive when the confirmation is declined', async () => {
    const confirm = vi.fn().mockReturnValue(false);
    vi.stubGlobal('confirm', confirm);

    render(SettingsView, { props: {} });
    await clickRebuild();

    expect(confirm).toHaveBeenCalledTimes(1);
    expect(rebuildHistory).not.toHaveBeenCalled();
  });

  it('starts the rebuild only after the confirmation is accepted', async () => {
    vi.stubGlobal('confirm', vi.fn().mockReturnValue(true));

    render(SettingsView, { props: {} });
    await clickRebuild();

    await waitFor(() => expect(rebuildHistory).toHaveBeenCalledTimes(1));
  });

  it('warns that the whole archive is rewritten and that interrupting is safe', async () => {
    const confirm = vi.fn().mockReturnValue(false);
    vi.stubGlobal('confirm', confirm);

    render(SettingsView, { props: {} });
    await clickRebuild();

    // The prompt is the user's only chance to understand the cost, so pin the
    // claims it makes rather than merely that some string was passed.
    const message = String(confirm.mock.calls[0]?.[0] ?? '');
    expect(message).toMatch(/re-parsed/i);
    expect(message).toMatch(/several minutes/i);
    expect(message).toMatch(/safe to interrupt/i);
  });
});
