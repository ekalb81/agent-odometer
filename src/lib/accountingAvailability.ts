import type { TokenTotals } from './types';

/** Never turn missing integrity evidence into a numeric zero. */
export function unavailableTokens(): TokenTotals {
  return { input_tokens: NaN, cached_input_tokens: NaN, cache_creation_input_tokens: NaN,
    output_tokens: NaN, reasoning_output_tokens: NaN, total_tokens: NaN };
}

export function accountingUnavailable(reason: unknown): string {
  const message = String(reason);
  if (message.includes('accounting_export_changed')) return 'Usage changed while choosing the export destination. Refresh the preview before exporting.';
  if (message.includes('accounting_identity_ambiguous')) {
    return 'Usage unavailable: historical records contain ambiguous accounting identities. Individual records remain available.';
  }
  if (message.includes('accounting_identity_unverified')) {
    return 'Usage unavailable: accounting identity verification is incomplete. Retry when history is ready.';
  }
  return 'Usage unavailable: the complete accounting scope could not be verified. Retry the query.';
}

export function hasVerifiedTokens(tokens: TokenTotals | undefined): tokens is TokenTotals {
  return !!tokens && ['input_tokens', 'cached_input_tokens', 'cache_creation_input_tokens', 'output_tokens', 'reasoning_output_tokens', 'total_tokens'].every(key => { const value = tokens[key as keyof TokenTotals]; return typeof value === 'number' && Number.isFinite(value) && value >= 0; });
}
