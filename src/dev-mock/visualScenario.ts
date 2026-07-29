/**
 * The small, deliberately finite set of browser-only fixture states used by
 * visual regression tests. Keep parsing here (rather than in a test) so a
 * malformed URL cannot accidentally select an undocumented fixture.
 */
export const VISUAL_SCENARIOS = [
  'default',
  'sessions-empty',
  'sessions-scanning',
  'sessions-availability-fallback',
  'instructions-empty',
  'instructions-loading',
  'instructions-error',
  'instructions-content-error',
  'settings-save-error',
  'defender-slow',
  'defender-error',
  'updater-available',
  'updater-installing',
  'updater-error',
] as const;

export type VisualScenario = (typeof VISUAL_SCENARIOS)[number];

const visualScenarioSet = new Set<string>(VISUAL_SCENARIOS);

export interface VisualScenarioSelection {
  scenario: VisualScenario;
  warning: string | null;
}

/**
 * Converts the optional `?visualScenario=` value into a known scenario.
 * Unknown values fail closed to the normal deterministic fixture data.
 */
export function selectVisualScenario(search: string): VisualScenarioSelection {
  const requested = new URLSearchParams(search).get('visualScenario');
  if (!requested || requested === 'default') return { scenario: 'default', warning: null };
  if (visualScenarioSet.has(requested)) return { scenario: requested as VisualScenario, warning: null };
  return {
    scenario: 'default',
    warning: `[dev-mock] unknown visualScenario=${JSON.stringify(requested)}; using default`,
  };
}

export function isDefenderVisualScenario(scenario: VisualScenario | undefined): boolean {
  return scenario === 'defender-slow' || scenario === 'defender-error';
}

export function isUpdaterVisualScenario(scenario: VisualScenario): boolean {
  return scenario === 'updater-available' || scenario === 'updater-installing' || scenario === 'updater-error';
}
