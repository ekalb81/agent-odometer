import { repositoryLabel } from './sessionProjection';
import type { SessionSummary } from './types';

/** Format a session start in the viewer's local wall-clock time. Optional
 * locale/time-zone arguments keep DST behavior deterministic in tests. */
export function formatStartedLocal(
  startedMs: number,
  locales?: Intl.LocalesArgument,
  timeZone?: string,
): string {
  return new Intl.DateTimeFormat(locales, {
    dateStyle: 'medium',
    timeStyle: 'medium',
    timeZone,
  }).format(new Date(startedMs));
}

/** Zero is the aggregate wire representation for a category that may be
 * measured as zero, omitted, or not applicable. Keep that ambiguity visible. */
export function formatTokenCategory(value: number, locales?: Intl.LocalesArgument): string {
  return value === 0 ? '—' : new Intl.NumberFormat(locales).format(value);
}

export interface ModelProviderVisual {
  key: 'openai' | 'anthropic' | 'google' | 'xai' | 'other';
  label: string;
  tint: string;
}

/** Map provider metadata to a bounded visual palette. Harness fallback keeps
 * historical summaries useful while unknown providers stay visibly neutral. */
export function modelProviderVisual(
  session: Pick<SessionSummary, 'harness' | 'model_provider'>,
): ModelProviderVisual {
  const raw = session.model_provider?.trim();
  const normalized = (raw || (session.harness === 'codex' ? 'openai' : 'anthropic')).toLowerCase();
  if (normalized === 'openai' || normalized === 'codex') {
    return { key: 'openai', label: 'OpenAI', tint: 'var(--provider-openai-row)' };
  }
  if (normalized === 'anthropic' || normalized === 'claude') {
    return { key: 'anthropic', label: 'Anthropic', tint: 'var(--provider-anthropic-row)' };
  }
  if (normalized === 'google' || normalized === 'gemini' || normalized === 'vertex') {
    return { key: 'google', label: 'Google', tint: 'var(--provider-google-row)' };
  }
  if (normalized === 'xai' || normalized === 'grok') {
    return { key: 'xai', label: 'xAI', tint: 'var(--provider-xai-row)' };
  }
  return { key: 'other', label: raw?.slice(0, 40) || 'Other provider', tint: 'var(--provider-other-row)' };
}

export function groupSessionsByRepository<T extends Pick<
  SessionSummary,
  'id' | 'parent_thread_id' | 'working_directory'
>>(sessions: T[]): { label: string; sessions: T[] }[] {
  const groups = new Map<string, { label: string; sessions: T[] }>();
  const byId = new Map(sessions.map((session) => [session.id, session]));

  for (const session of sessions) {
    // A visible subagent stays with its parent even if its transcript records
    // another (or no) project folder. This preserves the nested row contract.
    let anchor = session;
    const seen = new Set<string>();
    while (anchor.parent_thread_id && !seen.has(anchor.id)) {
      seen.add(anchor.id);
      const parent = byId.get(anchor.parent_thread_id);
      if (!parent) break;
      anchor = parent;
    }
    const workingDirectory = anchor.working_directory;
    const normalized = workingDirectory?.replace(/\\/g, '/').replace(/\/+$/, '') ?? '';
    const windowsStyle = Boolean(workingDirectory && (/\\/.test(workingDirectory) || /^[A-Za-z]:[\\/]/.test(workingDirectory)));
    // The full normalized path is an internal identity only; display remains
    // the privacy-safe final segment. Prefix missing paths to avoid collisions
    // with a real directory whose name happens to match the fallback label.
    const key = workingDirectory
      ? `path:${windowsStyle ? normalized.toLowerCase() : normalized}`
      : 'missing:';
    const group = groups.get(key);
    if (group) group.sessions.push(session);
    else groups.set(key, {
      label: repositoryLabel(anchor) ?? 'No repository recorded',
      sessions: [session],
    });
  }

  return [...groups.values()];
}
