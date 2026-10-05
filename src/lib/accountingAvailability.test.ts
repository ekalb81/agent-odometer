import { it, expect } from 'vitest';
import { accountingUnavailable, hasVerifiedTokens, unavailableTokens } from './accountingAvailability';
import { zeroTotals } from './sessionProjection';
it('preserves unknown as unavailable and qualifies identity errors without disclosing raw error data', () => {
  expect(hasVerifiedTokens(unavailableTokens())).toBe(false);
  expect(hasVerifiedTokens(undefined)).toBe(false);
  expect(hasVerifiedTokens(zeroTotals())).toBe(true);
  expect(accountingUnavailable('accounting_identity_ambiguous: /private/source')).toContain('ambiguous accounting identities');
  expect(accountingUnavailable('accounting_identity_unverified')).toContain('verification is incomplete');
  expect(accountingUnavailable('accounting_export_changed')).toContain('Rechecking');
  expect(accountingUnavailable('/private/source')).not.toContain('/private/source');
});
