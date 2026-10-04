import { getOrganizationSummaries } from '../ipc';
import type { OrganizationSummary } from '../types';

let summaries = $state<Record<string, OrganizationSummary>>({});
let busy = $state(false);
let error = $state<string | null>(null);
let generation = 0;

export const organizationStore = {
  get summaries() { return summaries; },
  get busy() { return busy; },
  get error() { return error; },
  update(summary: OrganizationSummary) {
    generation++; busy = false;
    summaries = { ...summaries, [summary.identity.session_key]: summary };
  },
  async load(keys: string[]) {
    const request = ++generation;
    busy = true; error = null;
    try {
      const next: Record<string, OrganizationSummary> = {};
      // Each IPC/read is bounded; no private note bodies enter this store.
      for (let start = 0; start < keys.length; start += 1000) {
        const rows = await getOrganizationSummaries(keys.slice(start, start + 1000));
        if (request !== generation) return;
        for (const row of rows) next[row.identity.session_key] = row;
      }
      if (request === generation) summaries = next;
    } catch (cause) {
      if (request === generation) { summaries = {}; error = String(cause); }
    } finally { if (request === generation) busy = false; }
  },
};
