import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import TranscriptHandoff from './TranscriptHandoff.svelte';
import type { Session, TranscriptPage } from '../lib/types';

const mocks = vi.hoisted(() => ({ getTranscriptPage: vi.fn(), writeExport: vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const session = {
  storage_id: 'codex:selected', source_availability: 'missing',
  turns: [{ index: 1, status: 'completed', started_at: null, completed_at: null,
    user_message: 'Selected prompt private phrase', last_agent_message: 'Chosen answer', tool_metrics: { calls: 1, successes: 1, failures: 0 } }],
} as Session;
const page: TranscriptPage = { provider: 'codex', availability: 'available', source_complete: true,
  next_cursor: null, issues: [], records: [{ id: 'source-one', byte_offset: 15, byte_length: 20,
    raw_json: 'PRIVATE_RAW', kind: 'response_item', message_id: null, issue: null,
    presentation: { role: 'assistant', timestamp: null, blocks: [{ kind: 'text', text: 'Original source answer', call_id: null, name: null, edit: null }] } }] };
beforeEach(() => {
  vi.resetAllMocks();
  mocks.getTranscriptPage.mockResolvedValue(page);
  mocks.writeExport.mockResolvedValue(true);
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.setAttribute('open', ''); } });
});
const first = () => screen.getByRole('checkbox', { name: /Retained turn 1 · user_message/ });
async function preview(): Promise<HTMLIFrameElement> {
  await userEvent.click(first());
  await userEvent.click(screen.getByRole('button', { name: 'Build handoff preview' }));
  return await screen.findByTitle('Exact handoff HTML preview') as HTMLIFrameElement;
}
it('starts with no selection, requires review, and saves or copies the exact redacted selected content', async () => {
  const user = userEvent.setup();
  render(TranscriptHandoff, { session, onclose: vi.fn() });
  expect(mocks.getTranscriptPage).not.toHaveBeenCalled();
  expect(screen.getByRole('button', { name: 'Build handoff preview' })).toBeDisabled();
  await user.type(screen.getByRole('textbox', { name: /Also redact/ }), 'private phrase');
  const frame = await preview();
  expect(frame).toHaveAttribute('sandbox', '');
  expect(frame.srcdoc).not.toMatch(/private phrase|Chosen answer|PRIVATE_RAW|codex:selected/);
  const save = screen.getByRole('button', { name: 'Save reviewed handoff…' });
  expect(save).toBeDisabled();
  await user.click(screen.getByRole('checkbox', { name: /I reviewed every/ }));
  await user.click(save);
  expect(mocks.writeExport).toHaveBeenCalledWith('odometer-handoff.html', 'html', frame.srcdoc);
  await screen.findByText('Saved the exact reviewed handoff.');
  const clipboard = vi.spyOn(navigator.clipboard, 'writeText');
  const markdown = (screen.getByRole('textbox', { name: 'Exact handoff Markdown' }) as HTMLTextAreaElement).value;
  await user.click(screen.getByRole('button', { name: 'Copy reviewed handoff' }));
  expect(clipboard).toHaveBeenCalledWith(markdown);
  await screen.findByText('Copied the exact reviewed handoff.');
  await user.type(screen.getByRole('textbox', { name: /User-written/ }), 'New next step');
  expect(screen.queryByTitle('Exact handoff HTML preview')).not.toBeInTheDocument();
});
it('rejects late reads after cancellation or changing session and clears private draft notes', async () => {
  let resolve!: (value: TranscriptPage) => void;
  mocks.getTranscriptPage.mockImplementation(() => new Promise(done => { resolve = done; }));
  const view = render(TranscriptHandoff, { session, onclose: vi.fn() });
  await userEvent.type(screen.getByRole('textbox', { name: /User-written/ }), 'Private old note');
  await userEvent.click(screen.getByRole('button', { name: 'Read source records' }));
  await userEvent.click(screen.getByRole('button', { name: 'Cancel read' }));
  resolve(page);
  await waitFor(() => expect(screen.queryByText('Original source answer')).not.toBeInTheDocument());
  await userEvent.click(screen.getByRole('button', { name: 'Read source records' }));
  await view.rerender({ session: { ...session, storage_id: 'claude:another', turns: [] } });
  resolve(page);
  await waitFor(() => expect(screen.getByRole('textbox', { name: /User-written/ })).toHaveValue(''));
  expect(screen.queryByText('Original source answer')).not.toBeInTheDocument();
  expect(screen.queryByTitle('Exact handoff HTML preview')).not.toBeInTheDocument();
});
it('resets selection when source scope changes and exposes exact source inspection only in the app', async () => {
  render(TranscriptHandoff, { session, onclose: vi.fn() });
  await userEvent.click(screen.getByRole('button', { name: 'Read source records' }));
  const source = await screen.findByRole('checkbox', { name: /Source record at byte 15/ });
  expect(mocks.getTranscriptPage).toHaveBeenCalledWith(expect.objectContaining({ session_id: 'codex:selected' }));
  expect(screen.getByRole('button', { name: 'Inspect exact source' })).toBeEnabled();
  await userEvent.click(source);
  await userEvent.click(screen.getByRole('button', { name: 'Build handoff preview' }));
  expect((await screen.findByTitle('Exact handoff HTML preview')).getAttribute('srcdoc')).toContain('Original source answer');
  await userEvent.click(screen.getByRole('checkbox', { name: /Allow tool calls/ }));
  expect(screen.queryByTitle('Exact handoff HTML preview')).not.toBeInTheDocument();
  expect(screen.queryByRole('checkbox', { name: /Source record/ })).not.toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Build handoff preview' })).toBeDisabled();
});
it('reports unavailable source and canceled or failed saves without disclosing error payloads', async () => {
  render(TranscriptHandoff, { session, onclose: vi.fn() });
  mocks.getTranscriptPage.mockRejectedValue(new Error('PRIVATE_SOURCE'));
  await userEvent.click(screen.getByRole('button', { name: 'Read source records' }));
  await screen.findByText(/Source read unavailable/);
  expect(screen.queryByText(/PRIVATE_SOURCE/)).not.toBeInTheDocument();
  await preview();
  await userEvent.click(screen.getByRole('checkbox', { name: /I reviewed every/ }));
  mocks.writeExport.mockResolvedValue(false);
  await userEvent.click(screen.getByRole('button', { name: 'Save reviewed handoff…' }));
  await screen.findByText('Save canceled.');
  mocks.writeExport.mockRejectedValue(new Error('PRIVATE_DESTINATION'));
  await userEvent.click(screen.getByRole('button', { name: 'Save reviewed handoff…' }));
  await screen.findByText(/could not be copied or saved/);
  expect(screen.queryByText(/PRIVATE_DESTINATION/)).not.toBeInTheDocument();
});
