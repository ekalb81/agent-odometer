import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sessionsStore } from '../lib/stores/sessions.svelte';
import { sessionDetailPaneStore } from '../lib/stores/sessionDetailPane.svelte';
import { defaultFilters } from '../lib/sessionProjection';
import type { Session, SessionSummary, TokenTotals } from '../lib/types';
import SessionsView from './SessionsView.svelte';

// SessionsView is a large integration point (grid + analytics band + the
// wide-layout detail pane it composes), so this file mocks every ipc.ts
// export generically — most are never exercised because the components that
// would call them are all gated behind an `active` prop that stays false
// here (the analytics disclosure defaults closed) — and gives the handful
// the pane collapse behavior actually touches real, deterministic
// implementations.
const zeroTokens: TokenTotals = {
  input_tokens: 0,
  cached_input_tokens: 0,
  cache_creation_input_tokens: 0,
  output_tokens: 0,
  reasoning_output_tokens: 0,
  total_tokens: 0,
};

function summary(id: string, name: string): SessionSummary {
  return {
    id,
    storage_id: id,
    harness: 'codex',
    thread_name: name,
    forked_from_id: null,
    parent_thread_id: null,
    agent_path: null,
    agent_nickname: null,
    file_path: `${id}.jsonl`,
    source_availability: 'present',
    archived: false,
    started_at: '2026-08-01T00:00:00Z',
    last_event_at: '2026-08-01T01:00:00Z',
    working_directory: null,
    originator: null,
    source: null,
    cli_version: null,
    model_provider: null,
    model: null,
    service_tier: null,
    plan_type: null,
    credits_unlimited: null,
    credits_balance: null,
    context_window: null,
    total_turns: 0,
    first_user_message: null,
    tokens_total: zeroTokens,
    buckets: [],
    tool_metrics: {
      calls: 0, reads: 0, searches: 0, mutations: 0, commands: 0, other: 0,
      successes: 0, failures: 0, unknown: 0, mutation_targets: 0,
      one_shot_mutations: 0, retry_count: 0, duration_ms: 0, output_bytes: 0,
    },
    tool_metrics_by_model: {},
    category_totals: {},
    optimization_findings_count: 0,
    project_key: null,
    project_label: null,
    project_provenance: null,
  };
}

function fullSession(id: string, name: string): Session {
  const base = summary(id, name);
  return {
    ...base,
    subagent_id_is_path_fallback: false,
    history_mode: null,
    memory_mode: null,
    latest_context_tokens: null,
    tokens_by_model: {},
    tokens_history: [],
    rate_limits_history: [],
    turns: [],
    tool_observations: [],
    optimization_findings: [],
  };
}

const { ipcMocks, getSessionDetails } = vi.hoisted(() => {
  const getSessionDetails = vi.fn();
  return {
    getSessionDetails,
    ipcMocks: {
      getSessionDetails,
      sessionsInRanges: vi.fn((ranges: unknown[]) => Promise.resolve(ranges.map(() => ({})))),
      listExternalEvents: vi.fn().mockResolvedValue([]),
      resolveProjects: vi.fn().mockResolvedValue([]),
      listProviders: vi.fn().mockResolvedValue([]),
      writeExport: vi.fn().mockResolvedValue(undefined),
      onConfigEvent: vi.fn().mockResolvedValue(() => {}),
    },
  };
});

vi.mock('../lib/ipc', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../lib/ipc')>();
  const mocked: Record<string, unknown> = { ...actual, ...ipcMocks };
  for (const [key, value] of Object.entries(actual)) {
    if (key in ipcMocks || typeof value !== 'function') continue;
    // Every other export: a harmless async no-op. Every ipc call SessionsView
    // and its always-mounted children (ConfigTimeline, GitOutcomes, etc.)
    // might reach at mount time is gated on an `active` prop that is false
    // for everything but the grid itself in this test, so these are never
    // actually invoked — they exist only so the import doesn't throw.
    mocked[key] = key.startsWith('on')
      ? vi.fn().mockResolvedValue(() => {})
      : vi.fn().mockResolvedValue(undefined);
  }
  return mocked;
});

function stubLayoutApis(): void {
  // jsdom has neither. `isWide` and the virtual list's viewport height both
  // read from these once at setup and then follow their listeners, so a
  // fixed "always wide, fixed height" stub is enough for this file's needs.
  vi.stubGlobal('matchMedia', vi.fn().mockImplementation((query: string) => ({
    matches: true,
    media: query,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })));
  vi.stubGlobal('ResizeObserver', class {
    observe() {}
    unobserve() {}
    disconnect() {}
  });
}

describe('SessionsView wide-layout detail pane', () => {
  beforeEach(() => {
    localStorage.clear();
    getSessionDetails.mockClear();
    sessionDetailPaneStore.setOpen(false);
    sessionsStore.replaceAll([
      summary('codex:thread:alpha', 'Fix login bug'),
      summary('codex:thread:beta', 'Refactor exporter'),
    ]);
    getSessionDetails.mockImplementation((id: string) =>
      Promise.resolve(fullSession(id, id.endsWith('alpha') ? 'Fix login bug' : 'Refactor exporter')));
    stubLayoutApis();
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    sessionsStore.replaceAll([]);
  });

  function renderView() {
    return render(SessionsView, {
      props: {
        harness: 'all',
        active: true,
        filters: defaultFilters(),
        onfilterschange: () => {},
      },
    });
  }

  it('starts closed, reserving no width for the placeholder pane', async () => {
    renderView();
    await screen.findByRole('button', { name: /Select session Fix login bug/ });

    expect(sessionDetailPaneStore.open).toBe(false);
    const pane = document.getElementById('session-detail-pane');
    expect(pane).not.toBeNull();
    expect(pane).toHaveStyle({ width: '0px' });
    // Width 0 with overflow-hidden only clips visually: without `inert` the
    // pane's controls stay in the tab order and the accessibility tree, so
    // the toggle's aria-expanded="false" would be lying about content a
    // keyboard or screen-reader user can still reach.
    expect((pane as HTMLElement).inert).toBe(true);
  });

  it('opens the pane when a session is selected', async () => {
    renderView();
    const row = await screen.findByRole('button', { name: /Select session Fix login bug/ });

    await userEvent.click(row);

    await waitFor(() => expect(getSessionDetails).toHaveBeenCalledWith('codex:thread:alpha'));
    expect(sessionDetailPaneStore.open).toBe(true);
    const pane = document.getElementById('session-detail-pane') as HTMLElement;
    expect(pane).toHaveStyle({ width: '410px' });
    expect(pane.inert).toBe(false);
    expect(await screen.findByRole('button', { name: 'Hide details' })).toHaveAttribute('aria-expanded', 'true');
  });

  it('keeps the selection and its data when the pane collapses, and restores it on reopen', async () => {
    renderView();
    const row = await screen.findByRole('button', { name: /Select session Fix login bug/ });
    await userEvent.click(row);
    await waitFor(() => expect(getSessionDetails).toHaveBeenCalledTimes(1));

    await userEvent.click(await screen.findByRole('button', { name: 'Hide details' }));

    // Collapsed: no second fetch, but the row is still the selected one.
    expect(sessionDetailPaneStore.open).toBe(false);
    expect(document.getElementById('session-detail-pane')).toHaveStyle({ width: '0px' });
    expect(getSessionDetails).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: /Select session Fix login bug/ }).className).toContain('bg-accent-rowbg');

    await userEvent.click(await screen.findByRole('button', { name: 'Show details' }));

    // Reopened: same session reappears without an extra fetch.
    expect(sessionDetailPaneStore.open).toBe(true);
    expect(document.getElementById('session-detail-pane')).toHaveStyle({ width: '410px' });
    expect(getSessionDetails).toHaveBeenCalledTimes(1);
  });

  it('persists the open state across a remount, the same way sessionGridStore persists column choices', async () => {
    const { unmount } = renderView();
    const row = await screen.findByRole('button', { name: /Select session Fix login bug/ });
    await userEvent.click(row);
    await waitFor(() => expect(sessionDetailPaneStore.open).toBe(true));
    unmount();

    expect(localStorage.getItem('sessionDetailPaneOpen.v1')).toBe('true');

    renderView();
    await screen.findByRole('button', { name: /Select session Fix login bug/ });
    expect(document.getElementById('session-detail-pane')).toHaveStyle({ width: '410px' });
  });
});
