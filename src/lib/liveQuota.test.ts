import { describe, expect, it } from 'vitest';
import { liveQuotaTrayLabel, liveQuotaUnavailable, type LiveQuotaStatus } from './liveQuota';
import type { QuotaWindow } from './types';

const window: QuotaWindow = { kind: 'burst', unit: 'percent', window_minutes: 300, used: 80, remaining: 20, limit: 100,
  unlimited: false, resets_at: null, window_started_at: null, window_started_at_estimated: false,
  observed_at: '2026-01-01T00:00:00Z', confidence: 'high', stale: false, unavailable: null, forecast: null };
function status(): LiveQuotaStatus {
  return { busy: false, configuration_error: null, accounts: [{ provider: 'codex',
    consent: { account_id: 'synthetic-account', label: 'Work', enabled: true, consented_at: '2026-01-01T00:00:00Z' },
    observed_at: window.observed_at, ordinary_usage_allowed: null, unavailable: null,
    buckets: [{ limit_id: 'standard', limit_name: null, spend_control_reached: null, snapshot: { provider: 'codex', provenance: 'live_provider', windows: [{ ...window }], unavailable: null } }],
  }] };
}
describe('account-scoped tray quota', () => {
  it.each(['stale_observation', 'clock_skew'])('does not present %s permission as current', (reason) => {
    const value = status();
    value.accounts[0].ordinary_usage_allowed = false;
    value.accounts[0].unavailable = reason;
    expect(liveQuotaTrayLabel(value)).toBe('Codex Work · live unavailable');
    expect(liveQuotaUnavailable(reason)).toContain('current usage permission is unavailable');
  });
  it('keeps source/account/bucket attribution and never substitutes an expired or changed account', () => {
    const value = status();
    expect(liveQuotaTrayLabel(value)).toBe('Codex Work · standard 20% left · live');
    value.accounts[0].buckets[0].snapshot.windows[0].stale = true;
    expect(liveQuotaTrayLabel(value)).toContain('(stale)');
    value.accounts[0].ordinary_usage_allowed = false;
    expect(liveQuotaTrayLabel(value)).toContain('ordinary usage blocked');
    value.accounts[0].unavailable = 'account_changed';
    expect(liveQuotaTrayLabel(value)).toBe('Codex Work · live unavailable');
    value.accounts[0].consent.enabled = false;
    expect(liveQuotaTrayLabel(value)).toBeNull();
  });
});
