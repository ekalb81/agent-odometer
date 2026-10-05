import { describe, it, expect } from 'vitest';
import { liveAlertScopes } from './liveAccountAlerts';
import type { LiveQuotaStatus } from './liveQuota';
const now = Date.parse('2026-10-04T12:00:00Z');
export function fixture(): LiveQuotaStatus {
  const stamp = new Date(now).toISOString();
  return { busy: false, configuration_error: null, accounts: [{ provider: 'codex', consent: { account_id: 'synthetic-account', consented_at: '2026-10-01T12:00:00Z', enabled: true, label: 'Synthetic account' }, observed_at: stamp, ordinary_usage_allowed: true, unavailable: null, buckets: [{ limit_id: 'model-limit', limit_name: 'Model limit', spend_control_reached: false, snapshot: { provider: 'codex', provenance: 'live_provider', unavailable: null, windows: [{ kind: 'burst', unit: 'percent', window_minutes: 300, used: 85, remaining: 15, limit: 100, unlimited: false, resets_at: new Date(now + 3600000).toISOString(), window_started_at: null, window_started_at_estimated: false, window_start_basis: 'unknown', observed_at: stamp, confidence: 'medium', stale: false, unavailable: null, forecast: null }] } }] }] };
}
describe('cached live account alert choices', () => {
  it('pins exact consent, bucket and duration without deriving account identity from usage', () => {
    const [scope] = liveAlertScopes(fixture(), now);
    expect(scope.rule).toEqual({ account_id: 'synthetic-account', consented_at: '2026-10-01T12:00:00Z', limit_id: 'model-limit', window_kind: 'burst', window_minutes: 300 });
    expect(scope.label).toContain('Model limit');
  });
  it('rejects ambiguity, revoked consent, expired and future observations and transcript provenance', () => {
    const mutations: ((status: LiveQuotaStatus) => void)[] = [
      s => { s.accounts[0].consent.enabled = false; },
      s => { s.accounts.push(structuredClone(s.accounts[0])); },
      s => { s.accounts[0].buckets.push(structuredClone(s.accounts[0].buckets[0])); },
      s => { s.accounts[0].buckets[0].snapshot.windows.push(structuredClone(s.accounts[0].buckets[0].snapshot.windows[0])); },
      s => { s.accounts[0].observed_at = new Date(now + 1).toISOString(); },
      s => { s.accounts[0].observed_at = new Date(now - 600001).toISOString(); },
      s => { s.accounts[0].buckets[0].snapshot.provenance = 'transcript_derived'; },
      s => { s.accounts[0].buckets[0].snapshot.windows[0].resets_at = new Date(now).toISOString(); },
      s => { s.accounts[0].buckets[0].snapshot.windows[0].used = NaN; },
    ];
    for (const mutate of mutations) { const status = fixture(); mutate(status); expect(liveAlertScopes(status, now)).toEqual([]); }
  });
});
