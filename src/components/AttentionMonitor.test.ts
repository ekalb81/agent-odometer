import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import AttentionMonitor from './AttentionMonitor.svelte';
import { ambient } from '../lib/stores/ambient';
import type { AmbientSnapshot } from '../lib/types';
const { checkAmbientAlerts, getAttentionStatus } = vi.hoisted(() => ({ checkAmbientAlerts: vi.fn(), getAttentionStatus: vi.fn() }));
vi.mock('../lib/ipc', () => ({ checkAmbientAlerts, getAttentionStatus, getAmbientStatus: vi.fn(), onQuotaPolicyUpdated: vi.fn().mockResolvedValue(() => {}), setAttentionPreferences: vi.fn() }));
const notify = vi.fn();
function snapshot(id?: string): AmbientSnapshot {
  const notice = { id: id ?? 'old', provider: 'Codex', route: 'attention' as const, code: 'turn_completed', observed_at: '2026-10-04T12:00:00Z', delivered_at: '2026-10-04T12:00:01Z' };
  return { available: true, as_of: notice.delivered_at, notifications: { enabled: true, quiet_hours: null }, alerts: id ? [notice] : [], recent: [notice] };
}
beforeEach(() => {
  vi.clearAllMocks(); ambient.set(null); checkAmbientAlerts.mockResolvedValue(snapshot());
  getAttentionStatus.mockResolvedValue({ available: false, alerts: [], observations: [], preferences: { revision: 0, categories: [] } });
  const NotificationMock = function(...args: unknown[]) { notify(...args); };
  Object.assign(NotificationMock, { permission: 'granted' }); vi.stubGlobal('Notification', NotificationMock);
});
afterEach(() => { vi.unstubAllGlobals(); });
it('recent history never replays; one shared edge delivers once with a typed evidence route', async () => {
  const onOpen = vi.fn(); render(AttentionMonitor, { onOpen });
  await waitFor(() => expect(checkAmbientAlerts).toHaveBeenCalledOnce());
  await new Promise(resolve => setTimeout(resolve, 0)); expect(notify).not.toHaveBeenCalled();
  ambient.set(snapshot('next'));
  expect(await screen.findByRole('status')).toHaveTextContent('Codex: Turn completed. Observed');
  expect(notify).toHaveBeenCalledOnce();
  await fireEvent.click(screen.getByRole('button', { name: 'Evidence' })); expect(onOpen).toHaveBeenCalledWith('attention');
  await fireEvent.click(screen.getByRole('button', { name: 'Dismiss alert' })); ambient.set(snapshot('next'));
  await waitFor(() => expect(screen.queryByRole('status')).not.toBeInTheDocument()); expect(notify).toHaveBeenCalledOnce();
});
it('fails closed on unavailable or disabled policy and preserves in-app delivery when desktop throws', async () => {
  render(AttentionMonitor); await waitFor(() => expect(checkAmbientAlerts).toHaveBeenCalledOnce()); await new Promise(resolve => setTimeout(resolve, 0));
  notify.mockImplementation(() => { throw new Error('unsupported'); }); ambient.set(snapshot('new'));
  expect(await screen.findByRole('status')).toBeVisible();
  ambient.set({ ...snapshot(), available: false });
  await waitFor(() => expect(screen.queryByRole('status')).not.toBeInTheDocument());
  ambient.set({ ...snapshot('disabled'), notifications: { enabled: false, quiet_hours: null } });
  expect(notify).toHaveBeenCalledOnce();
});
it('delivers a resolved and rearmed incident again using distinct edge IDs', async () => {
  render(AttentionMonitor); await waitFor(() => expect(checkAmbientAlerts).toHaveBeenCalledOnce());
  await new Promise(resolve => setTimeout(resolve, 0));
  const incident = (id: string) => {
    const value = snapshot(id); value.alerts[0].code = 'provider_incident'; value.alerts[0].route = 'provider_status'; return value;
  };
  ambient.set(incident('condition:edge1'));
  expect(await screen.findByRole('status')).toHaveTextContent('Public provider incident observed');
  await fireEvent.click(screen.getByRole('button', { name: 'Dismiss alert' }));
  ambient.set(snapshot());
  ambient.set(incident('condition:edge2'));
  expect(await screen.findByRole('status')).toHaveTextContent('Public provider incident observed');
  expect(notify).toHaveBeenCalledTimes(2);
});
