import type { LiveQuotaStatus } from './liveQuota';
import type { LiveAccountBudget } from './types';

export interface LiveAlertScope { key: string; label: string; rule: Omit<LiveAccountBudget, 'id' | 'threshold_percent' | 'enabled'> }
/** Presentation choices only. Rust independently validates cached observations. */
export function liveAlertScopes(status: LiveQuotaStatus | null, now = Date.now()): LiveAlertScope[] {
  if (!status || status.configuration_error || status.busy) return [];
  const scopes: LiveAlertScope[] = [];
  for (const account of status.accounts) {
    const observed = Date.parse(account.observed_at ?? '');
    if (account.provider !== 'codex' || !account.consent.enabled || account.unavailable || !Number.isFinite(observed) || observed > now || now - observed > 600_000) continue;
    if (status.accounts.filter(a => a.consent.account_id === account.consent.account_id && a.consent.consented_at === account.consent.consented_at && a.consent.enabled).length !== 1) continue;
    for (const bucket of account.buckets) {
      if (account.buckets.filter(b => b.limit_id === bucket.limit_id).length !== 1 || bucket.snapshot.provider !== 'codex' || bucket.snapshot.provenance !== 'live_provider' || bucket.snapshot.unavailable) continue;
      for (const window of bucket.snapshot.windows) {
        if (window.unit !== 'percent' || window.stale || window.unavailable || window.observed_at !== account.observed_at || window.used === null || !Number.isFinite(window.used) || window.used < 0 || window.used > 100 || !window.window_minutes || (window.resets_at && Date.parse(window.resets_at) <= now)) continue;
        if (bucket.snapshot.windows.filter(w => w.unit === 'percent' && w.kind === window.kind && w.window_minutes === window.window_minutes).length !== 1) continue;
        const rule = { account_id: account.consent.account_id, consented_at: account.consent.consented_at, limit_id: bucket.limit_id, window_kind: window.kind, window_minutes: window.window_minutes };
        scopes.push({ key: JSON.stringify(rule), label: `${account.consent.label} · ${bucket.limit_name ?? bucket.limit_id} · ${window.kind} ${window.window_minutes} min`, rule });
      }
    }
  }
  return scopes;
}
