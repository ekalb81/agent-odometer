import { listProviders } from '../ipc';
import type { ProviderDescriptor } from '../types';

/** Registered provider descriptors (id, display name, capability flags), for
 * descriptor-driven UI surfaces (tabs, labels, accents). Fetched once; safe
 * to read before that fetch resolves — `descriptors` starts empty and
 * `displayName`/`byId` fall back to the raw id so callers never need to
 * special-case the loading state. */
function createProvidersStore() {
  let descriptors = $state<ProviderDescriptor[]>([]);
  let inflight: Promise<void> | null = null;

  function init(): Promise<void> {
    if (inflight) return inflight;
    inflight = listProviders()
      .then((fetched) => {
        descriptors = fetched;
      })
      .catch((error) => {
        console.error('listProviders failed:', error);
      });
    return inflight;
  }

  function byId(id: string): ProviderDescriptor | undefined {
    return descriptors.find((descriptor) => descriptor.id === id);
  }

  function displayName(id: string): string {
    return byId(id)?.display_name ?? id;
  }

  return {
    get descriptors() {
      return descriptors;
    },
    init,
    byId,
    displayName,
  };
}

export const providersStore = createProvidersStore();
