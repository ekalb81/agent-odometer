import { beforeEach, describe, expect, it, vi } from 'vitest';
import { createProjectStore } from './projects.svelte';
import type { ProjectInfo } from '../types';

const { resolveProjects } = vi.hoisted(() => ({ resolveProjects: vi.fn() }));
vi.mock('../ipc', () => ({ resolveProjects }));
const project = (key: string, overrides: string[] = []): ProjectInfo => ({
  project_key: key, label: key, provenance: 'repository_root', member_keys: [key],
  session_count: 1, overridden_session_keys: overrides,
});
function deferred() {
  let resolve!: (value: ProjectInfo[]) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<ProjectInfo[]>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
beforeEach(() => resolveProjects.mockReset());

describe('project assignment lookup', () => {
  it('refreshes once when startup finishes and rejects the earlier partial mapping', async () => {
    const store = createProjectStore();
    const partial = deferred();
    resolveProjects.mockReturnValueOnce(partial.promise)
      .mockResolvedValue([project('complete', ['session'])]);
    const startup = store.load();
    await store.observeScanComplete(false);
    await store.observeScanComplete(true);
    await store.observeScanComplete(true);
    expect(resolveProjects).toHaveBeenCalledTimes(2);
    partial.resolve([]);
    await startup;
    expect(store.hasOverride('session')).toBe(true);
    await store.observeScanComplete(false);
    await store.observeScanComplete(true);
    expect(resolveProjects).toHaveBeenCalledTimes(3);
  });

  it('refreshes an already complete startup snapshot and deduplicates editor requests', async () => {
    const store = createProjectStore();
    resolveProjects.mockResolvedValueOnce([]);
    await store.load();
    const fresh = deferred();
    resolveProjects.mockReturnValue(fresh.promise);
    const completion = store.observeScanComplete(true);
    expect(store.refreshIfIdle()).toBe(completion);
    expect(store.refreshIfIdle()).toBe(completion);
    fresh.resolve([project('arrived')]);
    await completion;
    expect(store.all()).toHaveLength(1);
    expect(resolveProjects).toHaveBeenCalledTimes(2);
  });

  it('joins only the overridden durable session and restores the detected project after undo', async () => {
    const store = createProjectStore();
    resolveProjects.mockResolvedValue([project('original'), project('target', ['codex:thread:same'])]);
    await store.load();
    expect(store.forSession({ storage_id: 'codex:thread:same', project_key: 'original' })?.label).toBe('target');
    expect(store.forSession({ storage_id: 'claude_code:session:same', project_key: 'original' })?.label).toBe('original');
    expect(store.forSession({ storage_id: 'codex:thread:same', project_key: null })?.label).toBe('target');
    resolveProjects.mockResolvedValue([project('original')]);
    await store.refresh();
    expect(store.hasOverride('codex:thread:same')).toBe(false);
    expect(store.forSession({ storage_id: 'codex:thread:same', project_key: 'original' })?.label).toBe('original');
  });

  it('accepts older payloads and deduplicates ordinary loads', async () => {
    const store = createProjectStore();
    const old = project('old');
    delete old.overridden_session_keys;
    const request = deferred();
    resolveProjects.mockReturnValue(request.promise);
    const first = store.load();
    expect(store.load()).toBe(first);
    request.resolve([old]);
    await first;
    await store.load();
    expect(resolveProjects).toHaveBeenCalledTimes(1);
    expect(store.info('old')).toEqual(old);
  });

  it.each(['success', 'failure'])('rejects a superseded %s after an edit refresh', async (outcome) => {
    const store = createProjectStore();
    const old = deferred();
    const fresh = deferred();
    resolveProjects.mockReturnValueOnce(old.promise).mockReturnValueOnce(fresh.promise);
    const initial = store.load();
    const replacement = store.refresh();
    fresh.resolve([project('target', ['session'])]);
    await replacement;
    if (outcome === 'success') old.resolve([project('old')]);
    else old.reject('obsolete failure');
    await initial;
    expect(store.forSession({ storage_id: 'session', project_key: 'old' })?.label).toBe('target');
    expect(store.error).toBeNull();
    expect(store.loaded).toBe(true);
  });

  it('reports current failures and allows an explicit retry', async () => {
    const store = createProjectStore();
    resolveProjects.mockRejectedValueOnce('history preparing').mockResolvedValueOnce([project('ready')]);
    await store.load();
    expect(store.error).toBe('history preparing');
    await store.refresh();
    expect(store.error).toBeNull();
    expect(store.info('ready')?.label).toBe('ready');
  });
});
