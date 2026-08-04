import type { Config } from './types';

export const DEFENDER_EXCLUSION_RECEIPT_VERSION = 1;

export type DefenderReceiptStatus = 'never_requested' | 'current' | 'partial' | 'stale';

export function normalizeDefenderRoot(path: string): string {
  let normalized = path.replaceAll('/', '\\');
  while (normalized.length > 3 && normalized.endsWith('\\')) {
    normalized = normalized.slice(0, -1);
  }
  return normalized.toLocaleLowerCase('en-US');
}

export function configuredDefenderRoots(config: Config): string[] {
  return [...new Set([
    ...config.session_roots,
    ...config.archive_roots,
    ...config.claude_session_roots,
  ].filter(Boolean).map(normalizeDefenderRoot))].sort();
}

export function defenderReceiptStatus(config: Config): DefenderReceiptStatus {
  const receipt = config.defender_exclusion_receipt;
  if (!receipt) return 'never_requested';
  if (receipt.version !== DEFENDER_EXCLUSION_RECEIPT_VERSION) return 'stale';

  const currentRoots = configuredDefenderRoots(config);
  const receiptRoots = [...new Set(
    receipt.configured_roots.filter(Boolean).map(normalizeDefenderRoot),
  )].sort();
  const sameConfiguration = currentRoots.length === receiptRoots.length
    && currentRoots.every((root, index) => root === receiptRoots[index]);
  if (!sameConfiguration) return 'stale';

  const verifiedRoots = new Set(
    receipt.verified_roots.filter(Boolean).map(normalizeDefenderRoot),
  );
  return currentRoots.every((root) => verifiedRoots.has(root)) ? 'current' : 'partial';
}

/** The fixture marker is installed only by the browser-only dev-mock branch. */
export function isWindowsDefenderSurface(
  userAgent = navigator.userAgent,
  visualScenario = document.documentElement.dataset.visualScenario ?? null,
): boolean {
  return userAgent.includes('Windows')
    || visualScenario === 'defender-slow'
    || visualScenario === 'defender-error';
}
