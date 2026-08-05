import { describe, expect, it } from 'vitest';
import { computeTrayTotals, type TraySessionLike } from './trayTotals';
import type { RangeTotals, RateCard, TokenTotals } from './types';

const zero: TokenTotals = {
  input_tokens: 0, cached_input_tokens: 0, cache_creation_input_tokens: 0, output_tokens: 0,
  reasoning_output_tokens: 0, total_tokens: 0,
};

function range(model: string, input: number, output: number): RangeTotals {
  return {
    tokens: { ...zero, input_tokens: input, output_tokens: output, total_tokens: input + output },
    buckets: [{
      model,
      service_tier: null,
      tokens: { ...zero, input_tokens: input, output_tokens: output, total_tokens: input + output },
    }],
    tool_metrics: {
      calls: 0, reads: 0, searches: 0, mutations: 0, commands: 0, other: 0,
      successes: 0, failures: 0, unknown: 0, mutation_targets: 0,
      one_shot_mutations: 0, retry_count: 0, duration_ms: 0, output_bytes: 0,
    },
    tool_metrics_by_model: {},
    optimization_findings_count: 0,
  };
}

const rateCard: RateCard = {
  version: 1,
  currency: 'credits',
  unit: 'per_1m_tokens',
  source_url: 'https://example.test/rates',
  fetched_at: null,
  models: { m: { input: 1_000_000, cached_input: 0, cache_creation_input: 0, output: 2_000_000, reasoning: 2_000_000 } },
  fallback_model: 'm',
  currencies: { codex: 'credits', claude_code: 'USD' },
  fallback_models: { codex: 'm', claude_code: 'm' },
  api_models: { m: { input: 500_000, cached_input: 0, cache_creation_input: 0, output: 1_000_000, reasoning: 1_000_000 } },
  unpriced_models: [],
  pricing_catalog: { notes: [], rate_periods: [], conditional_modifiers: [] },
  model_aliases: {},
  free_local_models: [],
  subscription_plans: {},
  display_currency: null,
  refresh: { last_success_at: null, last_attempt_at: null, last_failure_reason: null, max_cache_age_secs: 604_800 },
};

function session(id: string, harness: TraySessionLike['harness'], unlimited: boolean | null = null): TraySessionLike {
  return { storage_id: id, harness, credits_unlimited: unlimited };
}

describe('computeTrayTotals', () => {
  it('sums tokens and prices codex + claude sessions from their rollups', () => {
    const totals = computeTrayTotals(
      [session('c1', 'codex'), session('a1', 'claude_code')],
      { c1: range('m', 3, 1), a1: range('m', 2, 2) },
      rateCard,
    );
    expect(totals.tokens).toBe('8');
    // c1: 3 input × 1 credit + 1 output × 2 credits = 5.00 credits
    expect(totals.codex_credits).toBe('5.00');
    // c1 API: 3 × 0.5 + 1 × 1 = $2.50
    expect(totals.codex_api_usd).toBe('$2.50');
    // a1: 2 × 1 + 2 × 2 = $6.00 (claude prices its plan table in USD)
    expect(totals.claude_usd).toBe('$6.00');
  });

  it('skips sessions with no rollup in the window', () => {
    const totals = computeTrayTotals(
      [session('c1', 'codex'), session('idle', 'codex')],
      { c1: range('m', 1, 0) },
      rateCard,
    );
    expect(totals.tokens).toBe('1');
    expect(totals.codex_credits).toBe('1.00');
  });

  it('reports unlimited sessions without billing them', () => {
    const totals = computeTrayTotals(
      [session('u1', 'codex', true), session('c1', 'codex')],
      { u1: range('m', 5, 5), c1: range('m', 1, 0) },
      rateCard,
    );
    expect(totals.codex_credits).toBe('1.00 + 1 unlimited');
    const onlyUnlimited = computeTrayTotals(
      [session('u1', 'codex', true)],
      { u1: range('m', 5, 5) },
      rateCard,
    );
    expect(onlyUnlimited.codex_credits).toBe('unlimited (1)');
  });

  it('marks the API estimate unavailable when no API table is configured', () => {
    const noApi: RateCard = { ...rateCard, api_models: {} };
    const totals = computeTrayTotals(
      [session('c1', 'codex')],
      { c1: range('m', 1, 0) },
      noApi,
    );
    expect(totals.codex_api_usd).toBe('unavailable · missing direct rate');
  });

  it('carries a quota label through unmodified, or an empty string when none is available', () => {
    const withQuota = computeTrayTotals(
      [session('c1', 'codex')],
      { c1: range('m', 1, 0) },
      rateCard,
      'codex 5h 37% left',
    );
    expect(withQuota.quota).toBe('codex 5h 37% left');

    const withoutQuota = computeTrayTotals([session('c1', 'codex')], { c1: range('m', 1, 0) }, rateCard);
    expect(withoutQuota.quota).toBe('');
  });
});
