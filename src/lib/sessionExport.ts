import type { Harness, SessionSummary } from './types';

export type SessionExportFormat = 'csv' | 'json';

type HierarchicalSession = Pick<SessionSummary, 'id' | 'storage_id' | 'harness' | 'parent_thread_id'>;

export function collectSessionExportTree<T extends HierarchicalSession>(
  sessions: Iterable<T>,
  rootId: string,
  includeDescendants: boolean,
): T[] {
  const all = [...sessions];
  const byStorageId = new Map(all.map((session) => [session.storage_id, session]));
  const providerToStorageIds = new Map<string, string[]>();
  for (const session of all) {
    const key = `${session.harness}\0${session.id}`;
    const ids = providerToStorageIds.get(key);
    if (ids) ids.push(session.storage_id);
    else providerToStorageIds.set(key, [session.storage_id]);
  }
  const root = byStorageId.get(rootId);
  if (!root) return [];
  if (!includeDescendants) return [root];

  const children = new Map<string, T[]>();
  for (const session of all) {
    if (!session.parent_thread_id) continue;
    const parentIds = providerToStorageIds.get(`${session.harness}\0${session.parent_thread_id}`);
    const parentStorageId = parentIds?.length === 1 ? parentIds[0] : null;
    if (!parentStorageId || !byStorageId.has(parentStorageId)) continue;
    const siblings = children.get(parentStorageId);
    if (siblings) siblings.push(session);
    else children.set(parentStorageId, [session]);
  }

  const result: T[] = [];
  const seen = new Set<string>();
  const append = (session: T) => {
    if (seen.has(session.storage_id)) return;
    seen.add(session.storage_id);
    result.push(session);
    for (const child of children.get(session.storage_id) ?? []) append(child);
  };
  append(root);
  return result;
}

export function sessionExportContent(
  rows: Record<string, string | number | boolean | null>[],
  format: SessionExportFormat,
  toCsv: (rows: Record<string, string | number | boolean | null>[]) => string,
): string {
  return format === 'json' ? `${JSON.stringify(rows, null, 2)}\n` : toCsv(rows);
}

export function sessionExportFileName(
  harness: Harness,
  sessionId: string,
  descendantCount: number,
  format: SessionExportFormat,
): string {
  const suffix = descendantCount > 0 ? `-with-${descendantCount}-children` : '';
  return `odometer-${harness}-${sessionId.slice(0, 8)}${suffix}.${format}`;
}
