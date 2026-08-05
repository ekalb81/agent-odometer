import { resolveProjects } from '../ipc';
import type { ProjectInfo } from '../types';

/**
 * Resolves each session's auto-computed `project_key` to its effective
 * project — the local alias/merge overrides applied, alongside every other
 * session currently grouped under it (#41).
 *
 * The backend owns the label: a session's `project_label` field is only the
 * default before any override, so the grid, grouping, and export all join
 * through this map rather than reading `project_label` directly. Mirrors
 * `workingDirectoryStore`'s fetch-once-until-refreshed shape — overrides
 * change rarely, and this re-renders on every live session update.
 */
function createProjectStore() {
  let byKey = $state<ReadonlyMap<string, ProjectInfo>>(new Map());
  let loaded = $state(false);
  let inFlight: Promise<void> | null = null;

  function load(): Promise<void> {
    if (inFlight) return inFlight;
    inFlight = resolveProjects()
      .then((entries) => {
        const map = new Map<string, ProjectInfo>();
        for (const entry of entries) {
          for (const memberKey of entry.member_keys) {
            map.set(memberKey, entry);
          }
        }
        byKey = map;
        loaded = true;
      })
      .catch(() => {
        // A failed resolve leaves every row on its auto-computed
        // `project_label`, which is still correct — just without any local
        // alias/merge applied.
        loaded = true;
      })
      .finally(() => {
        inFlight = null;
      });
    return inFlight;
  }

  /** Forces a re-resolve, for after an alias/merge/split edit. */
  function refresh(): Promise<void> {
    if (inFlight) return inFlight;
    loaded = false;
    return load();
  }

  return {
    get loaded() {
      return loaded;
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
  };
}

export const projectStore = createProjectStore();
