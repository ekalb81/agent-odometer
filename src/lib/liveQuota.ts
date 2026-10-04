import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { QuotaSnapshot } from './types';

export interface DiscoveredQuotaAccount { account_id: string; plan_type: string | null }
export interface LiveQuotaAccountView {
  provider: 'codex';
  consent: { account_id: string; label: string; consented_at: string; enabled: boolean };
  observed_at: string | null;
  ordinary_usage_allowed: boolean | null;
  buckets: { limit_id: string; limit_name: string | null; spend_control_reached: boolean | null; snapshot: QuotaSnapshot }[];
  unavailable: string | null;
}
export interface LiveQuotaStatus {
  accounts: LiveQuotaAccountView[];
  busy: boolean;
  configuration_error: string | null;
}
export const onLiveQuotaUpdated = (callback: () => void) => listen('live-quota-updated', callback);
export const getLiveQuotaStatus = () => invoke<LiveQuotaStatus>('get_live_quota_status');
export const identifyQuotaAccount = () => invoke<DiscoveredQuotaAccount>('identify_quota_account');
export const approveQuotaAccount = (accountId: string, label: string) => invoke<void>('approve_quota_account', { accountId, label });
export const changeQuotaAccount = (accountId: string, enabled: boolean, revoke = false) => invoke<void>('change_quota_account', { accountId, enabled, revoke });

export function liveQuotaUnavailable(reason: string | null): string {
  const labels: Record<string, string> = {
    disabled: 'Polling paused', no_observation: 'No current observation', offline: 'Codex CLI unavailable or offline',
    launch_failed: 'Codex CLI could not start', timeout: 'Provider request timed out', auth_expired: 'Sign-in unavailable or expired',
    rate_limited: 'Provider limited polling; retrying later', provider_outage: 'Provider unavailable; retrying later',
    unsupported: 'Installed CLI does not expose a supported account identity', account_changed: 'Signed-in account changed; identify and approve it separately',
    protocol_mismatch: 'Installed CLI protocol is unsupported', invalid_response: 'Provider response could not be validated',
    stale_observation: 'Live observation is stale; current usage permission is unavailable',
    clock_skew: 'Clock differs from the fetch time; current usage permission is unavailable',
  };
  return reason ? (labels[reason] ?? 'Quota unavailable') : '';
}

/** Same account-scoped observations as the dashboard. Never substitute another
 * account or transcript reading when the enabled live source is unavailable. */
export function liveQuotaTrayLabel(status: LiveQuotaStatus): string | null {
  const account = status.accounts.find((account) => account.consent.enabled);
  if (!account) return null;
  if (status.configuration_error || account.unavailable) return `Codex ${account.consent.label} · live unavailable`;
  if (account.ordinary_usage_allowed === false) return `Codex ${account.consent.label} · ordinary usage blocked · live`;
  for (const bucket of account.buckets) {
    if (bucket.spend_control_reached === true) continue;
    const window = bucket.snapshot.windows.find((window) => window.unit === 'percent' && window.unavailable === null && window.remaining !== null);
    if (window?.remaining != null) return `Codex ${account.consent.label} · ${bucket.limit_name ?? bucket.limit_id} ${window.remaining.toFixed(0)}% left${window.stale ? ' (stale)' : ''} · live`;
  }
  return `Codex ${account.consent.label} · live unavailable`;
}
