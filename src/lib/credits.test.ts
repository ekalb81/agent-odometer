import { describe, expect, it } from 'vitest';
import {
  computeSessionApiCostScenarios,
  computeSessionCredits,
  creditsFromBuckets,
  resolveModelPricing,
  tokensCost,
} from './credits';
import rawBundledRates from '../../src-tauri/rates.json';
import type { ModelRate, RateCard, Session, TokenTotals } from './types';

/** The same rates.json Rust bundles via `include_str!` (see
 * `RateCard::load_bundled` and the mirrored regression tests in
 * `src-tauri/src/turn_receipts.rs`) — loaded here so the Rust and
 * TypeScript pricing paths are checked against identical, real catalog
 * data rather than two hand-picked fixtures that could drift apart. */
const bundledRates = rawBundledRates as unknown as {
  models: Record<string, ModelRate>;
  api_models: Record<string, ModelRate>;
};

const zero: TokenTotals = {
  input_tokens: 0,
  cached_input_tokens: 0,
  cache_creation_input_tokens: 0,
  output_tokens: 0,
  reasoning_output_tokens: 0,
  total_tokens: 0,
};

function totals(
  input: number,
  cached: number,
  output: number,
  reasoning: number,
  cacheCreation = 0,
): TokenTotals {
  return {
    input_tokens: input,
    cached_input_tokens: cached,
    cache_creation_input_tokens: cacheCreation,
    output_tokens: output,
    reasoning_output_tokens: reasoning,
    total_tokens: input + output,
  };
}

function rate(
  input: number,
  cached: number,
  output: number,
  reasoning: number,
  cacheCreation: number | null = 0,
): ModelRate {
  return { input, cached_input: cached, cache_creation_input: cacheCreation, output, reasoning };
}

const defaultRefresh: RateCard['refresh'] = {
  last_success_at: null,
  last_attempt_at: null,
  last_failure_reason: null,
  max_cache_age_secs: 604_800,
};

const rateCard: RateCard = {
  version: 7,
  currency: 'credits',
  unit: 'per_1m_tokens',
  source_url: 'https://example.test/rates',
  fetched_at: null,
  models: { 'gpt-5.6-sol': rate(125, 12.5, 750, 750) },
  fallback_model: 'gpt-5.6-sol',
  currencies: { codex: 'credits', claude_code: 'USD' },
  fallback_models: { codex: 'gpt-5.6-sol', claude_code: 'gpt-5.6-sol' },
  api_models: { 'gpt-5.6-sol': rate(5, 0.5, 30, 30, 6.25) },
  unpriced_models: [],
  pricing_catalog: {
    notes: ['cache writes are unobserved'],
    rate_periods: [{
      id: 'openai/gpt-5.6-sol/base',
      surface: 'openai_api_usd',
      model: 'gpt-5.6-sol',
      from: '2026-01-01T00:00:00Z',
      to: null,
      rate: rate(5, 0.5, 30, 30, 6.25),
      cache_write_input_multiplier: 1.25,
      provenance: { evidence: 'test', source_url: 'https://example.test/source', verified_at: '2026-07-25T00:00:00Z', note: null },
      label: 'base API rate',
    }],
    conditional_modifiers: [{
      id: 'openai/gpt-5.6-sol/high-context',
      surface: 'openai_api_usd',
      model: 'gpt-5.6-sol',
      from: '2026-01-01T00:00:00Z',
      to: null,
      condition: { kind: 'request_input_token_threshold', greater_than: 272_000 },
      multipliers: { input: 2, output: 1.5 },
      provenance: { evidence: 'test', source_url: 'https://example.test/source', verified_at: '2026-07-25T00:00:00Z', note: null },
      label: 'high context API rate',
    }],
  },
  model_aliases: {},
  free_local_models: [],
  subscription_plans: {},
  display_currency: null,
  refresh: defaultRefresh,
};

function session(
  events: Session['tokens_history'],
  harness: Session['harness'] = 'codex',
  model = 'gpt-5.6-sol',
  tokensByModel: Session['tokens_by_model'] = {},
): Session {
  return {
    id: 'thread', storage_id: `${harness}:thread:thread`, harness, thread_name: null,
    forked_from_id: null, parent_thread_id: null, agent_path: null, agent_nickname: null,
    file_path: 'fixture.jsonl', source_availability: 'present', archived: false,
    started_at: '2026-07-01T00:00:00Z', last_event_at: '2026-07-01T01:00:00Z',
    working_directory: null, originator: null, source: null, subagent_id_is_path_fallback: false,
    history_mode: null, memory_mode: null,
    cli_version: null, model_provider: harness === 'codex' ? 'openai' : 'anthropic', model, service_tier: null,
    plan_type: 'pro', credits_unlimited: null, credits_balance: null, context_window: null,
    latest_context_tokens: null, total_turns: 0, first_user_message: null, tokens_total: zero,
    tokens_by_model: tokensByModel, tokens_history: events, rate_limits_history: [], turns: [], tool_observations: [],
    tool_metrics: { calls: 0, reads: 0, searches: 0, mutations: 0, commands: 0, other: 0, successes: 0, failures: 0, unknown: 0, mutation_targets: 0, one_shot_mutations: 0, retry_count: 0, duration_ms: 0, output_bytes: 0 },
    tool_metrics_by_model: {}, category_totals: {}, optimization_findings: [],
    project_key: null, project_label: null, project_provenance: null,
  };
}

describe('time-aware API pricing scenarios', () => {
  it('keeps the flat reference separate and only applies high-context multipliers strictly above 272K', () => {
    const result = computeSessionApiCostScenarios(session([
      { timestamp: '2026-07-01T00:00:00Z', model: 'gpt-5.6-sol', service_tier: null, request_input_tokens: 272_000, total_tokens: 272_000, delta: totals(272_000, 20_000, 10_000, 2_000) },
      { timestamp: '2026-07-01T00:05:00Z', model: 'gpt-5.6-sol', service_tier: null, request_input_tokens: 272_001, total_tokens: 272_001, delta: totals(272_001, 20_000, 10_000, 2_000) },
    ]), rateCard);

    expect(result.flat?.total).toBeCloseTo(3.140005, 9);
    expect(result.timeAware?.total).toBeCloseTo(4.56001, 9);
    expect(result.timeAware?.appliedRatePeriods).toEqual(['openai/gpt-5.6-sol/base']);
    expect(result.timeAware?.appliedModifiers).toEqual(['openai/gpt-5.6-sol/high-context']);
    expect(result.timeAware?.unobservedCacheWriteInputMultipliers).toEqual([1.25]);
    expect(result.timeAware?.cacheWritePricingUnmodeled).toBe(true);
    expect(result.timeAware?.conditionalEvidenceMissing).toEqual([]);
  });

  it('evaluates request thresholds from direct request evidence, not a resumed-session reconciliation remainder', () => {
    const result = computeSessionApiCostScenarios(session([
      { timestamp: '2026-07-01T00:00:00Z', model: 'gpt-5.6-sol', service_tier: null, request_input_tokens: 100_000, total_tokens: 400_000, delta: totals(400_000, 0, 0, 0) },
    ]), rateCard);

    expect(result.timeAware?.total).toBe(2);
    expect(result.timeAware?.appliedModifiers).toEqual([]);
  });

  it('does not speculate when historical request-input evidence is missing', () => {
    const result = computeSessionApiCostScenarios(session([
      { timestamp: '2026-07-01T00:00:00Z', model: 'gpt-5.6-sol', service_tier: null, request_input_tokens: null, total_tokens: 300_000, delta: totals(300_000, 0, 0, 0) },
    ]), rateCard);

    expect(result.timeAware?.total).toBe(1.5);
    expect(result.timeAware?.appliedModifiers).toEqual([]);
    expect(result.timeAware?.conditionalEvidenceMissing).toEqual(['openai/gpt-5.6-sol/high-context']);
  });

  it('does not use a current rule retroactively when an event falls outside dated coverage', () => {
    const result = computeSessionApiCostScenarios(session([
      { timestamp: '2025-12-31T23:59:59Z', model: 'gpt-5.6-sol', service_tier: null, request_input_tokens: 10, total_tokens: 10, delta: totals(10, 0, 0, 0) },
    ]), rateCard);

    expect(result.flat?.total).toBeCloseTo(0.00005, 9);
    expect(result.timeAware).toBeNull();
  });

  it('selects each side of a half-open limited-time period from the event timestamp', () => {
    const sonnetRates: RateCard = {
      ...rateCard,
      models: {
        ...rateCard.models,
        'claude-sonnet-5': rate(3, 0.3, 15, 15, 3.75),
      },
      pricing_catalog: {
        notes: [],
        conditional_modifiers: [],
        rate_periods: [
          {
            id: 'anthropic/claude-sonnet-5/intro', surface: 'anthropic_api_usd', model: 'claude-sonnet-5',
            from: '2026-01-01T00:00:00Z', to: '2026-09-01T00:00:00Z',
            rate: rate(2, 0.2, 10, 10, 2.5),
            cache_write_input_multiplier: 1.25,
            provenance: { evidence: 'test', source_url: 'https://example.test/sonnet', verified_at: '2026-07-27T00:00:00Z', note: null },
            label: 'introductory',
          },
          {
            id: 'anthropic/claude-sonnet-5/standard', surface: 'anthropic_api_usd', model: 'claude-sonnet-5',
            from: '2026-09-01T00:00:00Z', to: null,
            rate: rate(3, 0.3, 15, 15, 3.75),
            cache_write_input_multiplier: 1.25,
            provenance: { evidence: 'test', source_url: 'https://example.test/sonnet', verified_at: '2026-07-27T00:00:00Z', note: null },
            label: 'standard',
          },
        ],
      },
    };
    const result = computeSessionApiCostScenarios(session([
      { timestamp: '2026-08-31T23:59:59Z', model: 'claude-sonnet-5', service_tier: null, request_input_tokens: 1_000_000, total_tokens: 1_000_000, delta: totals(1_000_000, 0, 0, 0) },
      { timestamp: '2026-09-01T00:00:00Z', model: 'claude-sonnet-5', service_tier: null, request_input_tokens: 1_000_000, total_tokens: 1_000_000, delta: totals(1_000_000, 0, 0, 0) },
    ], 'claude_code', 'claude-sonnet-5'), sonnetRates);

    expect(result.flat?.total).toBe(6);
    expect(result.timeAware?.total).toBe(5);
    expect(result.timeAware?.appliedRatePeriods).toEqual([
      'anthropic/claude-sonnet-5/intro',
      'anthropic/claude-sonnet-5/standard',
    ]);
    expect(result.timeAware?.cacheWritePricingUnmodeled).toBe(true);
    expect(result.timeAware?.unobservedCacheWriteInputMultipliers).toEqual([1.25]);
  });
});

describe('cache dimensions', () => {
  it('prices cache-creation and cached-read as disjoint subsets of input, never double-counted', () => {
    // 100 input tokens total: 60 plain, 25 cache-read, 15 cache-creation.
    // A double-counting bug would price more than 100 input-side tokens.
    const model = rate(10, 1, 0, 0, 12.5); // input=$10, cached=$1, cache-creation=$12.5 per 1M
    const delta = totals(100, 25, 0, 0, 15);
    const plainInputTokens = delta.input_tokens - delta.cached_input_tokens - delta.cache_creation_input_tokens;
    expect(plainInputTokens).toBe(60);

    const cost = computeSessionCredits(
      session([], 'claude_code', 'claude-sonnet-5', { 'claude-sonnet-5': delta }),
      { ...rateCard, models: { 'claude-sonnet-5': model }, fallback_model: 'claude-sonnet-5' },
    );
    const expected = (60 * 10 + 25 * 1 + 15 * 12.5) / 1_000_000;
    expect(cost.total).toBeCloseTo(expected, 12);
  });

  it('applies the cache-write premium distinctly from the plain input rate', () => {
    const cheapInput = rate(1, 0.1, 0, 0, 1.25); // cache-creation priced 1.25x input
    const buckets = [{
      model: 'claude-sonnet-5', service_tier: null,
      tokens: totals(0, 0, 0, 0, 1_000_000), // pure cache-creation event
    }];
    const result = creditsFromBuckets(buckets, {
      ...rateCard, models: { 'claude-sonnet-5': cheapInput }, fallback_model: 'claude-sonnet-5',
    }, 'claude_code');
    expect(result.total).toBeCloseTo(1.25, 9);
    expect(result.total).not.toBeCloseTo(1.0, 9); // would be the plain-input (undercounted) price
  });

  it('prices cache-creation tokens at the ordinary input rate — never free — when no cache-write premium is published', () => {
    // A model whose ModelRate never states a cache-write premium
    // (cache_creation_input: null) must price cache-creation tokens at the
    // ordinary input rate, producing the exact same total this usage would
    // have priced at before the cache-creation dimension existed (when
    // those tokens were still indistinguishable from plain input). Pricing
    // them at 0 would silently bill real usage as free — the regression
    // this test guards against.
    const noPublishedPremium = rate(4, 0.4, 2, 2, null);
    const delta = totals(100_000, 10_000, 5_000, 0, 20_000);
    const buckets = [{ model: 'gpt-5.6-sol', service_tier: null, tokens: delta }];

    const result = creditsFromBuckets(buckets, {
      ...rateCard, models: { 'gpt-5.6-sol': noPublishedPremium }, fallback_model: 'gpt-5.6-sol',
    }, 'codex');

    // Pre-#42 accounting: cache-creation tokens were folded into plain
    // input, so the whole non-cached-read remainder (which now includes
    // what is separately tracked as cache_creation_input_tokens) priced at
    // the ordinary input rate.
    const preCacheCreationDimensionTotal = (
      (delta.input_tokens - delta.cached_input_tokens) * noPublishedPremium.input
      + delta.cached_input_tokens * noPublishedPremium.cached_input
      + delta.output_tokens * noPublishedPremium.output
    ) / 1_000_000;
    expect(result.total).toBeCloseTo(preCacheCreationDimensionTotal, 9);
    expect(result.total).not.toBe(0);
    expect(result.byModel[0].basis).toBe('estimated');
  });

  it('treats an explicit zero cache-write rate as a deliberate free claim, distinct from an absent rate', () => {
    const explicitlyFree = rate(4, 0.4, 2, 2, 0);
    const buckets = [{
      model: 'local-model', service_tier: null,
      tokens: totals(0, 0, 0, 0, 50_000),
    }];
    const result = creditsFromBuckets(buckets, {
      ...rateCard, models: { 'local-model': explicitlyFree }, fallback_model: 'local-model',
    }, 'claude_code');
    expect(result.total).toBe(0);
    // A direct, explicit 0 rate is still an authoritative direct price, not
    // an estimate — only an *absent* rate downgrades the basis.
    expect(result.byModel[0].basis).toBe('direct');
  });
});

describe('bundled rate-card regression guard', () => {
  // Mirrors src-tauri/src/turn_receipts.rs's
  // `every_bundled_model_prices_added_dimensions_at_or_above_their_folded_cost`
  // and `bundled_models_without_a_published_premium_fall_back_to_the_input_rate`.
  // Rust and TypeScript both compute prices from TokenTotals + ModelRate and
  // must never disagree, so both get the same guard against the next token
  // dimension being added the wrong way: defaulting to a zero rate instead
  // of falling back to the ordinary rate of the bucket it is a subset of.
  const bundledModels = (): Array<[string, ModelRate]> => [
    ...Object.entries(bundledRates.models),
    ...Object.entries(bundledRates.api_models),
  ];

  it('prices every bundled model no lower than folding cache-creation and reasoning into ordinary input/output', () => {
    // cached_input_tokens is deliberately left distinguished (and identical)
    // in both totals: a cache *read* is a genuine, intentional discount
    // below the input rate, not a premium-or-fallback dimension, so it is
    // not part of this invariant.
    const full = totals(1_000_000, 200_000, 500_000, 100_000, 150_000);
    const folded: TokenTotals = { ...full, cache_creation_input_tokens: 0, reasoning_output_tokens: 0 };

    const models = bundledModels();
    expect(models.length).toBeGreaterThan(0);
    for (const [model, rate] of models) {
      const fullCost = tokensCost(full, model, bundledRates as RateCard, null, 'codex', { [model]: rate }).cost;
      const foldedCost = tokensCost(folded, model, bundledRates as RateCard, null, 'codex', { [model]: rate }).cost;
      expect(fullCost).toBeGreaterThanOrEqual(foldedCost - 1e-9);
    }
  });

  it('falls back every bundled model without a published cache-creation premium to its input rate, not zero', () => {
    const modelsWithoutPremium = bundledModels().filter(([, rate]) => rate.cache_creation_input == null);
    expect(modelsWithoutPremium.length).toBeGreaterThan(0);
    for (const [model, rate] of modelsWithoutPremium) {
      const pureCacheCreation = totals(0, 0, 0, 0, 1_000_000);
      const result = tokensCost(pureCacheCreation, model, bundledRates as RateCard, null, 'codex', { [model]: rate });
      expect(result.cost).toBeCloseTo(rate.input, 9);
      expect(result.cost).not.toBe(0);
    }
  });
});

describe('unknown models', () => {
  it('falls back to the configured harness fallback model and flags it', () => {
    const result = tokensCost(totals(1_000_000, 0, 0, 0), 'totally-unknown-model', rateCard, null, 'codex', rateCard.models);
    expect(result.basis).toBe('fallback');
    expect(result.fallbackUsed).toBe(true);
    expect(result.cost).toBeCloseTo((1_000_000 * 125) / 1_000_000, 9);
  });

  it('is unavailable, not silently zero-priced-as-fallback, when no fallback rate exists either', () => {
    const cardWithoutFallbackRate: RateCard = { ...rateCard, models: {}, fallback_model: 'nonexistent' };
    const resolution = resolveModelPricing(cardWithoutFallbackRate, 'totally-unknown-model', 'codex', cardWithoutFallbackRate.models);
    expect(resolution.basis).toBe('unavailable');
  });

  it('excludes a known-unpriced model from cost rather than fallback-pricing it', () => {
    const cardWithUnpriced: RateCard = { ...rateCard, unpriced_models: ['preview-model'] };
    const result = tokensCost(totals(1_000_000, 0, 0, 0), 'preview-model', cardWithUnpriced, null, 'codex', cardWithUnpriced.models);
    expect(result.unpriced).toBe(true);
    expect(result.cost).toBe(0);
    expect(result.basis).toBe('unavailable');
  });
});

describe('aliases and alias cycles', () => {
  it('resolves an alias before falling back, distinct from a direct match', () => {
    const card: RateCard = {
      ...rateCard,
      model_aliases: { 'claude-sonnet-5-20260815': 'claude-sonnet-5' },
      models: { 'claude-sonnet-5': rate(3, 0.3, 15, 15) },
      fallback_model: 'claude-sonnet-5',
    };
    const resolution = resolveModelPricing(card, 'claude-sonnet-5-20260815', 'claude_code', card.models);
    expect(resolution.basis).toBe('aliased');
    expect(resolution.resolvedModel).toBe('claude-sonnet-5');

    const direct = resolveModelPricing(card, 'claude-sonnet-5', 'claude_code', card.models);
    expect(direct.basis).toBe('direct');
  });

  it('follows a multi-hop alias chain to its canonical target', () => {
    const aliases = { a: 'b', b: 'c' };
    const resolution = resolveModelPricing(
      { ...rateCard, model_aliases: aliases, models: { c: rate(1, 0.1, 5, 5) }, fallback_model: 'c' },
      'a', 'codex', { c: rate(1, 0.1, 5, 5) },
    );
    expect(resolution.basis).toBe('aliased');
    expect(resolution.resolvedModel).toBe('c');
  });

  it('terminates on an alias cycle instead of hanging, and falls through to fallback', () => {
    const cyclicAliases = { a: 'b', b: 'a' };
    const card: RateCard = {
      ...rateCard,
      model_aliases: cyclicAliases,
      models: { 'fallback-model': rate(9, 0.9, 9, 9) },
      fallback_model: 'fallback-model',
      fallback_models: {},
    };
    // The real assertion is that this call returns at all (a naive
    // implementation without cycle detection would loop forever), and
    // resolves deterministically to the configured fallback since neither
    // "a" nor "b" is ever a real rate-table key in this scenario.
    const resolution = resolveModelPricing(card, 'a', 'codex', card.models);
    expect(resolution.basis).toBe('fallback');
    expect(resolution.resolvedModel).toBe('fallback-model');
  });

  it('a self-referencing alias terminates without hanging', () => {
    const card: RateCard = {
      ...rateCard,
      model_aliases: { a: 'a' },
      models: { 'fallback-model': rate(9, 0.9, 9, 9) },
      fallback_model: 'fallback-model',
      fallback_models: {},
    };
    const resolution = resolveModelPricing(card, 'a', 'codex', card.models);
    expect(resolution.basis).toBe('fallback');
  });
});

describe('precision', () => {
  it('sums fractional per-1M rates without cent-level rounding drift across many small events', () => {
    // 37 events of 333,333 tokens is not evenly divisible by 1e6 — a naive
    // per-event rounding step would drift from the closed-form total.
    const perEventTokens = 333_333;
    const eventCount = 37;
    const rateCardForModel = rate(3.0, 0.3, 15, 15, 3.75);
    const buckets = Array.from({ length: eventCount }, () => ({
      model: 'claude-sonnet-5', service_tier: null,
      tokens: totals(perEventTokens, 0, 0, 0),
    }));
    const result = creditsFromBuckets(buckets, {
      ...rateCard, models: { 'claude-sonnet-5': rateCardForModel }, fallback_model: 'claude-sonnet-5',
    }, 'claude_code');
    const expected = (perEventTokens * eventCount * 3.0) / 1_000_000;
    expect(result.total).toBeCloseTo(expected, 9);
  });

  it('keeps sub-cent precision instead of collapsing tiny amounts to zero', () => {
    const buckets = [{ model: 'claude-sonnet-5', service_tier: null, tokens: totals(1, 0, 0, 0) }];
    const result = creditsFromBuckets(buckets, {
      ...rateCard, models: { 'claude-sonnet-5': rate(3, 0.3, 15, 15) }, fallback_model: 'claude-sonnet-5',
    }, 'claude_code');
    expect(result.total).toBeGreaterThan(0);
    expect(result.total).toBeCloseTo(3 / 1_000_000, 12);
  });
});

describe('fast-tier service multipliers', () => {
  it('applies the documented 2.5x multiplier to fast GPT-5.5', () => {
    const card: RateCard = { ...rateCard, models: { 'gpt-5.5': rate(100, 10, 500, 500) }, fallback_model: 'gpt-5.5' };
    const result = tokensCost(totals(1_000_000, 0, 0, 0), 'gpt-5.5', card, 'fast', 'codex', card.models);
    expect(result.cost).toBeCloseTo(100 * 2.5, 9);
  });

  it('applies the documented 2x multiplier to fast GPT-5.4', () => {
    const card: RateCard = { ...rateCard, models: { 'gpt-5.4': rate(50, 5, 250, 250) }, fallback_model: 'gpt-5.4' };
    const result = tokensCost(totals(1_000_000, 0, 0, 0), 'gpt-5.4', card, 'fast', 'codex', card.models);
    expect(result.cost).toBeCloseTo(50 * 2, 9);
  });

  it('never applies a fast multiplier to a model without a documented fast rate', () => {
    const card: RateCard = { ...rateCard, models: { 'claude-sonnet-5': rate(3, 0.3, 15, 15) }, fallback_model: 'claude-sonnet-5' };
    const result = tokensCost(totals(1_000_000, 0, 0, 0), 'claude-sonnet-5', card, 'fast', 'claude_code', card.models);
    expect(result.cost).toBeCloseTo(3, 9);
  });
});
