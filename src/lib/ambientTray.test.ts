import { expect, it } from 'vitest';
import { ambientQuotaLabel } from './ambientTray';
import type { QuotaSnapshot } from './types';
import type { LiveQuotaAccountView, LiveQuotaStatus } from './liveQuota';
const now = Date.parse('2026-10-04T12:00:00Z');
const snapshot: QuotaSnapshot = { provider: 'codex', provenance: 'transcript_derived', unavailable: null, windows: [{
  limit: 100, window_started_at: null, window_started_at_estimated: false, confidence: "high", kind: 'burst', unit: 'percent', used: 63, remaining: 37, unlimited: false, window_minutes: 300,
  observed_at: '2026-10-04T11:55:00Z', resets_at: '2026-10-04T15:00:00Z', stale: false, unavailable: null,
  forecast: { pace_per_hour: 4.2, projected_exhaustion_at: null, reserve_deficit_percent: 2, evidence_points: 7 },
}] };
const empty: LiveQuotaStatus = { accounts: [], busy: false, configuration_error: null };
const account: LiveQuotaAccountView = { provider: 'codex', consent: { account_id: 'private', label: 'private label', enabled: true, consented_at: '2026-10-04T11:00:00Z' }, observed_at: '2026-10-04T11:55:00Z', ordinary_usage_allowed: true, buckets: [{ limit_id: 'id', limit_name: null, spend_control_reached: false, snapshot }], unavailable: null };
it('formats shared backend headroom/pace/reset in explicit provider scope', () => {
  expect(ambientQuotaLabel('codex', [snapshot], empty, now)).toContain('37% left · reset 3h 0m · pace 4.2%/h · unattributed transcript as of 5m ago');
  expect(ambientQuotaLabel('all', [snapshot], empty, now)).toContain('select a provider');
  expect(ambientQuotaLabel('claude_code', [snapshot], empty, now)).toBe('Claude Code transcript quota unavailable');
});
it('never chooses among live accounts or falls back from an unavailable approved account', () => {
  expect(ambientQuotaLabel('codex', [snapshot], { ...empty, accounts: [account, account] }, now)).toContain('multiple approved accounts');
  expect(ambientQuotaLabel('codex', [snapshot], { ...empty, accounts: [{ ...account, unavailable: 'offline' }] }, now)).toBe('Codex live unavailable');
  const live = ambientQuotaLabel('codex', [snapshot], { ...empty, accounts: [account] }, now);
  expect(live).toContain('approved account'); expect(live).not.toContain('private');
});
it('future/expired observations cannot present a current pace', () => {
  const stale = { ...snapshot, windows: [{ ...snapshot.windows[0], stale: true }] };
  expect(ambientQuotaLabel('codex', [stale], empty, now)).toContain('(stale) · reset 3h 0m · pace unknown');
  const future = { ...snapshot, windows: [{ ...snapshot.windows[0], observed_at: '2026-10-04T13:00:00Z' }] };
  expect(ambientQuotaLabel('codex', [future], empty, now)).toContain('(stale)');
  expect(ambientQuotaLabel('codex', [future], empty, now)).toContain('future timestamp');
});
it('names the exact selected live model limit without presenting it as account-wide headroom', () => {
  const model = { limit_id: 'gpt-5.5-model', limit_name: 'GPT-5.5', spend_control_reached: false, snapshot };
  const other = { ...model, limit_id: 'codex-other-model', limit_name: 'Codex other' };
  const value = { ...account, buckets: [model, other] };
  expect(ambientQuotaLabel('codex', [], { ...empty, accounts: [value] }, now)).toContain('limit GPT-5.5 [gpt-5.5-model]');
  const reordered = { ...value, buckets: [other, model] };
  expect(ambientQuotaLabel('codex', [], { ...empty, accounts: [reordered] }, now)).toContain('limit Codex other [codex-other-model]');
  const unsafe = { ...value, buckets: [{ ...model, limit_id: 'unsafe\nidentity' }] };
  expect(ambientQuotaLabel('codex', [], { ...empty, accounts: [unsafe] }, now)).toBe('Codex live quota unavailable · limit identity unavailable');
});
