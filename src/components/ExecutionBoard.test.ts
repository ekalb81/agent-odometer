import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import ExecutionBoard from './ExecutionBoard.svelte';
import { rates } from '../lib/stores/rates';
import type { ExecutionPage, SessionSummary, RateCard } from '../lib/types';
const mocks = vi.hoisted(() => ({ getExecutionPage: vi.fn(), getSessionPricing: vi.fn(), getTranscriptPage: vi.fn(), getRecordBookmarks: vi.fn(), editRecordBookmark: vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const session = (id: string, harness = 'codex'): SessionSummary => ({
  id, storage_id: `${harness}:${id}`, harness, thread_name: `Synthetic ${id}`,
  parent_thread_id: null, project_key: 'project', project_label: 'Synthetic project',
  started_at: '2026-01-01T00:00:00Z', last_event_at: '2026-01-01T00:01:00Z',
  source_availability: 'present', tokens_total: { total_tokens: 123 }, first_user_message: 'private prompt body',
}) as SessionSummary;
const page: ExecutionPage = { availability: 'partial', issues: ['incomplete_tail'], records: [{ record_id: 'exact-anchor', timestamp: '2026-01-01T00:00:10Z', role: 'assistant', issue: null, blocks: [{ kind: 'tool_call', call_id: 'c', name: 'Read' }] }], next_cursor: null, source_complete: false };
beforeEach(() => {
  vi.resetAllMocks(); rates.set(null);
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.setAttribute('open', ''); } });
  mocks.getExecutionPage.mockResolvedValue(page);
  mocks.getTranscriptPage.mockResolvedValue({ availability: 'available', issues: [], records: [], next_cursor: null, source_complete: true, provider: 'codex' });
  mocks.getRecordBookmarks.mockResolvedValue({ identity: { session_key: 'codex:a', fingerprint: 'synthetic', anchor: '' }, bookmarks: [], recovery_backup_unrestored: false });
});
it('loads bounded metadata without source bodies, then opens the exact record lazily', async () => {
  render(ExecutionBoard, { sessions: [session('a')], initialId: 'codex:a', onclose: vi.fn() });
  await screen.findByText(/partial coverage; unvisited records/);
  expect(mocks.getExecutionPage).toHaveBeenCalledWith({ session_id: 'codex:a', cursor: null, max_records: 25, max_bytes: 131_072 });
  expect(mocks.getTranscriptPage).not.toHaveBeenCalled();
  expect(screen.queryByText('private prompt body')).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole('button', { name: /Inspect Read at/ }));
  await waitFor(() => expect(mocks.getTranscriptPage).toHaveBeenCalledWith(expect.objectContaining({ session_id: 'codex:a', record_id: 'exact-anchor' })));
  expect(screen.getByRole('group', { name: 'Transcript inspector' })).toBeInTheDocument();
  expect(screen.getAllByRole('dialog')).toHaveLength(1);
  await userEvent.click(screen.getByLabelText('Record anchor'));
  await userEvent.keyboard('{Escape}');
  expect(screen.queryByRole('group', { name: 'Transcript inspector' })).not.toBeInTheDocument();
  expect(screen.getAllByRole('dialog')).toHaveLength(1);
});
it('rejects a delayed page after clearing selection and keeps the empty state honest', async () => {
  let resolve!: (value: ExecutionPage) => void;
  mocks.getExecutionPage.mockImplementation(() => new Promise<ExecutionPage>(done => { resolve = done; }));
  render(ExecutionBoard, { sessions: [session('a')], initialId: 'codex:a', onclose: vi.fn() });
  await waitFor(() => expect(mocks.getExecutionPage).toHaveBeenCalledTimes(1));
  await userEvent.click(screen.getByText('Select sessions (1/8)'));
  await userEvent.click(screen.getByRole('button', { name: 'Clear selection' }));
  resolve(page);
  await screen.findByText(/Select sessions to compare/);
  expect(screen.queryByRole('button', { name: /Inspect Read at/ })).not.toBeInTheDocument();
});
it('caps selection and renders only one candidate page for a large history', async () => {
  render(ExecutionBoard, { sessions: Array.from({ length: 100 }, (_, index) => session(String(index))), initialId: 'codex:0', onclose: vi.fn() });
  await userEvent.click(screen.getByText('Select sessions (1/8)'));
  expect(screen.getAllByRole('checkbox')).toHaveLength(25);
  for (const checkbox of screen.getAllByRole('checkbox').slice(1, 8)) await userEvent.click(checkbox);
  await screen.findByText('Select sessions (8/8)');
  expect(screen.getAllByRole('checkbox')[8]).toBeDisabled();
  expect(screen.getAllByRole('checkbox')[0]).not.toBeDisabled();
});
it('filters candidates by provider and project while retaining the explicit selection', async () => {
  render(ExecutionBoard, { sessions: [session('a'), { ...session('b', 'claude_code'), project_key: 'another', project_label: 'Other project' }], initialId: 'codex:a', onclose: vi.fn() });
  await userEvent.click(screen.getByText('Select sessions (1/8)'));
  await userEvent.selectOptions(screen.getByLabelText('Provider'), 'claude_code');
  await userEvent.selectOptions(screen.getByLabelText('Project'), 'another');
  expect(screen.getAllByRole('checkbox')).toHaveLength(1);
  expect(screen.getByRole('checkbox')).not.toBeChecked();
  expect(screen.getByRole('region', { name: 'Execution of Synthetic a' })).toBeInTheDocument();
});
it('does not combine a cursor-invalid source with already visited records', async () => {
  const first = { ...page, next_cursor: { session_id: 'codex:a', source_id: 'synthetic', generation: 'old', offset: 20, record_start: 20, partial: false } };
  mocks.getExecutionPage.mockResolvedValueOnce(first).mockResolvedValueOnce({ ...page, availability: 'cursor_invalid', records: [], issues: ['source_changed'] });
  render(ExecutionBoard, { sessions: [session('a')], initialId: 'codex:a', onclose: vi.fn() });
  await screen.findByRole('button', { name: /Inspect Read at/ });
  await userEvent.click(screen.getByRole('button', { name: 'Load next bounded page' }));
  await screen.findByText('source changed');
  expect(screen.queryByRole('button', { name: /Inspect Read at/ })).not.toBeInTheDocument();
});
it('rejects a superseded price after a same-version rate-card replacement', async () => {
  const rateCard = { version: 1, currency: 'credits', currencies: { codex: 'credits' } } as unknown as RateCard;
  const priced = (total: number) => ({ 'codex:a': { pricing: { plan: { total, by_model: [], missing_models: [], unpriced_models: [] }, api: null }, categories: {} } });
  let oldReply!: (value: unknown) => void;
  mocks.getSessionPricing.mockImplementationOnce(() => new Promise(resolve => { oldReply = resolve; })).mockResolvedValueOnce(priced(20));
  rates.set(rateCard);
  render(ExecutionBoard, { sessions: [session('a')], initialId: 'codex:a', onclose: vi.fn() });
  await waitFor(() => expect(mocks.getSessionPricing).toHaveBeenCalledTimes(1));
  rates.set({ ...rateCard });
  await screen.findByText(/20.00 legacy credits/);
  oldReply(priced(999));
  await waitFor(() => expect(screen.queryByText(/999.00 legacy credits/)).not.toBeInTheDocument());
  expect(screen.getByText(/20.00 legacy credits/)).toBeInTheDocument();
});
