import { getOrganizationSummaries, listOrganizationTags, getOrganizationRecoveryState } from '../ipc';
import type { OrganizationSummary } from '../types';

let summaries = $state<Record<string, OrganizationSummary>>({});
let busy = $state(false);
let error = $state<string | null>(null);
let tagLabels = $state<string[]>([]);
let recoveryUnrestored = $state(false);
let generation = 0;
let editSequence = 0;
const edits = new Map<string, number>();

export const organizationStore = {
  get summaries() { return summaries; },
  get busy() { return busy; },
  get error() { return error; },
  get tagLabels() { return tagLabels; },
  get recoveryUnrestored() { return recoveryUnrestored; },
  invalidate(reason: string) { generation++; busy = false; summaries = {}; tagLabels = []; edits.clear(); error = reason; },
  update(summary: OrganizationSummary) {
    edits.set(summary.identity.session_key, ++editSequence);
    summaries = { ...summaries, [summary.identity.session_key]: summary };
    tagLabels = [...new Set([...tagLabels, ...summary.tags])];
  },
  async load(keys: string[]) {
    const request = ++generation;
    const startedBeforeEdit = editSequence;
    busy = true; error = null;
    try {
      const next: Record<string, OrganizationSummary> = {};
      // Each IPC/read is bounded; no private note bodies enter this store.
      for (let start = 0; start < keys.length; start += 1000) {
        const rows = await getOrganizationSummaries(keys.slice(start, start + 1000));
        if (request !== generation) return;
        for (const row of rows) next[row.identity.session_key] = row;
      }
      const [labels, recovered] = await Promise.all([listOrganizationTags(), getOrganizationRecoveryState()]);
      if (request === generation) {
        for (const key of keys) {
          if ((edits.get(key) ?? 0) > startedBeforeEdit && summaries[key]) next[key] = summaries[key];
        }
        summaries = next; tagLabels = [...new Set([...labels, ...Object.values(next).flatMap(row => row.tags)])];
        recoveryUnrestored = recovered;
      }
    } catch (cause) {
      if (request === generation) { summaries = {}; error = String(cause); }
    } finally { if (request === generation) busy = false; }
  },
};
