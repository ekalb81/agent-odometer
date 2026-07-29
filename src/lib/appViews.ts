/** Canonical application navigation. Visual scenarios validate this registry. */
export const APP_VIEWS = [
  { id: 'all', label: 'All' },
  { id: 'codex', label: 'Codex' },
  { id: 'claude', label: 'Claude Code' },
  { id: 'instructions', label: 'Instructions' },
  { id: 'settings', label: 'Settings' },
] as const;

export type AppView = (typeof APP_VIEWS)[number]['id'];
