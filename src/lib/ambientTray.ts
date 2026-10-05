import type { QuotaSnapshot, QuotaWindow } from './types';
import type { LiveQuotaStatus } from './liveQuota';
import { quotaWindowLabel, resetCountdown } from './subscriptionUsage';

function windowText(provider: string, window: QuotaWindow, now: number, source: string): string {
  const age = Math.max(0, Math.floor((now - Date.parse(window.observed_at)) / 60_000));
  const reset = resetCountdown(window.resets_at, now) ?? 'unknown';
  const stale = window.stale || Date.parse(window.observed_at) > now || !Number.isFinite(age);
  const pace = !stale && window.forecast ? `${window.forecast.pace_per_hour.toFixed(1)}%/h` : 'unknown';
  const ageLabel = Date.parse(window.observed_at) > now ? 'future timestamp' : Number.isFinite(age) ? `${age}m ago` : 'unknown';
  return `${provider} ${quotaWindowLabel(window)} · ${window.remaining?.toFixed(0) ?? '?'}% left${stale ? ' (stale)' : ''} · reset ${reset} · pace ${pace} · ${source} as of ${ageLabel}`;
}

/** An all-provider tray cannot silently select an account/provider. Exactly
 * one enabled live account is required for an explicit Codex selection. */
export function ambientQuotaLabel(provider: string, snapshots: QuotaSnapshot[], live: LiveQuotaStatus, now: number): string {
  if (provider === 'all') return 'All providers · select a provider for scoped quota';
  const label = provider === 'codex' ? 'Codex' : provider === 'claude_code' ? 'Claude Code' : 'Gemini CLI';
  if (provider === 'codex') {
    const accounts = live.accounts.filter(account => account.consent.enabled);
    if (accounts.length > 1) return 'Codex live unavailable · multiple approved accounts';
    if (accounts.length === 1) {
      const account = accounts[0];
      if (live.configuration_error || account.unavailable) return 'Codex live unavailable';
      if (account.ordinary_usage_allowed === false) return 'Codex live · ordinary usage blocked';
      // Multiple buckets are different provider quota limits, never summed.
      const bucket = account.buckets.find(value => value.spend_control_reached !== true);
      const window = bucket?.snapshot.windows.find(value => value.unit === 'percent' && !value.unavailable && value.remaining != null);
      const safe = (value: string | null | undefined) => value && value.length <= 64 && /^[a-zA-Z0-9 ._:/-]+$/.test(value) ? value : null;
      const id = safe(bucket?.limit_id), name = safe(bucket?.limit_name);
      if (!id) return 'Codex live quota unavailable · limit identity unavailable';
      const identity = name && name !== id ? `${name} [${id}]` : id;
      return window ? windowText(`Codex · approved account · limit ${identity}`, window, now, 'live') : 'Codex live unavailable';
    }
  }
  const snapshot = snapshots.find(value => value.provider === provider);
  const window = snapshot?.windows.find(value => value.unit === 'percent' && !value.unavailable && value.remaining != null);
  return window ? windowText(label, window, now, 'unattributed transcript') : `${label} transcript quota unavailable`;
}
