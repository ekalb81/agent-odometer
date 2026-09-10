import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sessionsStore } from '../lib/stores/sessions.svelte';
import { sessionDetailPaneStore } from '../lib/stores/sessionDetailPane.svelte';
import { defaultFilters } from '../lib/sessionProjection';
import { rates } from '../lib/stores/rates';
import type { RateCard, RangeTotals, Session, SessionSummary, TokenTotals } from '../lib/types';
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
      getSessionPricing: vi.fn().mockResolvedValue({}),
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


function testRateCard(): RateCard {
  return {
    version: 1, currency: 'credits', unit: 'per_1m_tokens', source_url: '', fetched_at: null,
    models: { synthetic: { input: 10, cached_input: 1, cache_creation_input: 0, output: 20, reasoning: 20 } },
    fallback_model: 'synthetic', currencies: { codex: 'credits' }, fallback_models: {}, api_models: {},
    unpriced_models: [], pricing_catalog: { rate_periods: [], conditional_modifiers: [], notes: [] }, model_aliases: {},
    free_local_models: [], subscription_plans: {}, display_currency: null,
    refresh: { last_success_at: null, last_attempt_at: null, last_failure_reason: null, max_cache_age_secs: 86400 },
  };
}

describe('SessionsView range pricing refresh orchestration', () => {
  const ids = ['codex:thread:alpha', 'codex:thread:beta'];
  beforeEach(() => {
    localStorage.clear();
    stubLayoutApis();
    rates.set(testRateCard());
    sessionsStore.replaceAll(ids.map((id) => summary(id, id)));
    ipcMocks.getSessionPricing.mockReset().mockResolvedValue({});
    ipcMocks.writeExport.mockClear();
    ipcMocks.sessionsInRanges.mockReset();
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => Promise.resolve(ranges.map(() => ({}))));
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    rates.set(null);
    sessionsStore.replaceAll([]);
    vi.restoreAllMocks();
  });
  function mountRangeView() {
    return render(SessionsView, { props: {
      harness: 'codex', active: true,
      filters: { ...defaultFilters(), dateFrom: '2026-07-31T00:00', dateTo: '2026-08-02T00:00' },
      onfilterschange: () => {},
    } });
  }
  async function expectBothBatches(count: number) {
    await waitFor(() => expect(ipcMocks.sessionsInRanges).toHaveBeenCalledTimes(count));
    const calls = ipcMocks.sessionsInRanges.mock.calls as unknown as [unknown[], string[]][];
    expect(calls.slice(-2).map(([ranges]) => ranges.length).sort((a, b) => a - b)[0]).toBe(1);
    expect(calls.slice(-2).some(([ranges]) => ranges.length > 1)).toBe(true);
    return calls;
  }
  it('refetches both batched consumers on same-version rate replacement and preserves delta refreshes', async () => {
    mountRangeView();
    await expectBothBatches(2);
    rates.set(testRateCard());
    const replaced = await expectBothBatches(4);
    expect(replaced.slice(-2).every(([, fetchedIds]) => fetchedIds.join() === ids.join())).toBe(true);
    sessionsStore.applyMutations([summary(ids[0], 'Updated title')], []);
    const delta = await expectBothBatches(6);
    expect(delta.slice(-2).every(([, fetchedIds]) => fetchedIds.join() === ids[0])).toBe(true);
  });
  it('retains raw totals but hides stale backend prices while rate refresh is pending', async () => {
    const tokens = { ...zeroTokens, input_tokens: 1_000_000, total_tokens: 1_000_000 };
    const base = summary(ids[0], ids[0]);
    const totals: RangeTotals = {
      tokens, buckets: [{ model: 'synthetic', service_tier: null, tokens }],
      tool_metrics: base.tool_metrics, tool_metrics_by_model: {}, optimization_findings_count: 0,
      pricing: { plan: { total: 99999, by_model: [], missing_models: [], unpriced_models: [] }, api: null },
    };
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => Promise.resolve(ranges.map(() => ({ [ids[0]]: totals }))));
    const view = mountRangeView();
    await expectBothBatches(2);
    const costCell = () => screen.getByRole('button', { name: `Select session ${ids[0]}` }).querySelector('.text-accent-cost');
    await waitFor(() => expect(costCell()).toHaveTextContent('99,999.00'));
    const pending: ((value: Record<string, RangeTotals>[]) => void)[] = [];
    ipcMocks.sessionsInRanges.mockImplementation(() => new Promise((resolve) => pending.push(resolve)));
    const updated = testRateCard();
    updated.models.synthetic.input = 20;
    rates.set(updated);
    await expectBothBatches(4);
    expect(costCell()).toHaveTextContent('unavailable');
    expect(view.container.textContent).not.toContain('99,999');
    view.unmount();
    for (const resolve of pending) resolve([{}]);
  });
  it('uses one cumulative batch for all-time prices and never substitutes event-window totals', async () => {
    ipcMocks.getSessionPricing.mockResolvedValue(Object.fromEntries(ids.map((id) => [id, {
      pricing: { plan: { total: 42.5, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
    }])));
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    const row = await screen.findByRole('button', { name: `Select session ${ids[0]}` });
    await waitFor(() => expect(row.querySelector('.text-accent-cost')).toHaveTextContent('42.50'));
    expect(ipcMocks.getSessionPricing).toHaveBeenCalledExactlyOnceWith(ids);
    // Only analytics requests events. The all-time table reads cumulative pricing.
    expect(ipcMocks.sessionsInRanges.mock.calls.every(([ranges]) => ranges.length > 1)).toBe(true);
    ipcMocks.getSessionPricing.mockClear();
    await fireEvent.click(screen.getAllByText('Export JSON').find((element) => !(element as HTMLButtonElement).disabled)!);
    await waitFor(() => expect(ipcMocks.writeExport).toHaveBeenCalledTimes(1));
    expect(ipcMocks.getSessionPricing).toHaveBeenCalledExactlyOnceWith(ids);
    const rows = JSON.parse(ipcMocks.writeExport.mock.calls[0][2]);
    expect(rows.map((entry: { codex_credits: number }) => entry.codex_credits)).toEqual([42.5, 42.5]);
  });
  it('rejects an export completed under a superseded saved rate card', async () => {
    const summaries = Object.fromEntries(ids.map((id) => [id, {
      pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
    }]));
    ipcMocks.getSessionPricing.mockResolvedValue(summaries);
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    await waitFor(() => expect(ipcMocks.getSessionPricing).toHaveBeenCalledTimes(1));
    let finish!: (value: typeof summaries) => void;
    ipcMocks.getSessionPricing.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await fireEvent.click(screen.getAllByText('Export JSON').find((element) => !(element as HTMLButtonElement).disabled)!);
    await waitFor(() => expect(finish).toBeDefined());
    rates.set(testRateCard());
    finish(summaries);
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Rates changed during export'));
    expect(ipcMocks.writeExport).not.toHaveBeenCalled();
  });
  it('rejects an export when an exported session changes while pricing is pending', async () => {
    const summaries = Object.fromEntries(ids.map((id) => [id, {
      pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
    }]));
    ipcMocks.getSessionPricing.mockResolvedValue(summaries);
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    await waitFor(() => expect(ipcMocks.getSessionPricing).toHaveBeenCalledTimes(1));
    let finish!: (value: typeof summaries) => void;
    ipcMocks.getSessionPricing.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await fireEvent.click(screen.getAllByText('Export JSON').find((element) => !(element as HTMLButtonElement).disabled)!);
    await waitFor(() => expect(finish).toBeDefined());
    sessionsStore.applyMutations([{ ...summary(ids[0], ids[0]), tokens_total: { ...zeroTokens, input_tokens: 100, total_tokens: 100 } }], []);
    finish(summaries);
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Sessions changed during export'));
    expect(ipcMocks.writeExport).not.toHaveBeenCalled();
  });
  it('hides previous all-time prices after the latest incremental pricing fetch fails', async () => {
    ipcMocks.getSessionPricing.mockResolvedValue(Object.fromEntries(ids.map((id) => [id, {
      pricing: { plan: { total: 42.5, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
    }])));
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    const row = await screen.findByRole('button', { name: `Select session ${ids[0]}` });
    await waitFor(() => expect(row.querySelector('.text-accent-cost')).toHaveTextContent('42.50'));
    ipcMocks.getSessionPricing.mockRejectedValueOnce(new Error('latest pricing failed'));
    sessionsStore.applyMutations([{ ...summary(ids[0], ids[0]), tokens_total: { ...zeroTokens, input_tokens: 100, total_tokens: 100 } }], []);
    await waitFor(() => expect(ipcMocks.getSessionPricing).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(row.querySelector('.text-accent-cost')).toHaveTextContent('unavailable'));
    expect(errors).toHaveBeenCalledWith('sessions_in_ranges failed:', expect.any(Error));
  });
  it('refuses an export whose backend pricing is missing', async () => {
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    await screen.findByRole('button', { name: `Select session ${ids[0]}` });
    await fireEvent.click(screen.getAllByText('Export JSON').find((element) => !(element as HTMLButtonElement).disabled)!);
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Pricing is unavailable'));
    expect(ipcMocks.writeExport).not.toHaveBeenCalled();
  });
  it.each(['resolve', 'reject'] as const)('discards obsolete %s after a rate replacement', async (outcome) => {
    const pending: { resolve: (value: Record<string, RangeTotals>[]) => void; reject: (reason: Error) => void; ranges: unknown[] }[] = [];
    ipcMocks.sessionsInRanges.mockImplementationOnce((ranges: unknown[]) => new Promise((resolve, reject) => pending.push({ resolve, reject, ranges })));
    ipcMocks.sessionsInRanges.mockImplementationOnce((ranges: unknown[]) => new Promise((resolve, reject) => pending.push({ resolve, reject, ranges })));
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    mountRangeView();
    await expectBothBatches(2);
    rates.set(testRateCard());
    // Let the rate effect advance epochs while both old requests are pending.
    await new Promise((resolve) => setTimeout(resolve, 30));
    for (const request of pending) {
      if (outcome === 'reject') request.reject(new Error('obsolete request'));
      else request.resolve(request.ranges.map(() => ({})));
    }
    await expectBothBatches(4);
    expect(errors).not.toHaveBeenCalled();
    sessionsStore.applyMutations([summary(ids[1], 'Updated title')], []);
    const delta = await expectBothBatches(6);
    expect(delta.slice(-2).every(([, fetchedIds]) => fetchedIds.join() === ids[1])).toBe(true);
  });
});
