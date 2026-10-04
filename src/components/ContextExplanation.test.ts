import { render, screen, waitFor } from '@testing-library/svelte';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import ContextExplanation from './ContextExplanation.svelte';
import type { TranscriptPage } from '../lib/types';
const mocks = vi.hoisted(() => ({ getTranscriptPage: vi.fn() }));
vi.mock('../lib/ipc', () => mocks);
const page: TranscriptPage = { provider: 'codex', availability: 'partial', issues: ['incomplete_tail'], source_complete: false, next_cursor: null, records: [{ id: 'synthetic:boundary', byte_offset: 0, byte_length: 50, raw_json: null, kind: 'compacted', message_id: null, issue: null, context_evidence: { contributors: [], compaction: true, pre_compaction_tokens: null, input_tokens: null, output_tokens: null, context_window: null, unsupported: false } }] };
beforeEach(() => {
  vi.resetAllMocks();
  Object.defineProperty(HTMLDialogElement.prototype, 'showModal', { configurable: true, value: function(this: HTMLDialogElement) { this.setAttribute('open', ''); } });
});
it('labels partial evidence and compaction without fabricating token attribution, and opens the exact anchor', async () => {
  mocks.getTranscriptPage.mockResolvedValue(page);
  const oninspect = vi.fn();
  render(ContextExplanation, { sessionId: 'codex:synthetic', onclose: vi.fn(), oninspect });
  await screen.findByText('Recorded compaction boundary');
  expect(screen.getByText(/Partial history/)).toBeInTheDocument();
  expect(screen.getByText(/pre-compaction tokens: unavailable/)).toBeInTheDocument();
  expect(screen.getByText(/Source gaps: incomplete tail/)).toBeInTheDocument();
  expect(screen.getByText(/does not estimate token shares/)).toBeInTheDocument();
  await userEvent.click(screen.getByRole('button', { name: 'Inspect source record' }));
  expect(oninspect).toHaveBeenCalledWith('synthetic:boundary');
  expect(mocks.getTranscriptPage).toHaveBeenCalledWith(expect.objectContaining({ max_records: 25, max_bytes: 131_072 }));
});
it('clears old evidence while switching sessions and rejects superseded results', async () => {
  let resolve!: (value: TranscriptPage) => void;
  mocks.getTranscriptPage.mockResolvedValueOnce(page).mockReturnValueOnce(new Promise(done => { resolve = done; }));
  const view = render(ContextExplanation, { sessionId: 'codex:first', onclose: vi.fn(), oninspect: vi.fn() });
  await screen.findByText('Recorded compaction boundary');
  await view.rerender({ sessionId: 'codex:second', onclose: vi.fn(), oninspect: vi.fn() });
  await waitFor(() => expect(mocks.getTranscriptPage).toHaveBeenCalledTimes(2));
  expect(screen.queryByText('synthetic:boundary')).not.toBeInTheDocument();
  resolve({ ...page, records: [], availability: 'missing', issues: ['source_missing'] });
  await screen.findByText(/No source evidence available/);
  expect(screen.queryByText('Recorded compaction boundary')).not.toBeInTheDocument();
});
it('reports read failure without presenting previous evidence as current', async () => {
  mocks.getTranscriptPage.mockRejectedValue(new Error('synthetic failure'));
  render(ContextExplanation, { sessionId: 'codex:synthetic', onclose: vi.fn(), oninspect: vi.fn() });
  await screen.findByRole('alert');
  expect(screen.queryByText(/Coverage:/)).not.toBeInTheDocument();
});
