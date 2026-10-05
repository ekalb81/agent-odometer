import { writable } from 'svelte/store';
import { getAttentionStatus, setAttentionPreferences } from '../ipc';
import type { AttentionPreferences, AttentionSnapshot } from '../types';

export const attention = writable<AttentionSnapshot | null>(null);
export const attentionError = writable(false);
export const attentionLabels = {
  turn_started: 'Turn started', input_requested: 'Input requested', tool_completed: 'Tool completed',
  tool_failed: 'Tool failed', turn_completed: 'Turn completed', turn_interrupted: 'Turn interrupted or rolled back',
} as const;
let sequence = 0;
export async function refreshAttention(): Promise<void> {
  const request = ++sequence;
  try {
    const result = await getAttentionStatus();
    if (request !== sequence) return;
    attention.set(result); attentionError.set(false);
  } catch {
    if (request !== sequence) return;
    attention.set(null); attentionError.set(true);
  }
}
export async function saveAttention(preferences: AttentionPreferences): Promise<void> {
  sequence++;
  try { await setAttentionPreferences(preferences); }
  finally { sequence++; }
  await refreshAttention();
}
