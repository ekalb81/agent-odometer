import { writable } from 'svelte/store';
import { checkAmbientAlerts, getAmbientStatus } from '../ipc';
import type { AmbientSnapshot } from '../types';
export const ambient = writable<AmbientSnapshot | null>(null);
let generation = 0;
let collecting: Promise<void> | null = null;
export function invalidateAmbient(): void { generation++; ambient.set(null); }
export function refreshAmbient(): Promise<void> {
  if (collecting) return collecting;
  const request = ++generation;
  collecting = (async () => {
    try { const result = await checkAmbientAlerts(); if (request === generation) ambient.set(result); }
    catch { if (request === generation) ambient.set(null); }
  })().finally(() => { collecting = null; });
  return collecting;
}
export async function readAmbient(): Promise<void> {
  const request = generation, hadCollector = collecting !== null;
  try {
    const result = await getAmbientStatus();
    if (request === generation && !hadCollector && !collecting) ambient.set(result);
  } catch { if (request === generation && !hadCollector && !collecting) ambient.set(null); }
}
export const ambientLabels: Record<string, string> = {
  budget_crossed: 'Soft budget crossed', provider_incident: 'Public provider incident observed',
  stale_quota: 'Transcript quota observation expired', retained_sources_missing: 'Stored history has missing source transcripts',
  turn_started: 'Turn started', input_requested: 'Input requested', tool_completed: 'Tool completed',
  tool_failed: 'Tool failed', turn_completed: 'Turn completed', turn_interrupted: 'Turn interrupted or rolled back',
};
