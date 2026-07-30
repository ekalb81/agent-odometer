import { describe, expect, it, vi } from 'vitest';
import type { DefenderExclusionReceipt } from '../types';
import { createDefenderActionStore } from './defender.svelte';

const receipt: DefenderExclusionReceipt = {
  version: 1,
  configured_roots: [String.raw`C:\sessions`],
  verified_roots: [String.raw`C:\sessions`],
  verified_at: '2026-07-29T12:00:00Z',
};

describe('Defender action store', () => {
  it('keeps one request in flight and publishes its verified receipt', async () => {
    let resolveRequest!: (value: DefenderExclusionReceipt) => void;
    const requestReceipt = vi.fn(() => new Promise<DefenderExclusionReceipt>((resolve) => {
      resolveRequest = resolve;
    }));
    const applyReceipt = vi.fn();
    const store = createDefenderActionStore(requestReceipt, applyReceipt);

    const first = store.request('settings');
    const second = store.request('banner');
    expect(second).toBe(first);
    expect(requestReceipt).toHaveBeenCalledTimes(1);
    expect(store.phase).toBe('pending');
    expect(store.origin).toBe('settings');
    expect(store.error).toBeNull();

    store.clearFeedback();
    expect(store.phase).toBe('pending');
    resolveRequest(receipt);
    await first;

    expect(applyReceipt).toHaveBeenCalledWith(receipt);
    expect(store.phase).toBe('success');
    store.clearFeedback();
    expect(store.phase).toBe('idle');
    expect(store.origin).toBeNull();
  });

  it('retains an actionable error and allows retry', async () => {
    const requestReceipt = vi.fn()
      .mockRejectedValueOnce(new Error('policy blocked the request'))
      .mockResolvedValueOnce(receipt);
    const applyReceipt = vi.fn();
    const store = createDefenderActionStore(requestReceipt, applyReceipt);

    await expect(store.request('banner')).rejects.toThrow('policy blocked the request');
    expect(store.phase).toBe('error');
    expect(store.origin).toBe('banner');
    expect(store.error).toContain('policy blocked the request');

    await expect(store.request('settings')).resolves.toEqual(receipt);
    expect(requestReceipt).toHaveBeenCalledTimes(2);
    expect(store.phase).toBe('success');
    expect(store.origin).toBe('settings');
    expect(store.error).toBeNull();
  });
});
