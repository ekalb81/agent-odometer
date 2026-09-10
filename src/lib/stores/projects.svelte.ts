import { resolveProjects } from '../ipc';
import type { ProjectInfo, SessionSummary } from '../types';

/**
 * Resolves each session's auto-computed `project_key` to its effective
 * project — the local alias/merge overrides applied, alongside every other
 * session currently grouped under it (#41).
 *
 * The backend owns the label: a session's `project_label` field is only the
 * default before any override, so the grid and grouping join
 * through this map rather than reading `project_label` directly. Mirrors
 * `workingDirectoryStore`'s fetch-once-until-refreshed shape — overrides
 * change rarely, and this re-renders on every live session update.
 */
export function createProjectStore() {
  let byKey = $state<ReadonlyMap<string, ProjectInfo>>(new Map());
  let bySession = $state<ReadonlyMap<string, ProjectInfo>>(new Map());
  let loaded = $state(false);
  let error = $state<string | null>(null);
  let inFlight: Promise<void> | null = null;
  let epoch = 0;
  let scanComplete = false;

  function load(): Promise<void> {
    if (inFlight) return inFlight;
    if (loaded) return Promise.resolve();
    return fetchProjects();
  }

  function fetchProjects(): Promise<void> {
    const request = ++epoch;
    loaded = false;
    error = null;
    const pending = resolveProjects()
      .then((entries) => {
        if (request !== epoch) return;
        const map = new Map<string, ProjectInfo>();
        const overrides = new Map<string, ProjectInfo>();
        for (const entry of entries) {
          for (const memberKey of entry.member_keys) {
            map.set(memberKey, entry);
          }
          for (const key of entry.overridden_session_keys ?? []) overrides.set(key, entry);
        }
        byKey = map;
        bySession = overrides;
      })
      .catch((cause: unknown) => {
        if (request === epoch) error = String(cause);
      })
      .finally(() => {
        if (request === epoch) {
          loaded = true;
          inFlight = null;
        }
      });
    inFlight = pending;
    return pending;
  }

  /** Forces a re-resolve, for after an alias/merge/split edit. */
  function refresh(): Promise<void> {
    return fetchProjects();
  }

  function observeScanComplete(complete: boolean): Promise<void> {
    const finished = complete && !scanComplete;
    scanComplete = complete;
    // A completed scan supersedes any earlier partial snapshot.
    return finished ? refresh() : Promise.resolve();
  }

  return {
    get loaded() {
      return loaded;
    },
    get error() {
      return error;
    },
    forSession(session: Pick<SessionSummary, 'storage_id' | 'project_key'>): ProjectInfo | undefined {
      return bySession.get(session.storage_id) ?? (session.project_key ? byKey.get(session.project_key) : undefined);
    },
    hasOverride(sessionKey: string): boolean {
      return bySession.has(sessionKey);
    },
    /** Undefined until the fetch resolves, or for a session with no project. */
    info(projectKey: string | null | undefined): ProjectInfo | undefined {
      return projectKey ? byKey.get(projectKey) : undefined;
    },
    /** Every resolved project, for a management UI. */
    all(): ProjectInfo[] {
      return [...new Set(byKey.values())];
    },
    load,
    refresh,
    refreshIfIdle(): Promise<void> {
      return inFlight ?? fetchProjects();
    },
    observeScanComplete,
  };
}

export const projectStore = createProjectStore();
