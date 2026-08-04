import type { ProviderDescriptor } from './types';

export interface AppViewEntry {
  id: string;
  label: string;
}

/** Any provider id, or one of the static pseudo-views. Open (matches the
 * backend's open ProviderId) rather than a closed literal union. */
export type AppView = string;

/** Claude Code predates descriptor-driven navigation with a shorter tab id
 * ('claude') than its provider id ('claude_code'); kept stable here so
 * existing visual-regression baselines and the tab-click/filter-scope
 * plumbing don't change. Any provider without an entry uses its own id as
 * the tab id directly — the mapping only exists for this one legacy case. */
const LEGACY_TAB_IDS: Record<string, string> = { claude_code: 'claude' };
const PROVIDER_ID_FOR_TAB: Record<string, string> = { claude: 'claude_code' };

export function tabIdForProvider(providerId: string): string {
  return LEGACY_TAB_IDS[providerId] ?? providerId;
}

export function providerIdForTab(tabId: string): string {
  return PROVIDER_ID_FOR_TAB[tabId] ?? tabId;
}

/** Canonical application navigation: 'All' first, one tab per registered
 * provider descriptor (in the order `list_providers` returns them), then the
 * static Instructions/Settings views. Visual scenarios validate this
 * registry against the dev-mock `list_providers` fixture. */
export function appViews(descriptors: ProviderDescriptor[]): AppViewEntry[] {
  return [
    { id: 'all', label: 'All' },
    ...descriptors.map((descriptor) => ({ id: tabIdForProvider(descriptor.id), label: descriptor.display_name })),
    { id: 'instructions', label: 'Instructions' },
    { id: 'settings', label: 'Settings' },
  ];
}
