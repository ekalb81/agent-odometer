import { describe, expect, it } from 'vitest';
import { computeSessionApiCostScenarios } from './credits';
import type { RateCard, Session, TokenTotals } from './types';

const zero: TokenTotals = {
  input_tokens: 0,
  cached_input_tokens: 0,
  output_tokens: 0,
  reasoning_output_tokens: 0,
  total_tokens: 0,
};

function totals(input: number, cached: number, output: number, reasoning: number): TokenTotals {
  return {
    input_tokens: input,
    cached_input_tokens: cached,
    output_tokens: output,
    reasoning_output_tokens: reasoning,
    total_tokens: input + output,
  };
}

const rateCard: RateCard = {
  version: 7,
  currency: 'credits',
  unit: 'per_1m_tokens',
  source_url: 'https://example.test/rates',
  fetched_at: null,
  models: { 'gpt-5.6-sol': { input: 125, cached_input: 12.5, output: 750, reasoning: 750 } },
  fallback_model: 'gpt-5.6-sol',
  currencies: { codex: 'credits', claude_code: 'USD' },
  fallback_models: { codex: 'gpt-5.6-sol', claude_code: 'gpt-5.6-sol' },
  api_models: { 'gpt-5.6-sol': { input: 5, cached_input: 0.5, output: 30, reasoning: 30 } },
  unpriced_models: [],
  pricing_catalog: {
    notes: ['cache writes are unobserved'],
    rate_periods: [{
      id: 'openai/gpt-5.6-sol/base',
      surface: 'openai_api_usd',
      model: 'gpt-5.6-sol',
      from: '2026-01-01T00:00:00Z',
      to: null,
      rate: { input: 5, cached_input: 0.5, output: 30, reasoning: 30 },
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
};

function session(
  events: Session['tokens_history'],
  harness: Session['harness'] = 'codex',
  model = 'gpt-5.6-sol',
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
    tokens_by_model: {}, tokens_history: events, rate_limits_history: [], turns: [], tool_observations: [],
    tool_metrics: { calls: 0, reads: 0, searches: 0, mutations: 0, commands: 0, other: 0, successes: 0, failures: 0, unknown: 0, mutation_targets: 0, one_shot_mutations: 0, retry_count: 0, duration_ms: 0, output_bytes: 0 },
    tool_metrics_by_model: {}, category_totals: {}, optimization_findings: [],
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
        'claude-sonnet-5': { input: 3, cached_input: 0.3, output: 15, reasoning: 15 },
      },
      pricing_catalog: {
        notes: [],
        conditional_modifiers: [],
        rate_periods: [
          {
            id: 'anthropic/claude-sonnet-5/intro', surface: 'anthropic_api_usd', model: 'claude-sonnet-5',
            from: '2026-01-01T00:00:00Z', to: '2026-09-01T00:00:00Z',
            rate: { input: 2, cached_input: 0.2, output: 10, reasoning: 10 },
            cache_write_input_multiplier: 1.25,
            provenance: { evidence: 'test', source_url: 'https://example.test/sonnet', verified_at: '2026-07-27T00:00:00Z', note: null },
            label: 'introductory',
          },
          {
            id: 'anthropic/claude-sonnet-5/standard', surface: 'anthropic_api_usd', model: 'claude-sonnet-5',
            from: '2026-09-01T00:00:00Z', to: null,
            rate: { input: 3, cached_input: 0.3, output: 15, reasoning: 15 },
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
