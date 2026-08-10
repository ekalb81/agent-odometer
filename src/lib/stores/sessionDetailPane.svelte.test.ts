import { beforeEach, describe, expect, it, vi } from 'vitest';

const STORAGE_KEY = 'sessionDetailPaneOpen.v1';

async function loadStore() {
  vi.resetModules();
  return import('./sessionDetailPane.svelte');
}

describe('sessionDetailPaneStore', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('starts closed with nothing persisted', async () => {
    const { sessionDetailPaneStore } = await loadStore();
    expect(sessionDetailPaneStore.open).toBe(false);
  });

  it('toggles and persists the open state across a reload', async () => {
    let module = await loadStore();
    module.sessionDetailPaneStore.toggle();
    expect(module.sessionDetailPaneStore.open).toBe(true);
    expect(localStorage.getItem(STORAGE_KEY)).toBe('true');

    module = await loadStore();
    expect(module.sessionDetailPaneStore.open).toBe(true);

    module.sessionDetailPaneStore.setOpen(false);
    expect(module.sessionDetailPaneStore.open).toBe(false);
    expect(localStorage.getItem(STORAGE_KEY)).toBe('false');

    module = await loadStore();
    expect(module.sessionDetailPaneStore.open).toBe(false);
  });

  it('ignores anything persisted other than the literal string "true"', async () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(true));
    const { sessionDetailPaneStore } = await loadStore();
    // Stored as JSON.stringify(true) === 'true', so this one does load open —
    // the guard is against garbage, not against the store's own format.
    expect(sessionDetailPaneStore.open).toBe(true);

    localStorage.setItem(STORAGE_KEY, 'garbage');
    const reloaded = await loadStore();
    expect(reloaded.sessionDetailPaneStore.open).toBe(false);
  });
});
