import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import TranscriptInspector from './TranscriptInspector.svelte';
import type { TranscriptCursor, TranscriptPage, TranscriptRecord } from '../lib/types';
const mocks = vi.hoisted(() => ({ getTranscriptPage: vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const cursor: TranscriptCursor = { session_id: 'session-a', source_id: 'source', generation: 'g', offset: 100, record_start: 100, partial: false };
const call: TranscriptRecord = { id: 'source:call', byte_offset: 0, byte_length: 90, raw_json: '{"synthetic":"call"}', kind: 'response_item', message_id: null, issue: null, presentation: { role: 'assistant', timestamp: null, blocks: [{ kind: 'tool_call', name: 'Read', call_id: 'call-1', text: 'synthetic.rs', edit: null }] } };
const output: TranscriptRecord = { ...call, id: 'source:result', byte_offset: 100, presentation: { role: 'tool', timestamp: null, blocks: [{ kind: 'tool_result', name: null, call_id: 'call-1', text: 'line one\nline two', edit: null }] } };
function page(records: TranscriptRecord[], next = false): TranscriptPage { return { provider: 'codex', availability: 'available', issues: [], records, next_cursor: next ? cursor : null, source_complete: !next }; }
beforeEach(() => {
  vi.resetAllMocks();
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.setAttribute('open', ''); } });
});
it('pairs explicit tool IDs across bounded pages and returns to an anchored page', async () => {
  mocks.getTranscriptPage.mockImplementation(async (request) => request.cursor ? page([output]) : page([call], true));
  render(TranscriptInspector, { sessionId: 'session-a', onclose: vi.fn() });
  await screen.findByRole('button', { name: 'Expand assistant' });
  expect(screen.queryByText('synthetic.rs')).not.toBeInTheDocument();
  await userEvent.click(screen.getByRole('button', { name: 'Expand assistant' }));
  await userEvent.click(screen.getByRole('button', { name: 'Next page' }));
  await userEvent.click(await screen.findByRole('button', { name: 'Expand tool' }));
  await userEvent.click(screen.getByRole('button', { name: 'Jump to tool call' }));
  await waitFor(() => expect(mocks.getTranscriptPage).toHaveBeenLastCalledWith(expect.objectContaining({ record_id: call.id, cursor: null, max_bytes: 131_072 })));
  await screen.findByRole('button', { name: 'Collapse assistant' });
  await userEvent.click(screen.getByRole('button', { name: 'Previous page' }));
  await screen.findByRole('button', { name: 'Expand tool' });
  await userEvent.click(screen.getByRole('button', { name: 'Return to selected record' }));
  await screen.findByRole('button', { name: 'Collapse assistant' });
});
it('discards a delayed response after switching sessions', async () => {
  let resolveFirst!: (value: TranscriptPage) => void;
  mocks.getTranscriptPage.mockReturnValueOnce(new Promise(resolve => { resolveFirst = resolve; })).mockResolvedValue(page([output]));
  const view = render(TranscriptInspector, { sessionId: 'session-a', onclose: vi.fn() });
  await waitFor(() => expect(mocks.getTranscriptPage).toHaveBeenCalledTimes(1));
  await view.rerender({ sessionId: 'session-b', onclose: vi.fn() });
  await screen.findByRole('button', { name: 'Expand tool' });
  resolveFirst(page([call]));
  await waitFor(() => expect(screen.queryByRole('button', { name: 'Expand assistant' })).not.toBeInTheDocument());
});
it('discards previously opened bodies and anchors immediately while a different session loads', async () => {
  let resolveNext!: (value: TranscriptPage) => void;
  mocks.getTranscriptPage.mockResolvedValueOnce(page([call])).mockReturnValueOnce(new Promise(resolve => { resolveNext = resolve; }));
  const view = render(TranscriptInspector, { sessionId: 'session-a', onclose: vi.fn() });
  await userEvent.click(await screen.findByRole('button', { name: 'Expand assistant' }));
  await userEvent.click(screen.getByRole('button', { name: 'Select anchor' }));
  expect(screen.getByText('synthetic.rs')).toBeInTheDocument();
  expect(screen.getByLabelText('Record anchor')).toHaveValue(call.id);
  await view.rerender({ sessionId: 'session-b', onclose: vi.fn() });
  await waitFor(() => expect(mocks.getTranscriptPage).toHaveBeenCalledTimes(2));
  expect(screen.queryByText('synthetic.rs')).not.toBeInTheDocument();
  expect(screen.queryByText(call.id)).not.toBeInTheDocument();
  expect(screen.getByLabelText('Record anchor')).toHaveValue('');
  resolveNext(page([output]));
  await screen.findByRole('button', { name: 'Expand tool' });
});
it.each(['__proto__', 'constructor'])('pairs literal provider call ID %s without inherited object entries', async (call_id) => {
  const withId = (record: TranscriptRecord): TranscriptRecord => ({ ...record, presentation: { ...record.presentation!, blocks: record.presentation!.blocks.map(block => ({ ...block, call_id })) } });
  mocks.getTranscriptPage.mockResolvedValue(page([withId(call), withId(output)]));
  render(TranscriptInspector, { sessionId: 'session-a', onclose: vi.fn() });
  await userEvent.click(await screen.findByRole('button', { name: 'Expand tool' }));
  await userEvent.click(screen.getByRole('button', { name: 'Jump to tool call' }));
  await waitFor(() => expect(mocks.getTranscriptPage).toHaveBeenLastCalledWith(expect.objectContaining({ record_id: call.id })));
  expect(screen.queryByRole('alert')).not.toBeInTheDocument();
});
it('renders omitted payloads honestly and escapes explicitly opened raw content', async () => {
  const record = { ...call, id: 'source:unsafe', raw_json: '<script>window.secret=1</script>', presentation: null };
  mocks.getTranscriptPage.mockResolvedValue({ ...page([{ ...output, id: 'source:omitted', raw_json: null, presentation: null, issue: 'record_too_large' }, record]), availability: 'partial', source_complete: false, issues: ['incomplete_tail'] });
  const { container } = render(TranscriptInspector, { sessionId: 'session-a', onclose: vi.fn() });
  await screen.findByText('record too large · payload unavailable');
  expect(screen.getByText(/This transcript view is partial/)).toBeInTheDocument();
  await userEvent.click(screen.getAllByRole('button', { name: 'Expand response_item' })[1]);
  await userEvent.click(screen.getByText('Raw source record'));
  expect(screen.getByText('<script>window.secret=1</script>')).toBeInTheDocument();
  expect(container.querySelector('script')).toBeNull();
});
it('keeps missing-source content unavailable and exposes a direct anchor request', async () => {
  mocks.getTranscriptPage.mockResolvedValue({ ...page([]), availability: 'missing', issues: ['source_missing'], source_complete: false });
  render(TranscriptInspector, { sessionId: 'session-a', recordId: 'source:call', onclose: vi.fn() });
  await screen.findByText(/Transcript missing/);
  expect(mocks.getTranscriptPage).toHaveBeenCalledWith(expect.objectContaining({ record_id: 'source:call' }));
  expect(screen.getByRole('button', { name: 'Next page' })).toBeDisabled();
});
it('does not label a provider-recorded empty output as an absent payload', async () => {
  const empty = { ...output, raw_json: '{"output":""}', presentation: { ...output.presentation!, blocks: output.presentation!.blocks.map(block => ({ ...block, text: '' })) } };
  mocks.getTranscriptPage.mockResolvedValue(page([empty]));
  render(TranscriptInspector, { sessionId: 'session-a', onclose: vi.fn() });
  await userEvent.click(await screen.findByRole('button', { name: 'Expand tool' }));
  expect(screen.queryByText('Payload not recorded')).not.toBeInTheDocument();
  expect(screen.getByText(/Inspect the raw source record for recorded fields/)).toBeInTheDocument();
  await userEvent.click(screen.getByText('Raw source record'));
  expect(screen.getByText('{"output":""}')).toBeInTheDocument();
});
