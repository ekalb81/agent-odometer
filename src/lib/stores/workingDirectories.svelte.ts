import { resolveWorkingDirectories } from '../ipc';
import type { WorkingDirectoryInfo } from '../types';

/**
 * Resolves session working directories to their repository, when they have
 * one.
 *
 * A working directory is not a repository by definition: scratch directories
 * have no repo at all, and their final path segment is often a one- or
 * two-character fragment. The backend walks the filesystem once per *distinct*
 * directory — roughly thirty times fewer than there are sessions — so this
 * fetches the whole map once rather than resolving per row.
 */
function createWorkingDirectoryStore() {
  let byDirectory = $state<ReadonlyMap<string, WorkingDirectoryInfo>>(new Map());
  let loaded = $state(false);
  let inFlight: Promise<void> | null = null;

  function load(): Promise<void> {
    // Deliberately fetch-once: repository membership changes rarely, and the
    // grid re-renders on every live session update.
    if (inFlight) return inFlight;
    inFlight = resolveWorkingDirectories()
      .then((entries) => {
        byDirectory = new Map(entries.map((entry) => [entry.directory, entry]));
        loaded = true;
      })
      .catch(() => {
        // A failed resolve leaves every row on its path fallback, which is
        // still correct — just without repository highlighting.
        loaded = true;
      })
      .finally(() => {
        inFlight = null;
      });
    return inFlight;
  }

  /** Forces a re-resolve, for when a directory may have become a repository. */
  function refresh(): Promise<void> {
    if (inFlight) return inFlight;
    loaded = false;
    return load();
  }

  return {
    get loaded() {
      return loaded;
    },
    /** Undefined until the fetch resolves, or for an unknown directory. */
    info(directory: string | null | undefined): WorkingDirectoryInfo | undefined {
      return directory ? byDirectory.get(directory) : undefined;
    },
    load,
    refresh,
  };
}

export const workingDirectoryStore = createWorkingDirectoryStore();
