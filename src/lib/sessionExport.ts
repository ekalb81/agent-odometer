import type { Harness, SessionSummary } from './types';

export type SessionExportFormat = 'csv' | 'json';

type HierarchicalSession = Pick<SessionSummary, 'id' | 'parent_thread_id'>;

export function collectSessionExportTree<T extends HierarchicalSession>(
  sessions: Iterable<T>,
  rootId: string,
  includeDescendants: boolean,
): T[] {
  const all = [...sessions];
  const byId = new Map(all.map((session) => [session.id, session]));
  const root = byId.get(rootId);
  if (!root) return [];
  if (!includeDescendants) return [root];

  const children = new Map<string, T[]>();
  for (const session of all) {
    if (!session.parent_thread_id || !byId.has(session.parent_thread_id)) continue;
    const siblings = children.get(session.parent_thread_id);
    if (siblings) siblings.push(session);
    else children.set(session.parent_thread_id, [session]);
  }

  const result: T[] = [];
  const seen = new Set<string>();
  const append = (session: T) => {
    if (seen.has(session.id)) return;
    seen.add(session.id);
    result.push(session);
    for (const child of children.get(session.id) ?? []) append(child);
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
