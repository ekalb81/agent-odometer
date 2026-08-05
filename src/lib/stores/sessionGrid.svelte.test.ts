import { beforeEach, describe, expect, it, vi } from 'vitest';

const STORAGE_KEY = 'sessionGridPreferences.v1';

async function loadStore() {
  vi.resetModules();
  return import('./sessionGrid.svelte');
}

describe('sessionGridStore', () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it('starts with every default column visible in the declared order', async () => {
    const { DEFAULT_COLUMN_IDS, sessionGridStore } = await loadStore();

    expect(sessionGridStore.columnIds).toEqual([...DEFAULT_COLUMN_IDS]);
    expect(sessionGridStore.groupByRepository).toBe(false);
    expect(sessionGridStore.colorByModelProvider).toBe(false);
    expect(sessionGridStore.flattenSubagents).toBe(false);
  });

  // The per-run detail columns ship available-but-hidden: turning them all on
  // by default would widen the grid from nine columns to fourteen.
  it('offers the per-run detail columns without showing them by default', async () => {
    const { DEFAULT_COLUMN_IDS, SESSION_GRID_COLUMNS, sessionGridStore } = await loadStore();
    const available = SESSION_GRID_COLUMNS.map((column) => column.id);

    for (const id of ['duration', 'agent', 'parent', 'turns', 'tools'] as const) {
      expect(available).toContain(id);
      expect(DEFAULT_COLUMN_IDS).not.toContain(id);
    }

    sessionGridStore.setVisible('duration', true);
    expect(sessionGridStore.columnIds).toContain('duration');
  });

  it('persists visibility, ordering, and repository grouping', async () => {
    let module = await loadStore();
    module.sessionGridStore.setVisible('cached', false);
    module.sessionGridStore.move('repository', -1);
    module.sessionGridStore.setGroupByRepository(true);
    module.sessionGridStore.setColorByModelProvider(true);
    module.sessionGridStore.setFlattenSubagents(true);

    module = await loadStore();
    expect(module.sessionGridStore.columnIds).not.toContain('cached');
    expect(module.sessionGridStore.columnIds.indexOf('repository')).toBeLessThan(
      module.sessionGridStore.columnIds.indexOf('started'),
    );
    expect(module.sessionGridStore.groupByRepository).toBe(true);
    expect(module.sessionGridStore.colorByModelProvider).toBe(true);
    expect(module.sessionGridStore.flattenSubagents).toBe(true);
  });

  it('keeps the name column visible and restores defaults on reset', async () => {
    const { DEFAULT_COLUMN_IDS, sessionGridStore } = await loadStore();
    sessionGridStore.setVisible('name', false);
    sessionGridStore.setVisible('output', false);
    sessionGridStore.setGroupByRepository(true);
    sessionGridStore.setColorByModelProvider(true);

    expect(sessionGridStore.columnIds).toContain('name');
    sessionGridStore.reset();

    expect(sessionGridStore.columnIds).toEqual([...DEFAULT_COLUMN_IDS]);
    expect(sessionGridStore.groupByRepository).toBe(false);
    expect(sessionGridStore.colorByModelProvider).toBe(false);
    expect(JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}')).toEqual({
      columns: [...DEFAULT_COLUMN_IDS],
      groupByRepository: false,
      colorByModelProvider: false,
      flattenSubagents: false,
    });
  });

  it('falls back safely when persisted preferences are malformed', async () => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify({ columns: ['unknown'], groupByRepository: 'yes' }));

    const { DEFAULT_COLUMN_IDS, sessionGridStore } = await loadStore();
    expect(sessionGridStore.columnIds).toEqual([...DEFAULT_COLUMN_IDS]);
    expect(sessionGridStore.groupByRepository).toBe(false);
    expect(sessionGridStore.colorByModelProvider).toBe(false);
  });
});
