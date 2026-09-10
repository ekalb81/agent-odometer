import { createHash } from 'node:crypto';
import { describe, expect, it } from 'vitest';
import { createFixtureData, createPricingFixtureInput } from './fixtures';
import { assertFixtureRates, mockRangePricing, mockSessionPricing, mockSummaryPricing } from './pricing';
import generated from './pricing.generated.json';

const now = Date.parse('2026-07-29T15:30:00.000Z');

describe('Rust-generated browser pricing responses', () => {
  it('is tied to the current synthetic raw inputs, including distinct summary and detail totals', () => {
    const input = createPricingFixtureInput();
    expect(createHash('sha256').update(JSON.stringify(input)).digest('hex')).toBe(generated.input_sha256);
    expect(input).toEqual(generated.input);
    expect(Object.keys(input.cases).sort()).toEqual(Object.keys(generated.cases).sort());
    const first = Object.values(input.cases)[0];
    expect(first.summary.tokens_total.total_tokens).not.toBe(first.detail.tokens_history.reduce((sum, event) => sum + event.delta.total_tokens, 0));
    const key = Object.keys(input.cases)[0];
    expect(mockSummaryPricing(key).pricing.plan.total).not.toBe(mockSessionPricing(key, first.detail).plan.total);
  });

  it('preserves supplied unsupported, unpriced and fallback states when simulating partial ranges', () => {
    const data = createFixtureData(now, 'sessions-availability-fallback');
    const fallback = data.fixtures.find(f => f.id.endsWith('c0de0001'))!;
    const unpriced = data.fixtures.find(f => f.id.endsWith('c0de0004'))!;
    const claude = data.fixtures.find(f => f.harness === 'claude_code')!;
    const full = mockRangePricing(data.pricingKey(fallback), 1);
    const partial = mockRangePricing(data.pricingKey(fallback), 0.5);
    expect(partial.plan.total).toBe(full.plan.total / 2);
    expect(partial.plan.missing_models).toEqual(['gpt-5.7-visual-preview']);
    expect(partial.plan.by_model[0].basis).toBe('fallback');
    const unavailable = mockRangePricing(data.pricingKey(unpriced), 0.5);
    expect(unavailable.plan.unpriced_models).toEqual(['gpt-5.3-codex-spark']);
    expect(unavailable.plan.total).toBe(0);
    expect(mockRangePricing(data.pricingKey(claude), 0.5).api).toBeNull();
    expect(mockRangePricing(data.pricingKey(fallback), 0).plan.total).toBe(0);
    expect(() => mockRangePricing(data.pricingKey(fallback), Number.NaN)).toThrow('fraction');
    expect(() => mockSummaryPricing('not-a-fixture')).toThrow('No Rust-generated');
  });

  it('keeps canned responses independent of caller mutations', () => {
    const data = createFixtureData(now);
    const key = data.pricingKey(data.fixtures[0]);
    const before = mockSummaryPricing(key);
    const changed = mockSummaryPricing(key);
    changed.pricing.plan.total = 999;
    changed.pricing.plan.by_model[0].cost = 999;
    expect(mockSummaryPricing(key)).toEqual(before);
  });

  it('reuses exact monetary inputs in stress mode while remapping turn identities', () => {
    const data = createFixtureData(now, 'sessions-availability-fallback', 20);
    const templates = createPricingFixtureInput().cases;
    for (const fixture of data.fixtures.filter(f => f.pricingTemplate)) {
      const key = data.pricingKey(fixture);
      const template = templates[key];
      const session = data.details(fixture);
      expect(data.summary(fixture).buckets).toEqual(template.summary.buckets);
      expect(session.turns.map(turn => turn.tokens)).toEqual(template.detail.turns.map(turn => turn.tokens));
      const prices = mockSessionPricing(key, session);
      expect(Object.keys(prices.turn_prices)).toEqual(session.turns.map(turn => turn.turn_id));
      expect(prices.plan).toEqual(mockSessionPricing(key, template.detail).plan);
    }
  });

  it('accepts an unchanged Settings roundtrip, empty optional aliases, reordered models and display-only edits', () => {
    const original = createFixtureData(now).rates;
    const roundtrip = { ...original,
      version: original.version + 1,
      fetched_at: null,
      source_url: 'https://example.invalid/updated',
      floating_model_aliases: {},
      models: Object.fromEntries(Object.entries(original.models).reverse().map(([model, rate]) => [model, {
        input: Number(String(rate.input)), cached_input: Number(String(rate.cached_input)),
        cache_creation_input: rate.cache_creation_input ?? null,
        output: Number(String(rate.output)), reasoning: Number(String(rate.reasoning)),
      }])),
    };
    expect(() => assertFixtureRates(roundtrip, original)).not.toThrow();
  });

  it('rejects price-affecting saves instead of displaying stale fixture prices', () => {
    const original = createFixtureData(now).rates;
    const edited = structuredClone(original);
    edited.models['gpt-5.6-sol'].input += 1;
    expect(() => assertFixtureRates(edited, original)).toThrow('native app');
    expect(() => assertFixtureRates({ ...original, free_local_models: ['gpt-5.6-sol'] }, original)).toThrow('native app');
    expect(() => assertFixtureRates({ ...original, fallback_model: 'gpt-5.6-luna' }, original)).toThrow('native app');
    expect(() => assertFixtureRates({ ...original, refresh: { ...original.refresh, last_success_at: '2020-01-01T00:00:00.000Z' } }, original)).toThrow('native app');
    expect(() => assertFixtureRates({ ...original, future_pricing_rule: true } as typeof original, original)).toThrow('native app');
  });
});
