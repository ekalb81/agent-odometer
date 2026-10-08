import { fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sessionsStore } from '../lib/stores/sessions.svelte';
import { historyStore } from '../lib/stores/history.svelte';
import { sessionDetailPaneStore } from '../lib/stores/sessionDetailPane.svelte';
import { projectStore } from '../lib/stores/projects.svelte';
import { sessionGridStore } from '../lib/stores/sessionGrid.svelte';
import { defaultFilters } from '../lib/sessionProjection';
import { rates } from '../lib/stores/rates';
import type { RateCard, RangeTotals, Session, SessionSummary, TokenTotals } from '../lib/types';
import SessionsView from './SessionsView.svelte';
import { tick } from 'svelte';

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
      prepareSessionSummaryExport: vi.fn(),
      publishSessionSummaryExport: vi.fn(),
      publishToolDimensionExport: vi.fn(),
      listToolImpactTargets: vi.fn().mockResolvedValue([]),
      getSubscriptionUsage: vi.fn().mockResolvedValue([]),
      getQuotaSnapshots: vi.fn().mockResolvedValue([]),
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
  vi.stubGlobal('innerWidth', 1440);
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

async function exportProjection(format: 'CSV' | 'JSON') {
  const disclosure = screen.getByText('Export projection').closest('details')!;
  if (!disclosure.open) await fireEvent.click(within(disclosure).getByText('Export projection'));
  await fireEvent.click(within(disclosure).getByRole('button', { name: `Export ${format}` }));
}

describe('SessionsView wide-layout detail pane', () => {
  beforeEach(async () => {
    localStorage.clear();
    sessionGridStore.reset();
    ipcMocks.resolveProjects.mockResolvedValue([]);
    await projectStore.refresh();
    getSessionDetails.mockClear();
    sessionDetailPaneStore.setOpen(false);
    sessionDetailPaneStore.setWidth(560);
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
    sessionGridStore.reset();
  });

  it('updates only the assigned row, including a session without a detected project', async () => {
    renderView();
    const alpha = await screen.findByRole('button', { name: 'Select session Fix login bug' });
    const beta = screen.getByRole('button', { name: 'Select session Refactor exporter' });
    ipcMocks.resolveProjects.mockResolvedValue([{ project_key: 'manual:alpha', label: 'Standalone project',
      provenance: 'fallback_path_identity', member_keys: ['manual:alpha'], session_count: 1,
      overridden_session_keys: ['codex:thread:alpha'] }] as never);
    await projectStore.refresh();
    await waitFor(() => expect(alpha).toHaveTextContent('Standalone project'));
    expect(beta).not.toHaveTextContent('Standalone project');
    ipcMocks.resolveProjects.mockResolvedValue([]);
    await projectStore.refresh();
    await waitFor(() => expect(alpha).not.toHaveTextContent('Standalone project'));
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

  it.each(['parent', 'child'])('keeps another project visible when collapsing a reassigned %s', async (assigned) => {
    const parent = { ...summary('codex:thread:parent', 'Parent task'),
      project_key: 'repo:original', project_label: 'Original project' };
    const child = { ...summary('codex:thread:child', 'Child task'),
      parent_thread_id: parent.id, source: 'subagent',
      project_key: 'repo:original', project_label: 'Original project' };
    sessionsStore.replaceAll([parent, child]);
    ipcMocks.resolveProjects.mockResolvedValue([
      { project_key: 'repo:original', label: 'Original project', provenance: 'repository_root',
        member_keys: ['repo:original'], session_count: 1, overridden_session_keys: [] },
      { project_key: 'manual:separate', label: 'Separate project', provenance: 'fallback_path_identity',
        member_keys: ['manual:separate'], session_count: 1,
        overridden_session_keys: [`codex:thread:${assigned}`] },
    ] as never);
    await projectStore.refresh();
    sessionGridStore.setGroupByRepository(true);
    renderView();
    await screen.findByRole('button', { name: 'Select session Child task' });
    await fireEvent.click(screen.getByRole('button', { name: 'Collapse subagent rows for Parent task' }));
    expect(screen.getByRole('button', { name: 'Select session Child task' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Select session Parent task' })).toBeInTheDocument();

    // Ordinary lineage collapse still applies when project grouping is off.
    await fireEvent.click(screen.getByRole('checkbox', { name: 'Group by repository' }));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Select session Child task' })).not.toBeInTheDocument());
  });

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
    expect(pane).toHaveStyle({ width: '560px' });
    expect(pane.inert).toBe(false);
    expect(await screen.findByRole('button', { name: 'Hide details' })).toHaveAttribute('aria-expanded', 'true');
  });

  it('distinguishes empty, loading, and failed detail states and retries the selected fetch', async () => {
    let rejectDetails!: (error: Error) => void;
    getSessionDetails.mockImplementationOnce(() => new Promise((_, reject) => { rejectDetails = reject; }));
    getSessionDetails.mockResolvedValueOnce(fullSession('codex:thread:alpha', 'Fix login bug'));
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    renderView();
    expect(screen.getByText('Select a session to see its details')).toBeInTheDocument();

    await userEvent.click(await screen.findByRole('button', { name: 'Select session Fix login bug' }));
    expect(await screen.findByText('Loading session details…')).toBeInTheDocument();
    rejectDetails(new Error('synthetic detail failure'));
    expect(await screen.findByText('Could not load session details.')).toBeInTheDocument();
    expect(screen.queryByText('Loading session details…')).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    const pane = document.getElementById('session-detail-pane')!;
    await within(pane).findByText('Fix login bug');
    expect(within(pane).queryByText('Could not load session details.')).not.toBeInTheDocument();
    expect(errors).toHaveBeenCalledWith('get_session_details failed:', expect.any(Error));
    errors.mockRestore();
  });

  it('delivers snapshots during continuous updates without parallel requests or postponed timers', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      let count = 0;
      getSessionDetails.mockImplementation((id: string) => {
        const value = fullSession(id, `Received snapshot ${++count}`);
        return new Promise(resolve => setTimeout(() => resolve(value), 650));
      });
      renderView();
      await tick();
      await fireEvent.click(screen.getByRole('button', { name: 'Select session Fix login bug' }));
      await tick();
      expect(getSessionDetails).toHaveBeenCalledTimes(1);
      for (let update = 0; update < 15; update++) {
        sessionsStore.applyMutations([summary('codex:thread:alpha', 'Fix login bug')], []);
        await tick();
        await vi.advanceTimersByTimeAsync(100);
        if (update === 6) {
          expect(screen.getByText('Received snapshot 1')).toBeInTheDocument();
          expect(screen.getByText(/Updating session details/)).toBeInTheDocument();
        }
      }
      expect(screen.getByText('Received snapshot 2')).toBeInTheDocument();
      expect(getSessionDetails).toHaveBeenCalledTimes(3);
      await vi.advanceTimersByTimeAsync(1500);
      expect(screen.queryByText(/Updating session details/)).not.toBeInTheDocument();
    } finally { vi.useRealTimers(); }
  });

  it.each(['success', 'failure'] as const)('rejects obsolete detail %s after selection or same-version rate replacement', async (outcome) => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    const pending: Array<{ resolve: (s: Session) => void; reject: (e: Error) => void }> = [];
    getSessionDetails.mockImplementation(() => new Promise<Session>((resolve, reject) => pending.push({ resolve, reject })));
    rates.set(testRateCard());
    renderView();
    await fireEvent.click(await screen.findByRole('button', { name: 'Select session Fix login bug' }));
    await waitFor(() => expect(pending).toHaveLength(1));
    rates.set(testRateCard());
    await waitFor(() => expect(pending).toHaveLength(2));
    pending[1].resolve(fullSession('codex:thread:alpha', 'Current snapshot'));
    await screen.findByText('Current snapshot');
    if (outcome === 'success') pending[0].resolve(fullSession('codex:thread:alpha', 'Obsolete snapshot'));
    else pending[0].reject(new Error('obsolete failure'));
    await tick();
    expect(screen.getByText('Current snapshot')).toBeInTheDocument();
    sessionsStore.applyMutations([summary('codex:thread:alpha', 'Fix login bug')], []);
    await waitFor(() => expect(pending).toHaveLength(3));
    await fireEvent.click(screen.getByRole('button', { name: 'Select session Refactor exporter' }));
    await waitFor(() => expect(pending).toHaveLength(4));
    pending[3].resolve(fullSession('codex:thread:beta', 'Selected beta'));
    await screen.findByText('Selected beta');
    if (outcome === 'success') pending[2].resolve(fullSession('codex:thread:alpha', 'Obsolete selection'));
    else pending[2].reject(new Error('obsolete selection failure'));
    await tick();
    expect(screen.getByText('Selected beta')).toBeInTheDocument();
    expect(screen.queryByText('Obsolete selection')).not.toBeInTheDocument();
    expect(screen.queryByText('Obsolete snapshot')).not.toBeInTheDocument();
    expect(errors).not.toHaveBeenCalledWith('get_session_details failed:', expect.any(Error));
    errors.mockRestore();
    rates.set(null);
  });

  it('retains raw details but strips prices immediately when the same-version rate card is replaced', async () => {
    rates.set(testRateCard());
    const value = fullSession('codex:thread:alpha', 'Priced snapshot');
    value.tokens_total = { ...zeroTokens, total_tokens: 12345 };
    value.pricing = { plan: { total: 31, by_model: [], missing_models: [], unpriced_models: [] }, flat_api: null, time_aware_api: null, turn_prices: {} };
    getSessionDetails.mockResolvedValueOnce(value);
    renderView();
    await fireEvent.click(await screen.findByRole('button', { name: 'Select session Fix login bug' }));
    await screen.findByText('Priced snapshot');
    const pane = screen.getByLabelText('Session details');
    expect(pane).toHaveTextContent('31.00');
    getSessionDetails.mockImplementationOnce(() => new Promise(() => {}));
    rates.set(testRateCard());
    await tick();
    expect(pane).toHaveTextContent('12,345');
    expect(pane).not.toHaveTextContent('31.00');
    expect(pane).toHaveTextContent('Pricing unavailable');
    rates.set(null);
  });

  it('keeps prior details visible and offers retry after a refresh failure', async () => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    renderView();
    await userEvent.click(await screen.findByRole('button', { name: 'Select session Fix login bug' }));
    const pane = document.getElementById('session-detail-pane')!;
    await within(pane).findByText('Fix login bug');

    getSessionDetails.mockRejectedValueOnce(new Error('synthetic refresh failure'));
    sessionsStore.applyMutations([summary('codex:thread:alpha', 'Fix login bug')], []);
    await waitFor(() => expect(getSessionDetails).toHaveBeenCalledTimes(2), { timeout: 1500 });
    const alert = await within(pane).findByText('Could not refresh session details. Showing previous details.');
    expect(alert.closest('[role="alert"]')).not.toBeNull();
    expect(within(pane).getByText('Fix login bug')).toBeInTheDocument();

    getSessionDetails.mockResolvedValueOnce(fullSession('codex:thread:alpha', 'Fix login bug'));
    await userEvent.click(within(pane).getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(getSessionDetails).toHaveBeenCalledTimes(3));
    await waitFor(() => expect(within(pane).queryByText('Could not refresh session details. Showing previous details.')).not.toBeInTheDocument());
    expect(errors).toHaveBeenCalledWith('get_session_details failed:', expect.any(Error));
    errors.mockRestore();
  });

  it('keeps the selection and its data when the pane collapses, and restores it on reopen', async () => {
    renderView();
    const row = await screen.findByRole('button', { name: /Select session Fix login bug/ });
    await userEvent.click(row);
    await waitFor(() => expect(getSessionDetails).toHaveBeenCalledTimes(1));

    await userEvent.click(await screen.findByRole('button', { name: 'Close session details' }));

    // Collapsed: no second fetch, but the row is still the selected one.
    expect(sessionDetailPaneStore.open).toBe(false);
    expect(document.getElementById('session-detail-pane')).toHaveStyle({ width: '0px' });
    expect(getSessionDetails).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('button', { name: /Select session Fix login bug/ }).className).toContain('bg-accent-rowbg');

    await userEvent.click(await screen.findByRole('button', { name: 'Show details' }));

    // Reopened: same session reappears without an extra fetch.
    expect(sessionDetailPaneStore.open).toBe(true);
    expect(document.getElementById('session-detail-pane')).toHaveStyle({ width: '560px' });
    expect(getSessionDetails).toHaveBeenCalledTimes(1);
  });

  it('resizes the investigation by keyboard within persisted width bounds', async () => {
    renderView();
    await fireEvent.click(await screen.findByRole('button', { name: 'Select session Fix login bug' }));
    const resize = screen.getByRole('separator', { name: 'Resize session details' });
    await fireEvent.keyDown(resize, { key: 'ArrowLeft' });
    expect(resize).toHaveAttribute('aria-valuenow', '580');
    await fireEvent.keyDown(resize, { key: 'ArrowRight' });
    expect(resize).toHaveAttribute('aria-valuenow', '560');
    await fireEvent.keyDown(resize, { key: 'Home' });
    expect(resize).toHaveAttribute('aria-valuenow', '410');
    await fireEvent.keyDown(resize, { key: 'End' });
    expect(resize).toHaveAttribute('aria-valuenow', '800');
    expect(localStorage.getItem('sessionDetailPaneWidth.v1')).toBe('800');
  });

  it('keeps projection exports and their privacy choice accessible in both workspace modes', async () => {
    renderView();
    const disclosure = screen.getByText('Export projection').closest('details')!;
    await fireEvent.click(within(disclosure).getByText('Export projection'));
    for (const mode of ['Sessions', 'Analytics']) {
      await fireEvent.click(screen.getByRole('button', { name: mode }));
      expect(within(disclosure).getByRole('button', { name: 'Export CSV' })).toBeVisible();
      expect(within(disclosure).getByRole('button', { name: 'Export JSON' })).toBeVisible();
      expect(within(disclosure).getByRole('checkbox', { name: 'Include working directories' })).not.toBeChecked();
    }
  });

  it('retains selection, sort, filters and analytics disclosures through mode and provider round trips', async () => {
    const props = { harness: 'all' as const, active: true, filters: { ...defaultFilters(), search: 'Fix login' }, onfilterschange: vi.fn() };
    const rendered = render(SessionsView, { props });
    const selected = await screen.findByRole('button', { name: 'Select session Fix login bug' });
    expect(screen.queryByRole('button', { name: 'Select session Refactor exporter' })).not.toBeInTheDocument();
    await fireEvent.click(selected);
    await waitFor(() => expect(getSessionDetails).toHaveBeenCalledTimes(1));
    await fireEvent.click(screen.getByRole('button', { name: /^Name/ }));
    const sortBefore = screen.getByRole('button', { name: /^Name/ }).textContent;
    await fireEvent.click(screen.getByRole('button', { name: 'Analytics' }));
    const model = screen.getByText(/^Model comparison/).closest('details')!;
    await fireEvent.click(within(model).getByText(/^Model comparison/));
    const content = rendered.container.querySelector<HTMLElement>('.analytics-content')!;
    content.scrollTop = 120;
    await rendered.rerender({ ...props, active: false });
    await rendered.rerender(props);
    expect(screen.getByRole('button', { name: 'Analytics' })).toHaveAttribute('aria-pressed', 'true');
    expect(model.open).toBe(true);
    expect(content.scrollTop).toBe(120);
    await fireEvent.click(screen.getByRole('button', { name: 'Sessions' }));
    expect(screen.getByRole('button', { name: /^Name/ }).textContent).toBe(sortBefore);
    expect(screen.getByRole('button', { name: 'Select session Fix login bug' }).className).toContain('bg-accent-rowbg');
    expect(screen.queryByRole('button', { name: 'Select session Refactor exporter' })).not.toBeInTheDocument();
    await waitFor(() => expect(getSessionDetails).toHaveBeenCalledTimes(2));
    await within(screen.getByLabelText('Session details')).findByText('Fix login bug');
    expect(props.onfilterschange).not.toHaveBeenCalled();
  });

  it('keeps each mounted provider grouping and tree mode independent', async () => {
    sessionsStore.replaceAll([summary('codex:thread:alpha', 'Fix login bug'), { ...summary('claude:thread:beta', 'Refactor exporter'), harness: 'claude_code' }]);
    const codexProps = { harness: 'codex' as const, active: true, filters: defaultFilters(), onfilterschange: () => {} };
    const claudeProps = { ...codexProps, harness: 'claude_code' as const, active: false };
    const codex = render(SessionsView, { props: codexProps });
    const claude = render(SessionsView, { props: claudeProps });
    await fireEvent.click(within(codex.container).getByRole('checkbox', { name: 'Group by repository' }));
    await codex.rerender({ ...codexProps, active: false });
    await claude.rerender({ ...claudeProps, active: true });
    expect(within(claude.container).getByRole('checkbox', { name: 'Group by repository' })).not.toBeChecked();
    await fireEvent.click(within(claude.container).getByRole('checkbox', { name: 'Flat list' }));
    await claude.rerender(claudeProps);
    await codex.rerender(codexProps);
    expect(within(codex.container).getByRole('checkbox', { name: 'Group by repository' })).toBeChecked();
    expect(within(codex.container).getByRole('checkbox', { name: 'Flat list' })).not.toBeChecked();
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
    expect(document.getElementById('session-detail-pane')).toHaveStyle({ width: '0px' });
    expect(screen.getByRole('button', { name: 'Show details' })).toBeDisabled();
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
    historyStore.set({ ...historyStore.status, status: 'ready', coverage_complete: true });
    ipcMocks.publishToolDimensionExport.mockReset().mockResolvedValue(true);
    ipcMocks.listToolImpactTargets.mockReset().mockResolvedValue([]);
    sessionsStore.replaceAll(ids.map((id) => summary(id, id)));
    ipcMocks.getSessionPricing.mockReset().mockImplementation((fetchIds: string[]) => Promise.resolve(Object.fromEntries(fetchIds.map(id => [id, { tokens: zeroTokens, pricing: { plan: { total: 0, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {} }]))));
    ipcMocks.writeExport.mockClear();
    ipcMocks.publishSessionSummaryExport.mockReset().mockResolvedValue(true);
    ipcMocks.prepareSessionSummaryExport.mockReset().mockImplementation((request: unknown) => Promise.resolve({ request, as_of: '2026-10-04T00:00:00Z', digest: 'synthetic', content: '[]', session_count: 2 }));
    ipcMocks.sessionsInRanges.mockReset();
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => Promise.resolve(ranges.map(() => ({}))));
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    rates.set(null);
    sessionsStore.replaceAll([]);
    vi.restoreAllMocks();
  });
  function mountRangeView(harness: 'codex' | 'all' = 'codex', search = '') {
    return render(SessionsView, { props: {
      harness, active: true,
      filters: { ...defaultFilters(), search, dateFrom: '2026-07-31T00:00', dateTo: '2026-08-02T00:00' },
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
  it.each(['provider', 'filter'] as const)('retains usable proofs while revalidating sustained updates outside the %s scope', async (scope) => {
    const base = summary(ids[0], ids[0]);
    const total: RangeTotals = { tokens: { ...zeroTokens, total_tokens: 321 }, buckets: [], tool_metrics: base.tool_metrics,
      tool_metrics_by_model: {}, optimization_findings_count: 0,
      pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null } };
    const pending: (() => void)[] = [];
    ipcMocks.sessionsInRanges.mockImplementationOnce((ranges: unknown[]) => new Promise(resolve => {
      pending.push(() => resolve(ranges.map(() => ({ [ids[0]]: total }))));
    }));
    ipcMocks.sessionsInRanges.mockImplementationOnce((ranges: unknown[]) => new Promise(resolve => {
      pending.push(() => resolve(ranges.map(() => ({ [ids[0]]: total }))));
    }));
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => Promise.resolve(ranges.map(() => ({ [ids[0]]: total }))));
    mountRangeView('codex', scope === 'filter' ? ids[0] : '');
    await expectBothBatches(2);
    const row = screen.getByRole('button', { name: `Select session ${ids[0]}` });
    for (let update = 0; update < 4; update++) {
      sessionsStore.applyMutations([{ ...summary('other', 'Other session'), harness: scope === 'provider' ? 'claude_code' : 'codex' }], []);
      await new Promise(resolve => setTimeout(resolve, 30));
    }
    pending.forEach(resolve => resolve());
    await waitFor(() => expect(row).toHaveTextContent('321'));
    expect(screen.getByText(/^Tokens ·/).parentElement).toHaveTextContent('321');
    for (let update = 0; update < 4; update++) {
      sessionsStore.applyMutations([{ ...summary('other', 'Other session'), harness: scope === 'provider' ? 'claude_code' : 'codex' }], []);
      await new Promise(resolve => setTimeout(resolve, 30));
      expect(row).toHaveTextContent('321');
      expect(screen.getByText(/^Tokens ·/).parentElement).toHaveTextContent('321');
    }
    await waitFor(() => expect(screen.queryByTestId('accounting-table-status')).not.toBeInTheDocument());
    expect(ipcMocks.sessionsInRanges.mock.calls.length).toBeGreaterThan(2);
    const calls = ipcMocks.sessionsInRanges.mock.calls as unknown as [unknown[], string[], string[]][];
    expect(calls.slice(2).every(([, fetchedIds]) => fetchedIds.length === 0)).toBe(true);
    expect(calls.every(([, , proofScope]) => proofScope.join() === (scope === 'filter' ? ids[0] : ids.join()))).toBe(true);
  });
  it.each(['startup', 'verified'] as const)('shows explicitly previous verified snapshots during a sustained 1100-session All scope stream with 350ms requests from %s', async (phase) => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      const corpus = Array.from({ length: 1100 }, (_, index) => summary(`synthetic:${index}`, `Session ${index}`));
      sessionsStore.replaceAll(corpus);
      const total: RangeTotals = { tokens: { ...zeroTokens, total_tokens: 321 }, buckets: [], tool_metrics: corpus[0].tool_metrics,
        tool_metrics_by_model: {}, optimization_findings_count: 0,
        pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null } };
      ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => phase === 'startup'
        ? new Promise(resolve => setTimeout(() => resolve(ranges.map(() => ({ [corpus[0].id]: total }))), 350))
        : Promise.resolve(ranges.map(() => ({ [corpus[0].id]: total }))));
      mountRangeView('all');
      await vi.advanceTimersByTimeAsync(0);
      const row = screen.getByRole('button', { name: 'Select session Session 0' });
      expect(row).toHaveTextContent(phase === 'startup' ? 'unavailable' : '321');
      expect(ipcMocks.sessionsInRanges).toHaveBeenCalledTimes(2);
      let currentTokens = 321;
      ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => {
        const captured = { ...total, tokens: { ...zeroTokens, total_tokens: currentTokens } };
        return new Promise(resolve => setTimeout(() => resolve(ranges.map(() => ({ [corpus[0].id]: captured }))), 350));
      });
      for (let update = 0; update < 10; update++) {
        currentTokens = 1000 + update;
        sessionsStore.applyMutations([{ ...corpus[0], tokens_total: { ...zeroTokens, total_tokens: currentTokens } }], []);
        await vi.advanceTimersByTimeAsync(100);
        if (update === 2) expect(ipcMocks.sessionsInRanges).toHaveBeenCalledTimes(phase === 'startup' ? 2 : 4);
        if (phase === 'startup' && update < 3) expect(row).toHaveTextContent('unavailable');
        else {
          expect(row).toHaveTextContent(/(?:321|1,00\d)/);
          expect(screen.getByTestId('accounting-table-status')).toHaveTextContent('Previous verified usage · refreshing');
        }
      }
      expect(ipcMocks.sessionsInRanges.mock.calls.length).toBeGreaterThanOrEqual(6);
      expect(row).not.toHaveTextContent('321');
      await vi.advanceTimersByTimeAsync(2000);
      expect(row).toHaveTextContent('1,009');
      expect(screen.queryByTestId('accounting-table-status')).not.toBeInTheDocument();
      expect(screen.getByText(/^Tokens ·/).parentElement).toHaveTextContent('1.0K');
    } finally {
      vi.useRealTimers();
    }
  });
  it('verifies current All scope totals between continuous 100ms updates when requests take 80ms', async () => {
    vi.useFakeTimers({ toFake: ['setTimeout', 'clearTimeout'] });
    try {
      const corpus = Array.from({ length: 1100 }, (_, index) => summary(`synthetic:${index}`, `Session ${index}`));
      sessionsStore.replaceAll(corpus);
      let currentTokens = 1000;
      ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => {
        const total: RangeTotals = { tokens: { ...zeroTokens, total_tokens: currentTokens }, buckets: [], tool_metrics: corpus[0].tool_metrics,
          tool_metrics_by_model: {}, optimization_findings_count: 0,
          pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null } };
        return new Promise(resolve => setTimeout(() => resolve(ranges.map(() => ({ [corpus[0].id]: total }))), 80));
      });
      mountRangeView('all');
      await vi.advanceTimersByTimeAsync(80);
      const row = screen.getByRole('button', { name: 'Select session Session 0' });
      expect(row).toHaveTextContent('1,000');
      let verifiedDuringStream = 0;
      for (let update = 1; update <= 10; update++) {
        currentTokens = 1000 + update;
        sessionsStore.applyMutations([{ ...corpus[0], tokens_total: { ...zeroTokens, total_tokens: currentTokens } }], []);
        await vi.advanceTimersByTimeAsync(99);
        if (!screen.queryByTestId('accounting-table-status')) {
          verifiedDuringStream++;
          expect(row).toHaveTextContent(new Intl.NumberFormat().format(currentTokens));
          expect(screen.getByText(/^Tokens ·/).parentElement).toHaveTextContent('1.0K');
        }
        await vi.advanceTimersByTimeAsync(1);
      }
      expect(verifiedDuringStream).toBeGreaterThan(0);
    } finally {
      vi.useRealTimers();
    }
  });
  it.each(['scope', 'date', 'history'] as const)('rejects earlier successes and failures after a hard %s change', async (change) => {
    const base = summary(ids[0], ids[0]);
    const total = (value: number): RangeTotals => ({ tokens: { ...zeroTokens, total_tokens: value }, buckets: [], tool_metrics: base.tool_metrics,
      tool_metrics_by_model: {}, optimization_findings_count: 0,
      pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null } });
    const pending: { resolve: (value: Record<string, RangeTotals>[]) => void; reject: (error: Error) => void; ranges: unknown[] }[] = [];
    const delayed = (ranges: unknown[]) => new Promise<Record<string, RangeTotals>[]>((resolve, reject) => pending.push({ resolve, reject, ranges }));
    ipcMocks.sessionsInRanges.mockImplementationOnce(delayed).mockImplementationOnce(delayed);
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[], fetchIds: string[] = []) => Promise.resolve(ranges.map(() => Object.fromEntries(fetchIds.map(id => [id, total(123)])))));
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    const view = mountRangeView();
    await expectBothBatches(2);
    if (change === 'history') historyStore.set({ ...historyStore.status, status: 'ready' });
    else await view.rerender({ harness: 'codex', active: true,
      filters: { ...defaultFilters(), search: change === 'scope' ? ids[0] : '', dateFrom: change === 'date' ? '2026-07-30T00:00' : '2026-07-31T00:00', dateTo: '2026-08-02T00:00' },
      onfilterschange: () => {},
    });
    await new Promise(resolve => setTimeout(resolve, 20));
    pending[0].resolve(pending[0].ranges.map(() => ({ [ids[0]]: total(987654) })));
    pending[1].reject(new Error('obsolete proof failed'));
    await expectBothBatches(4);
    const row = screen.getByRole('button', { name: `Select session ${ids[0]}` });
    await waitFor(() => expect(row).toHaveTextContent('123'));
    expect(row).not.toHaveTextContent('987,654');
    expect(errors).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByTestId('accounting-table-status')).not.toBeInTheDocument());
  });
  it('labels previously verified table usage while analytics is pending or fails independently', async () => {
    const pending: { resolve: (value: Record<string, RangeTotals>[]) => void; reject: (error: Error) => void; ranges: unknown[] }[] = [];
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => new Promise((resolve, reject) => pending.push({ resolve, reject, ranges })));
    const base = summary(ids[0], ids[0]);
    const total: RangeTotals = { tokens: { ...zeroTokens, total_tokens: 901 }, buckets: [], tool_metrics: base.tool_metrics,
      tool_metrics_by_model: {}, optimization_findings_count: 0,
      pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null } };
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    const view = mountRangeView();
    await expectBothBatches(2);
    sessionsStore.applyMutations([base], []);
    const table = pending.find(request => request.ranges.length === 1)!;
    const analytics = pending.find(request => request.ranges.length > 1)!;
    table.resolve([{ [ids[0]]: total }]);
    const row = screen.getByRole('button', { name: `Select session ${ids[0]}` });
    await waitFor(() => expect(row).toHaveTextContent('901'));
    expect(screen.getByTestId('accounting-table-status')).toHaveTextContent('Previous verified usage · refreshing');
    analytics.resolve(analytics.ranges.map(() => ({ [ids[0]]: total })));
    await expectBothBatches(4);
    const latestAnalytics = pending.slice(2).find(request => request.ranges.length > 1)!;
    latestAnalytics.reject(new Error('current analytics proof failed'));
    await waitFor(() => expect(screen.getByTestId('accounting-table-status')).toHaveTextContent('Analytics:'));
    expect(row).toHaveTextContent('901');
    expect(screen.getByTestId('accounting-table-status')).toHaveTextContent('Previous verified usage · refreshing');
    expect(errors).toHaveBeenCalled();
    view.unmount();
    pending.slice(2).find(request => request.ranges.length === 1)!.resolve([{}]);
  });
  it.each(['known-zero', 'unpriced-zero'] as const)('keeps chart and purchased-credit zero states honest: %s', async (mode) => {
    const card = testRateCard();
    card.api_models = card.models;
    rates.set(card);
    const base = summary(ids[0], ids[0]);
    sessionsStore.replaceAll([base]);
    const price = { total: 0, by_model: [], missing_models: [], unpriced_models: mode === 'unpriced-zero' ? ['synthetic'] : [] };
    const total: RangeTotals = { tokens: { ...zeroTokens, total_tokens: 100 }, buckets: [], tool_metrics: base.tool_metrics, tool_metrics_by_model: {}, optimization_findings_count: 0, pricing: { plan: price, api: price } };
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => Promise.resolve(ranges.map(() => ({ [ids[0]]: total }))));
    mountRangeView();
    await waitFor(() => expect(screen.getByText(/^Purchased-credit estimate/).parentElement).toHaveTextContent(mode === 'unpriced-zero' ? 'Unavailable' : '0.00'));
    const data = screen.getByText('Chart data · exact bucket intervals').closest('details')!;
    await fireEvent.click(within(data).getByText('Chart data · exact bucket intervals'));
    await waitFor(() => expect(data.querySelectorAll('tbody tr').length).toBeGreaterThan(1));
    const rows = data.querySelectorAll('tbody tr');
    expect(rows.length).toBeGreaterThan(1);
    for (const row of rows) expect(row.querySelectorAll('td')[2]).toHaveTextContent(mode === 'unpriced-zero' ? 'unavailable' : '$0.00');
    expect(screen.getByText(/^Max /)).toHaveTextContent(mode === 'unpriced-zero' ? 'Max unavailable' : 'Max $0.00');
    const plot = data.parentElement!.querySelector('svg')!;
    expect(plot.querySelector('polyline') !== null).toBe(mode === 'known-zero');
  });

  it.each(['unsupported', 'partial', 'fallback', 'purchased-primary', 'both-unsupported'] as const)('qualifies purchased estimates independently from API availability: %s', async (mode) => {
    const card = testRateCard();
    if (mode !== 'purchased-primary') card.api_models = card.models;
    card.currencies.claude_code = 'USD';
    rates.set(card);
    const tokens = { ...zeroTokens, input_tokens: 100, total_tokens: 100 };
    const base = { ...summary(ids[0], ids[0]), tokens_total: tokens };
    sessionsStore.replaceAll([base]);
    const unavailable = mode === 'unsupported' || mode === 'purchased-primary' || mode === 'both-unsupported';
    const purchased = { total: unavailable ? 0 : 20, by_model: [], missing_models: mode === 'fallback' ? ['synthetic'] : [], unpriced_models: mode === 'fallback' ? [] : ['synthetic'] };
    const api = { total: mode === 'both-unsupported' ? 0 : 17, by_model: [], missing_models: [], unpriced_models: mode === 'both-unsupported' ? ['synthetic'] : [] };
    const totals: RangeTotals = { tokens, buckets: [], tool_metrics: base.tool_metrics, tool_metrics_by_model: {}, optimization_findings_count: 0,
      pricing: { plan: api, api, current: { as_of: '2026-10-04T00:00:00Z', purchased_credits: purchased, included_allowance: api, api_estimate: api } } };
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => Promise.resolve(ranges.map(() => ({ [ids[0]]: totals }))));
    mountRangeView(mode === 'purchased-primary' ? 'codex' : 'all');
    await waitFor(() => {
      const label = screen.getAllByText(/^Purchased-credit estimate/).find(el => el.parentElement?.textContent?.includes(mode === 'fallback' ? 'fallback rate' : 'unpriced model'))!;
      expect(label).toBeDefined();
      expect(label.parentElement).toHaveTextContent(unavailable ? 'Unavailable' : '20.00');
      expect(label.parentElement).toHaveTextContent(mode === 'fallback' ? '1 fallback rate used' : '1 unpriced model excluded');
    });
    if (mode === 'purchased-primary') expect(screen.getAllByText('Unavailable').length).toBeGreaterThanOrEqual(2);
    else {
      await fireEvent.click(screen.getByRole('button', { name: 'Analytics' }));
      expect(screen.getByText('Codex purchased-credit estimate').parentElement).toHaveTextContent(unavailable ? 'Unavailable' : '20.00');
      expect(screen.getByText('Codex purchased-credit estimate').parentElement).toHaveTextContent(mode === 'fallback' ? '1 fallback rate used' : '1 unpriced model excluded');
      expect(screen.getByText('Codex API base USD').parentElement).toHaveTextContent(mode === 'both-unsupported' ? 'Unavailable' : '$17.00');
    }
  });

  it('refetches both batched consumers on same-version rate replacement and preserves delta refreshes', async () => {
    mountRangeView();
    await expectBothBatches(2);
    rates.set(testRateCard());
    const replaced = await expectBothBatches(4);
    expect(replaced.slice(-2).every(([, fetchedIds]) => fetchedIds.join() === ids.join())).toBe(true);
    sessionsStore.applyMutations([summary(ids[0], 'Updated title')], []);
    const delta = await expectBothBatches(6);
    expect(delta.slice(-2).every(([, fetchedIds]) => fetchedIds.join() === ids[0])).toBe(true);
    expect(delta.slice(-2).every(call => (call as unknown as [unknown, string[], string[]])[2].join() === ids.join())).toBe(true);
  });
  it.each(['history', 'retry'] as const)('refetches all same-ID table and analytics values after %s invalidates the proof', async (source) => {
    const base = summary(ids[0], ids[0]);
    const total = (value: number): RangeTotals => ({ tokens: { ...zeroTokens, total_tokens: value }, buckets: [], tool_metrics: base.tool_metrics, tool_metrics_by_model: {}, optimization_findings_count: 0,
      pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null } });
    let value = 123;
    ipcMocks.sessionsInRanges.mockImplementation((bounds: unknown[], fetchIds: string[] = []) => Promise.resolve(bounds.map(() => Object.fromEntries(fetchIds.map(id => [id, total(value)])))));
    mountRangeView();
    await expectBothBatches(2);
    const row = screen.getByRole('button', { name: `Select session ${ids[0]}` });
    await waitFor(() => expect(row).toHaveTextContent('123'));
    const mutation = sessionsStore.mutationLog.generation;
    value = 456;
    if (source === 'history') historyStore.set({ ...historyStore.status, status: 'ready' });
    else {
      await fireEvent.click(screen.getByRole('button', { name: 'Analytics' }));
      await fireEvent.click(screen.getByText(/^Task categories/));
      await fireEvent.click(screen.getByRole('button', { name: 'Retry categories' }));
    }
    const calls = await expectBothBatches(4);
    expect(calls.slice(-2).every(([, fetchedIds]) => fetchedIds.join() === ids.join())).toBe(true);
    expect(sessionsStore.mutationLog.generation).toBe(mutation);
    await waitFor(() => expect(row).toHaveTextContent('456'));
    expect(row).not.toHaveTextContent('123');
  });

  it.each(['none', 'to-only'] as const)('exports dimensions with the captured first analytics range under %s filter bounds', async (choice) => {
    const base = summary(ids[0], ids[0]);
    const total: RangeTotals = { tokens: { ...zeroTokens, total_tokens: 321 }, buckets: [], tool_metrics: base.tool_metrics, tool_metrics_by_model: {}, optimization_findings_count: 0,
      tool_dimensions: { context_source: { conversation_cache: { calls: 0, failures: 0, output_bytes: 0, duration_ms: 0, tokens: 321 } } },
      pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null } };
    ipcMocks.sessionsInRanges.mockImplementation((bounds: unknown[], fetchIds: string[] = []) => Promise.resolve(bounds.map(() => fetchIds.includes(ids[0]) ? { [ids[0]]: total } : {})));
    render(SessionsView, { props: { harness: 'codex', active: true, filters: { ...defaultFilters(), dateTo: choice === 'to-only' ? '2026-08-02T00:00' : '' }, onfilterschange: () => {} } });
    await userEvent.click(screen.getByRole('button', { name: 'Analytics' }));
    await userEvent.click(screen.getByRole('button', { name: 'Tools & context' }));
    await waitFor(() => expect(screen.queryByText('Tool, MCP, shell & context attribution', { exact: false })).toBeTruthy());
    const attribution = screen.getByText(/Tool, MCP, shell & context attribution/).closest('details')!;
    await userEvent.click(within(attribution).getByText(/Tool, MCP, shell & context attribution/));
    await fireEvent.click(within(attribution).getByRole('button', { name: 'Export CSV' }));
    const request = ipcMocks.publishToolDimensionExport.mock.calls[0][0];
    expect(request.session_ids).toEqual(ids);
    expect(request.from).toBe('2026-08-01T00:00:00.000Z');
    expect(request.to).not.toBeNull();
    const captured = ipcMocks.sessionsInRanges.mock.calls.filter(([bounds]) => bounds.length > 1).map(([bounds]) => (bounds as { from: string; to: string }[])[0]);
    expect(captured).toContainEqual({ from: request.from, to: request.to });
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
      tokens: { ...zeroTokens, total_tokens: 777 }, pricing: { plan: { total: 42.5, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
    }])));
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    const row = await screen.findByRole('button', { name: `Select session ${ids[0]}` });
    await waitFor(() => expect(row.querySelector('.text-accent-cost')).toHaveTextContent('42.50'));
    expect(ipcMocks.getSessionPricing).toHaveBeenCalledExactlyOnceWith(ids, ids);
    // Only analytics requests events. The all-time table reads cumulative pricing.
    expect(ipcMocks.sessionsInRanges.mock.calls.every(([ranges]) => ranges.length > 1)).toBe(true);
    ipcMocks.getSessionPricing.mockClear();
    ipcMocks.prepareSessionSummaryExport.mockImplementation(request => Promise.resolve({ request, as_of: '2026-10-04T00:00:00Z', digest: 'synthetic', session_count: 2, content: JSON.stringify([{ codex_credits: 42.5, total_tokens: 777 }, { codex_credits: 42.5, total_tokens: 777 }]) }));
    await exportProjection('JSON');
    await waitFor(() => expect(ipcMocks.publishSessionSummaryExport).toHaveBeenCalledTimes(1));
    expect(ipcMocks.getSessionPricing).not.toHaveBeenCalled();
    expect(ipcMocks.prepareSessionSummaryExport).toHaveBeenCalledExactlyOnceWith({ session_ids: ids, from: null, to: null, format: 'json', include_working_directory: false });
    const rows = JSON.parse(ipcMocks.publishSessionSummaryExport.mock.calls[0][0].content);
    expect(rows.map((entry: { codex_credits: number }) => entry.codex_credits)).toEqual([42.5, 42.5]);
    expect(rows.map((entry: { total_tokens: number }) => entry.total_tokens)).toEqual([777, 777]);
  });
  it('rejects an export completed under a superseded saved rate card', async () => {
    const summaries = Object.fromEntries(ids.map((id) => [id, {
      tokens: zeroTokens, pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
    }]));
    ipcMocks.getSessionPricing.mockResolvedValue(summaries);
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    await waitFor(() => expect(ipcMocks.getSessionPricing).toHaveBeenCalledTimes(1));
    let finish!: (value: typeof summaries) => void;
    ipcMocks.prepareSessionSummaryExport.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await exportProjection('JSON');
    await waitFor(() => expect(finish).toBeDefined());
    rates.set(testRateCard());
    finish({ request: {}, as_of: '2026-10-04T00:00:00Z', digest: 'synthetic', content: '[]', session_count: 2 } as unknown as typeof summaries);
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Rates changed during export'));
    expect(ipcMocks.writeExport).not.toHaveBeenCalled();
    expect(ipcMocks.publishSessionSummaryExport).not.toHaveBeenCalled();
  });
  it('rejects an export when an exported session changes while pricing is pending', async () => {
    const summaries = Object.fromEntries(ids.map((id) => [id, {
      tokens: zeroTokens, pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
    }]));
    ipcMocks.getSessionPricing.mockResolvedValue(summaries);
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    await waitFor(() => expect(ipcMocks.getSessionPricing).toHaveBeenCalledTimes(1));
    let finish!: (value: typeof summaries) => void;
    ipcMocks.prepareSessionSummaryExport.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    await exportProjection('JSON');
    await waitFor(() => expect(finish).toBeDefined());
    sessionsStore.applyMutations([{ ...summary(ids[0], ids[0]), tokens_total: { ...zeroTokens, input_tokens: 100, total_tokens: 100 } }], []);
    finish({ request: {}, as_of: '2026-10-04T00:00:00Z', digest: 'synthetic', content: '[]', session_count: 2 } as unknown as typeof summaries);
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('Sessions changed during export'));
    expect(ipcMocks.writeExport).not.toHaveBeenCalled();
    expect(ipcMocks.publishSessionSummaryExport).not.toHaveBeenCalled();
  });
  it('hides previous all-time prices after the latest incremental pricing fetch fails', async () => {
    ipcMocks.getSessionPricing.mockResolvedValue(Object.fromEntries(ids.map((id) => [id, {
      tokens: { ...zeroTokens, total_tokens: 777 }, pricing: { plan: { total: 42.5, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
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

  it('keeps a valid recent date window available when an older cumulative period is ambiguous', async () => {
    ipcMocks.getSessionPricing.mockRejectedValue('accounting_identity_ambiguous');
    const tokens = { ...zeroTokens, total_tokens: 901 };
    const total = { tokens, buckets: [], tool_metrics: zeroTokens, tool_metrics_by_model: {}, optimization_findings_count: 0,
      pricing: { plan: { total: 15, by_model: [], missing_models: [], unpriced_models: [] }, api: null } } as unknown as RangeTotals;
    ipcMocks.sessionsInRanges.mockImplementation((ranges: unknown[]) => Promise.resolve(ranges.map(() => ({ [ids[0]]: total }))));
    mountRangeView();
    const row = await screen.findByRole('button', { name: `Select session ${ids[0]}` });
    await waitFor(() => expect(row).toHaveTextContent('901'));
    expect(row.querySelector('.text-accent-cost')).toHaveTextContent('15.00');
    await waitFor(() => expect(screen.queryByTestId('accounting-table-status')).not.toBeInTheDocument());
  });
  it('withholds raw footer and row totals after a cached sibling identity failure', async () => {
    const snapshot = (fetchIds: string[]) => Object.fromEntries(fetchIds.map(id => [id, {
      tokens: { ...zeroTokens, input_tokens: 901, total_tokens: 901 },
      pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {},
    }]));
    ipcMocks.getSessionPricing.mockImplementation((fetchIds: string[]) => Promise.resolve(snapshot(fetchIds)));
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    const row = await screen.findByRole('button', { name: `Select session ${ids[0]}` });
    await waitFor(() => expect(row).toHaveTextContent('901'));
    ipcMocks.getSessionPricing.mockRejectedValueOnce('accounting_identity_ambiguous');
    sessionsStore.applyMutations([summary(ids[0], ids[0])], []);
    await screen.findByText(/ambiguous accounting identities/);
    expect(row).not.toHaveTextContent('901');
    expect(row).toHaveTextContent('unavailable');
    expect(ipcMocks.getSessionPricing.mock.calls.at(-1)).toEqual([[ids[0]], ids]);
    expect(screen.getByTestId('accounting-table-status')).toHaveTextContent('ambiguous');
    expect(errors).toHaveBeenCalled();
  });
  it('labels a same-scope in-flight cumulative snapshot as previously verified until the latest mutation is priced', async () => {
    let finish!: (value: unknown) => void;
    let finishLatest!: (value: unknown) => void;
    ipcMocks.getSessionPricing.mockImplementationOnce(() => new Promise(resolve => { finish = resolve; }));
    ipcMocks.getSessionPricing.mockImplementationOnce(() => new Promise(resolve => { finishLatest = resolve; }));
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    await waitFor(() => expect(finish).toBeDefined());
    sessionsStore.applyMutations([summary(ids[0], ids[0])], []);
    finish(Object.fromEntries(ids.map(id => [id, { tokens: { ...zeroTokens, total_tokens: 998877 }, pricing: { plan: { total: 98989, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {} }])));
    await waitFor(() => expect(ipcMocks.getSessionPricing).toHaveBeenCalledTimes(2));
    const row = screen.getByRole('button', { name: `Select session ${ids[0]}` });
    expect(row).toHaveTextContent('998,877');
    expect(screen.getByTestId('accounting-table-status')).toHaveTextContent('Previous verified usage · refreshing');
    expect(ipcMocks.getSessionPricing.mock.calls.at(-1)).toEqual([[ids[0]], ids]);
    finishLatest({ [ids[0]]: { tokens: { ...zeroTokens, total_tokens: 901 }, pricing: { plan: { total: 12, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {} } });
    await waitFor(() => expect(row).toHaveTextContent('901'));
    await waitFor(() => expect(screen.queryByTestId('accounting-table-status')).not.toBeInTheDocument());
  });
  it('refuses an export whose backend cumulative snapshot is missing', async () => {
    ipcMocks.prepareSessionSummaryExport.mockRejectedValue('accounting_identity_unverified');
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    await screen.findByRole('button', { name: `Select session ${ids[0]}` });
    await exportProjection('JSON');
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('accounting_identity_unverified'));
    expect(ipcMocks.writeExport).not.toHaveBeenCalled();
    expect(ipcMocks.publishSessionSummaryExport).not.toHaveBeenCalled();
  });
  it('shows a rejected post-picker identity proof without falling back to the generic writer', async () => {
    ipcMocks.publishSessionSummaryExport.mockRejectedValueOnce('accounting_identity_ambiguous');
    render(SessionsView, { props: { harness: 'codex', active: true, filters: defaultFilters(), onfilterschange: () => {} } });
    await screen.findByRole('button', { name: `Select session ${ids[0]}` });
    await exportProjection('JSON');
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('accounting_identity_ambiguous'));
    expect(ipcMocks.writeExport).not.toHaveBeenCalled();
    expect(ipcMocks.publishSessionSummaryExport).toHaveBeenCalledTimes(1);
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
