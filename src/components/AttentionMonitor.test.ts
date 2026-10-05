import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import AttentionMonitor from './AttentionMonitor.svelte';
import { attention } from '../lib/stores/attention';
import type { AttentionSnapshot } from '../lib/types';
const { getAttentionStatus } = vi.hoisted(() => ({ getAttentionStatus: vi.fn() }));
vi.mock('../lib/ipc', () => ({ getAttentionStatus, setAttentionPreferences: vi.fn() }));
const notify = vi.fn();
function snapshot(id = 'first'): AttentionSnapshot { return { available: true, preferences: { revision: 1, categories: ['turn_completed'], providers: [], tool_kind: null, stale_after_seconds: 300 }, observations: [], alerts: [{ id, session_ref: 'abc', provider_label: 'Codex', category: 'turn_completed', observed_at: '2026-10-04T12:00:00Z', source: 'retained turn completion' }] }; }
beforeEach(() => { vi.clearAllMocks(); attention.set(null); getAttentionStatus.mockResolvedValue(snapshot()); const NotificationMock = function(this: unknown, ...args: unknown[]) { notify(...args); }; Object.assign(NotificationMock, { permission: 'granted' }); vi.stubGlobal('Notification', NotificationMock); });
afterEach(() => { vi.unstubAllGlobals(); });
it('does not replay opening backlog; new alerts notify once and dismiss remains dismissed', async () => {
  render(AttentionMonitor);
  await waitFor(() => expect(getAttentionStatus).toHaveBeenCalledOnce());
  await new Promise(resolve => setTimeout(resolve, 0));
  expect(notify).not.toHaveBeenCalled();
  attention.set(snapshot('next'));
  expect(await screen.findByRole('status')).toHaveTextContent('Codex · Session abc: Turn completed. Source: retained turn completion. Observed');
  expect(notify).toHaveBeenCalledOnce();
  await fireEvent.click(screen.getByRole('button', { name: 'Dismiss transcript alert' })); attention.set(snapshot('next'));
  await waitFor(() => expect(screen.queryByRole('status')).not.toBeInTheDocument()); expect(notify).toHaveBeenCalledOnce();
});
it('removes notices when disabled or expired and survives unavailable desktop notifications', async () => {
  render(AttentionMonitor); await waitFor(() => expect(getAttentionStatus).toHaveBeenCalledOnce()); await new Promise(resolve => setTimeout(resolve, 0));
  notify.mockImplementation(() => { throw new Error('not supported'); }); attention.set(snapshot('new'));
  expect(await screen.findByRole('status')).toBeVisible();
  const expired = snapshot(); expired.alerts = []; attention.set(expired);
  await waitFor(() => expect(screen.queryByRole('status')).not.toBeInTheDocument());
});
