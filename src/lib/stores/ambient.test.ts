import { beforeEach, expect, it, vi } from 'vitest';
import { get } from 'svelte/store';
import type { AmbientSnapshot } from '../types';
const { checkAmbientAlerts, getAmbientStatus } = vi.hoisted(() => ({ checkAmbientAlerts: vi.fn(), getAmbientStatus: vi.fn() }));
vi.mock('../ipc', () => ({ checkAmbientAlerts, getAmbientStatus }));
const snapshot = (id: string): AmbientSnapshot => ({ available: true, as_of: id, notifications: { enabled: true, quiet_hours: null }, alerts: [], recent: [] });
function pending<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(done => { resolve = done; }); return { promise, resolve }; }
beforeEach(() => { vi.resetModules(); vi.clearAllMocks(); });
it('serializes consuming requests so concurrent callers cannot consume distinct edges', async () => {
  const request = pending<AmbientSnapshot>(); checkAmbientAlerts.mockReturnValue(request.promise);
  const store = await import('./ambient');
  const first = store.refreshAmbient(), second = store.refreshAmbient();
  expect(first).toBe(second); expect(checkAmbientAlerts).toHaveBeenCalledOnce();
  request.resolve(snapshot('delivery')); await first;
  expect(get(store.ambient)?.as_of).toBe('delivery');
});
it('does not let a settings read erase a newer consuming response', async () => {
  const read = pending<AmbientSnapshot>(), collect = pending<AmbientSnapshot>();
  getAmbientStatus.mockReturnValue(read.promise); checkAmbientAlerts.mockReturnValue(collect.promise);
  const store = await import('./ambient'); const reading = store.readAmbient(), collecting = store.refreshAmbient();
  collect.resolve(snapshot('edge')); await collecting; read.resolve(snapshot('history')); await reading;
  expect(get(store.ambient)?.as_of).toBe('edge');
});
it('rejects responses from before a policy change and allows a fresh collector', async () => {
  const old = pending<AmbientSnapshot>(); checkAmbientAlerts.mockReturnValueOnce(old.promise).mockResolvedValueOnce(snapshot('current'));
  const store = await import('./ambient'); const collecting = store.refreshAmbient(); store.invalidateAmbient();
  old.resolve(snapshot('obsolete')); await collecting; expect(get(store.ambient)).toBeNull();
  await store.refreshAmbient(); expect(get(store.ambient)?.as_of).toBe('current');
});
it('fails closed on collection errors and keeps a pending collector authoritative over a read', async () => {
  const collect = pending<AmbientSnapshot>(); checkAmbientAlerts.mockReturnValueOnce(collect.promise).mockRejectedValueOnce(new Error('private detail'));
  getAmbientStatus.mockResolvedValue(snapshot('history'));
  const store = await import('./ambient'); const collecting = store.refreshAmbient(); await store.readAmbient();
  expect(get(store.ambient)).toBeNull(); collect.resolve(snapshot('edge')); await collecting;
  await store.refreshAmbient(); expect(get(store.ambient)).toBeNull();
});
