import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import TranscriptSearch from './TranscriptSearch.svelte';
import type { TranscriptSearchPage, TranscriptSearchHit } from '../lib/types';
const mocks = vi.hoisted(() => ({ searchSessionContent: vi.fn(), resolveRetainedSearchTarget: vi.fn(), getTranscriptPage: vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const source: TranscriptSearchHit = { target: { kind: 'source_record', session_id: 'session-a', record_id: 'source:exact', block_index: 1 }, content_kind: 'text', snippet: { text: '🦀 needle <script>synthetic</script>', match_start: 3, match_end: 9, truncated_before: false, truncated_after: false } };
const retained: TranscriptSearchHit = { ...source, target: { kind: 'retained_turn', session_id: 'session-a', session_identity: 'lineage:old', snapshot_revision: 'snapshot:1:hash', turn_id: 'exact-turn', field: 'user_message' }, content_kind: 'user_message' };
const page = (hits: TranscriptSearchHit[] = [source]): TranscriptSearchPage => ({ phase: 'source', hits, next_cursor: null, issues: [], source_complete: true, retained_complete: false, scanned_records: 2, scanned_messages: 0 });
beforeEach(() => {
  vi.resetAllMocks();
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.setAttribute('open', ''); } });
  mocks.searchSessionContent.mockResolvedValue(page());
});
async function run(): Promise<void> {
  await userEvent.type(screen.getByLabelText('Find text'), 'needle');
  await userEvent.click(screen.getByRole('button', { name: 'Search from start' }));
}
it('keeps tool scopes off by default and uses explicit choices in the typed request', async () => {
  render(TranscriptSearch, { sessionId: 'session-a', onclose: vi.fn() });
  expect(mocks.searchSessionContent).not.toHaveBeenCalled();
  await run();
  expect(mocks.searchSessionContent).toHaveBeenLastCalledWith({ session_id: 'session-a', query: 'needle', scope: { conversation: true, tool_calls: false, tool_results: false }, cursor: null });
  await userEvent.click(screen.getByLabelText('Tool call arguments'));
  expect(screen.queryByText('Source record matches')).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole('button', { name: 'Search from start' }));
  expect(mocks.searchSessionContent).toHaveBeenLastCalledWith(expect.objectContaining({ scope: { conversation: true, tool_calls: true, tool_results: false } }));
  await userEvent.click(screen.getByLabelText('Conversation messages'));
  await userEvent.click(screen.getByLabelText('Tool call arguments'));
  expect(screen.getByRole('button', { name: 'Search from start' })).toBeDisabled();
});
it('highlights UTF-16 spans without rendering snippet markup', async () => {
  const { container } = render(TranscriptSearch, { sessionId: 'session-a', onclose: vi.fn() });
  await run();
  expect(await screen.findByText('needle', { selector: 'mark' })).toBeInTheDocument();
  expect(container.querySelector('script')).toBeNull();
  expect(screen.getByText(/Matching records on this page: 1/)).toBeInTheDocument();
});
it('drops delayed results when query or session changes and clears already displayed bodies', async () => {
  let resolveFirst!: (value: TranscriptSearchPage) => void;
  mocks.searchSessionContent.mockReturnValueOnce(new Promise(resolve => { resolveFirst = resolve; }));
  const view = render(TranscriptSearch, { sessionId: 'session-a', onclose: vi.fn() });
  await run();
  await userEvent.type(screen.getByLabelText('Find text'), ' changed');
  resolveFirst(page());
  await waitFor(() => expect(screen.queryByText('Source record matches')).not.toBeInTheDocument());
  await userEvent.click(screen.getByRole('button', { name: 'Search from start' }));
  await screen.findByText('Source record matches');
  await view.rerender({ sessionId: 'session-b', onclose: vi.fn() });
  await waitFor(() => expect(screen.getByLabelText('Find text')).toHaveValue(''));
  expect(screen.queryByText('Source record matches')).not.toBeInTheDocument();
});
it('shows partial and omitted coverage without claiming a complete absence of matches', async () => {
  mocks.searchSessionContent.mockResolvedValue({ ...page([]), source_complete: false, issues: ['record_too_large', 'retained_snapshot_too_large'] });
  render(TranscriptSearch, { sessionId: 'session-a', onclose: vi.fn() });
  await run();
  await screen.findByText('No matches in the examined source page.');
  expect(screen.getByText(/Source coverage is partial/)).toBeInTheDocument();
  expect(screen.getByText(/oversized source record was not searched/)).toBeInTheDocument();
  expect(screen.getByText(/8 MiB search limit/)).toBeInTheDocument();
});
it('continues using the exact source/retained cursor with separate provenance', async () => {
  const cursor = { session_id: 'session-a', query: 'needle', scope: { conversation: true, tool_calls: false, tool_results: false }, position: { phase: 'retained' as const, session_identity: 'lineage:old', snapshot_revision: 'snapshot:1:hash', next_turn: 0, incomplete: false } };
  mocks.searchSessionContent.mockResolvedValueOnce({ ...page([]), source_complete: false, next_cursor: cursor }).mockResolvedValueOnce({ ...page([retained]), phase: 'retained', source_complete: false, retained_complete: true, scanned_records: 0, scanned_messages: 2, issues: ['retained_prompt_and_final_reply_only', 'retained_matches_may_overlap_source'] });
  render(TranscriptSearch, { sessionId: 'session-a', onclose: vi.fn() });
  await run();
  await userEvent.click(await screen.findByRole('button', { name: 'Search retained messages' }));
  await screen.findByText('Retained message matches');
  expect(mocks.searchSessionContent).toHaveBeenLastCalledWith(expect.objectContaining({ cursor }));
  expect(screen.getByText(/may overlap them/)).toBeInTheDocument();
  expect(screen.getByText(/Other messages and tool bodies are not retained/)).toBeInTheDocument();
});
it('resolves exact retained fields and rejects expired targets without redirecting', async () => {
  mocks.searchSessionContent.mockResolvedValue({ ...page([retained]), phase: 'retained', source_complete: false });
  mocks.resolveRetainedSearchTarget.mockResolvedValueOnce({ target: retained.target, text: 'Exact retained synthetic prompt', truncated: true }).mockRejectedValueOnce(new Error('changed'));
  render(TranscriptSearch, { sessionId: 'session-a', onclose: vi.fn() });
  await run();
  await userEvent.click(await screen.findByRole('button', { name: 'Open retained message · user message' }));
  expect(await screen.findByText('Exact retained synthetic prompt')).toBeInTheDocument();
  expect(mocks.resolveRetainedSearchTarget).toHaveBeenCalledWith(retained.target);
  expect(screen.getByRole('region', { name: 'Selected retained message' })).toHaveFocus();
  await userEvent.click(screen.getByRole('button', { name: 'Open retained message · user message' }));
  await screen.findByRole('alert');
  expect(screen.queryByText('Exact retained synthetic prompt')).not.toBeInTheDocument();
  expect(mocks.getTranscriptPage).not.toHaveBeenCalled();
});
it('opens and focuses the exact matching source block in the inspector', async () => {
  mocks.getTranscriptPage.mockResolvedValue({ provider: 'codex', availability: 'partial', issues: ['earlier_records_not_inspected'], source_complete: false, next_cursor: null, records: [{ id: 'source:exact', byte_offset: 100, byte_length: 90, raw_json: '{"synthetic":true}', kind: 'response_item', message_id: null, issue: null, presentation: { role: 'assistant', timestamp: null, blocks: [{ kind: 'text', text: 'Before', name: null, call_id: null, edit: null }, { kind: 'text', text: 'Exact matching needle', name: null, call_id: null, edit: null }] } }] });
  render(TranscriptSearch, { sessionId: 'session-a', onclose: vi.fn() });
  await run();
  await userEvent.click(await screen.findByRole('button', { name: 'Open source record · text' }));
  await screen.findByText('Exact matching needle');
  expect(mocks.getTranscriptPage).toHaveBeenCalledWith(expect.objectContaining({ session_id: 'session-a', record_id: 'source:exact', cursor: null }));
  await waitFor(() => expect(document.getElementById('transcript-source:exact-block-1')).toHaveFocus());
});
