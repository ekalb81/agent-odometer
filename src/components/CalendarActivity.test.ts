import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { sessionsStore } from '../lib/stores/sessions.svelte';
import CalendarActivity from './CalendarActivity.svelte';
import type { HistoryStatus, RangeTotals } from '../lib/types';
import { historyStore } from '../lib/stores/history.svelte';

const mocks = vi.hoisted(() => ({ query: vi.fn(), queryHistory: vi.fn(), publish: vi.fn(), scan: { complete: true } }));
vi.mock('../lib/ipc', () => ({ sessionsInRanges: mocks.query, getHistoryStatus: mocks.queryHistory, publishActivitySummaryExport: mocks.publish }));
vi.mock('../lib/stores/scan.svelte', () => ({ scanStore: { get status() { return mocks.scan; } } }));
const range = { tokens: { total_tokens: 70 }, tool_metrics: { calls: 4 } } as RangeTotals;
function setHistory(status: HistoryStatus['status'], coverage_complete: boolean): void {
  historyStore.set({ status, coverage_complete, step: null, step_index: null, step_total: null, items_done: null, items_total: null, elapsed_ms: null } as HistoryStatus);
}
beforeEach(() => {
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.setAttribute('open', ''); } });
  vi.stubEnv('TZ', 'UTC');
  setHistory('ready', true); mocks.scan = { complete: true };
  mocks.query.mockReset().mockImplementation(async (bounds: unknown[]) => bounds.map(() => ({})));
  mocks.queryHistory.mockReset().mockImplementation(async () => historyStore.status);
  mocks.publish.mockReset().mockResolvedValue(true);
});
afterEach(() => vi.unstubAllEnvs());
const props = { from: '2026-07-28T00:00:00Z', to: '2026-07-29T23:59:59.999Z', sessionIds: ['codex:parent', 'codex:tools'], onbucket: vi.fn() };
describe('calendar activity', () => {
  it('previews only settled measured data and invalidates the snapshot when its scope changes', async () => {
    mocks.query.mockResolvedValue([{ 'codex:parent': range }, {}]);
    const view = render(CalendarActivity, props);
    expect(screen.getByRole('button', { name: 'Preview summary card' })).toBeDisabled();
    await screen.findByTestId('calendar-total');
    await userEvent.click(screen.getByRole('button', { name: 'Preview summary card' }));
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect((screen.getByLabelText('Companion Markdown') as HTMLTextAreaElement).value).toContain('70 recorded tokens');
    await userEvent.click(screen.getByRole('button', { name: 'Save SVG…' }));
    expect(mocks.publish).toHaveBeenCalledWith(expect.objectContaining({
      session_ids: props.sessionIds,
      coverage_complete: true,
      days: [
        { from: '2026-07-28T00:00:00.000Z', to: '2026-07-28T23:59:59.999Z', tokens: 70, tool_calls: 4 },
        { from: '2026-07-29T00:00:00.000Z', to: '2026-07-29T23:59:59.999Z', tokens: 0, tool_calls: 0 },
      ],
    }), 'odometer-activity.svg');
    await view.rerender({ ...props, sessionIds: ['claude:other'] });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    setHistory('unavailable', false);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Preview summary card' })).toBeDisabled());
  });
  it('scopes an effective project independently of the provider and session filter', async () => {
    render(CalendarActivity, { ...props, projects: [{ key: 'merged:work', label: 'Merged work', sessionIds: ['codex:tools'] }] });
    await screen.findByTestId('calendar-total');
    await userEvent.selectOptions(screen.getByLabelText('Activity project'), 'merged:work');
    await waitFor(() => expect(mocks.query.mock.calls.at(-1)?.[1]).toEqual(['codex:tools']));
    expect(screen.getByText(/current provider and session filters · Merged work/)).toBeInTheDocument();
  });
  it('passes the selected scope to Rust and drills into the exact event sessions through the keyboard', async () => {
    mocks.query.mockResolvedValue([{ 'codex:parent': range }, {}]);
    const onbucket = vi.fn();
    render(CalendarActivity, { ...props, onbucket });
    const day = await screen.findByRole('button', { name: '2026-07-28: 70 tokens. Show 1 event sessions.' });
    expect(mocks.query.mock.calls[0][1]).toEqual(props.sessionIds);
    day.focus(); await userEvent.keyboard('{Enter}');
    expect(onbucket).toHaveBeenCalledWith(expect.objectContaining({ tokens: 70, sessionIds: ['codex:parent'] }));
    await userEvent.selectOptions(screen.getByLabelText('Activity metric'), 'tool_calls');
    expect(screen.getByRole('button', { name: '2026-07-28: 4 tool calls. Show 1 event sessions.' })).toBeInTheDocument();
  });
  it('distinguishes intact zero, partial recorded zero, and unavailable coverage', async () => {
    const view = render(CalendarActivity, props);
    expect(await screen.findByText('Zero activity in the recorded history for this range.')).toBeInTheDocument();
    view.unmount();
    setHistory('ready', false);
    const partial = render(CalendarActivity, props);
    expect(await screen.findByText('No activity recorded in this range; missing activity is unknown.')).toBeInTheDocument();
    expect(screen.getByTestId('calendar-total')).toHaveTextContent('partial history');
    partial.unmount();
    setHistory('unavailable', false);
    mocks.query.mockClear();
    render(CalendarActivity, props);
    expect(screen.getByText(/History coverage is unavailable/)).toBeInTheDocument();
    expect(screen.queryByTestId('calendar-total')).not.toBeInTheDocument();
    expect(mocks.query).not.toHaveBeenCalled();
  });
  it('drills into sessions for the selected metric, including tool-only activity', async () => {
    mocks.query.mockResolvedValue([{ 'codex:parent': { tokens: { total_tokens: 70 }, tool_metrics: { calls: 0 } }, 'codex:tools': { tokens: { total_tokens: 0 }, tool_metrics: { calls: 4 } } }, {}]);
    const onbucket = vi.fn();
    render(CalendarActivity, { ...props, onbucket });
    await userEvent.click(await screen.findByRole('button', { name: '2026-07-28: 70 tokens. Show 1 event sessions.' }));
    expect(onbucket).toHaveBeenLastCalledWith(expect.objectContaining({ sessionIds: ['codex:parent'] }));
    await userEvent.selectOptions(screen.getByLabelText('Activity metric'), 'tool_calls');
    await userEvent.click(screen.getByRole('button', { name: 'Trend 2026-07-28: 4 tool calls. Show 1 event sessions.' }));
    expect(onbucket).toHaveBeenLastCalledWith(expect.objectContaining({ sessionIds: ['codex:tools'] }));
  });
  it('does not turn failed or truncated query responses into an empty day', async () => {
    mocks.query.mockRejectedValueOnce('archive read failed');
    render(CalendarActivity, props);
    expect(await screen.findByRole('alert')).toHaveTextContent('complete accounting scope could not be verified');
    expect(screen.queryByTestId('calendar-total')).not.toBeInTheDocument();
    mocks.query.mockResolvedValueOnce([{}]);
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(screen.getByRole('alert')).toHaveTextContent('complete accounting scope could not be verified'));
    mocks.query.mockResolvedValueOnce([{}, {}]);
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByText('Zero activity in the recorded history for this range.')).toBeInTheDocument();
  });
  it('refreshes archive provenance before each query and refuses newly unavailable coverage', async () => {
    const view = render(CalendarActivity, props);
    await screen.findByTestId('calendar-total');
    mocks.queryHistory.mockResolvedValueOnce({ status: 'unavailable', coverage_complete: false });
    mocks.query.mockClear();
    await view.rerender({ ...props, sessionIds: ['codex:tools'] });
    await screen.findByText(/History coverage is unavailable/);
    expect(screen.queryByTestId('calendar-total')).not.toBeInTheDocument();
    expect(mocks.query).not.toHaveBeenCalled();
    expect(mocks.queryHistory).toHaveBeenCalledTimes(2);
  });
  it('discards old scope results and splits layouts into bounded sequential requests', async () => {
    let resolve!: (value: Record<string, RangeTotals>[]) => void;
    mocks.query.mockImplementationOnce(() => new Promise((done) => { resolve = done; }));
    const view = render(CalendarActivity, props);
    await waitFor(() => expect(mocks.query).toHaveBeenCalledTimes(1));
    await view.rerender({ ...props, sessionIds: ['claude:other'] });
    resolve([{ 'codex:parent': range }, {}]);
    await screen.findByText('Zero activity in the recorded history for this range.');
    expect(screen.queryByRole('button', { name: /70 tokens/ })).not.toBeInTheDocument();
    expect(mocks.query.mock.calls.at(-1)?.[1]).toEqual(['claude:other']);
    await view.rerender({ ...props, from: '2026-01-01T00:00:00Z', to: '2026-03-31T23:59:59.999Z' });
    await waitFor(() => expect(screen.getByTestId('calendar-total')).toHaveTextContent('2026-01-01 – 2026-03-31'));
    expect(mocks.query.mock.calls.slice(-2).map((call) => call[0].length)).toEqual([64, 26]);
  });
  it('proves the unchanged cache scope and withholds obsolete activity after identity failure', async () => {
    mocks.query.mockResolvedValue([{ 'codex:parent': range }, {}]);
    render(CalendarActivity, props);
    await screen.findByTestId('calendar-total');
    mocks.query.mockRejectedValueOnce('accounting_identity_ambiguous');
    sessionsStore.applyMutations([], ['irrelevant']);
    await screen.findByRole('alert');
    expect(screen.queryByTestId('calendar-total')).not.toBeInTheDocument();
    expect(mocks.query.mock.calls.at(-1)?.[1]).toEqual([]);
    expect(mocks.query.mock.calls.at(-1)?.[2]).toEqual(props.sessionIds);
    expect(screen.getByRole('alert')).toHaveTextContent('ambiguous accounting identities');
  });
  it('rejects an old success after a same-layout mutation while preserving the replacement proof', async () => {
    let resolve!: (value: Record<string, RangeTotals>[]) => void;
    mocks.query.mockImplementationOnce(() => new Promise(done => { resolve = done; }));
    render(CalendarActivity, props);
    await waitFor(() => expect(mocks.query).toHaveBeenCalledTimes(1));
    sessionsStore.applyMutations([], ['irrelevant']);
    resolve([{ 'codex:parent': range }, {}]);
    await screen.findByText('Zero activity in the recorded history for this range.');
    expect(screen.queryByRole('button', { name: /70 tokens/ })).not.toBeInTheDocument();
    expect(mocks.query.mock.calls.at(-1)?.[2]).toEqual(props.sessionIds);
  });

});
