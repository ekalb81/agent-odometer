/// <reference types="node" />
// The frozen fixture is read only; production browser modules have no Node APIs.
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { aggregateModelMetrics, exportRows, projectSession, projectSessions, zeroToolMetrics, zeroTotals } from './sessionProjection';
import type { Harness, PricedSurface, RangePricing, RangeTotals, RateCard, SessionSummary, TierBucket } from './types';

/**
 * Desktop consumption of the frozen pricing oracle (issue #47).
 *
 * The Rust integration suite exercises production pricing and serialization
 * against these accepted prices. This suite sends those same wire payloads
 * through the desktop projections and exports. There is no second pricing
 * engine or expectation-regeneration path.
 *
 * Expected values were captured before the migration and must stay fixed:
 * changing them to match an implementation would destroy the regression oracle.
 */

// Resolved from the vitest root rather than `import.meta.url`: under this
// config the module URL is not a file: URL, so `fileURLToPath` throws.
const FIXTURE = resolve('tests/conformance/pricing-cases.json');

interface Case {
  name: string;
  harness: Harness;
  table: 'plan' | 'api';
  buckets: TierBucket[];
  expected: CaseExpectation;
}

interface CaseExpectation {
  /** Null when the engine reports "not answerable", distinct from a zero cost. */
  total: number | null;
  by_model: PricedSurface['by_model'];
  missing_models: string[];
  unpriced_models: string[];
}

interface Fixture {
  rate_card: RateCard;
  cases: Case[];
}

const fixture: Fixture = JSON.parse(readFileSync(FIXTURE, 'utf8'));
const projectionRates: RateCard = {
  ...fixture.rate_card,
  pricing_catalog: { rate_periods: [], conditional_modifiers: [], notes: [] },
};

function syntheticSession(buckets: TierBucket[]): SessionSummary {
  return {
    id: 'synthetic', storage_id: 'codex:thread:synthetic', harness: 'codex',
    thread_name: null, forked_from_id: null, parent_thread_id: null,
    agent_path: null, agent_nickname: null, file_path: '', source_availability: 'present',
    archived: false, started_at: '2026-08-01T00:00:00Z', last_event_at: '2026-08-01T01:00:00Z',
    working_directory: null, originator: null, source: null, cli_version: null,
    model_provider: null, model: null, service_tier: null, plan_type: null,
    credits_unlimited: null, credits_balance: null, context_window: null,
    total_turns: 0, first_user_message: null, tokens_total: zeroTotals(),
    buckets, tool_metrics: zeroToolMetrics(), tool_metrics_by_model: {},
    category_totals: {}, optimization_findings_count: 0,
    project_key: null, project_label: null, project_provenance: null,
  };
}

describe('pricing conformance fixture (issue #47)', () => {
  it('keeps the complete corpus of backend pricing paths', () => {
    // A fixture that silently lost cases would still pass every assertion
    // below, so the shape of the corpus is pinned too.
    const bases = new Set(
      fixture.cases.flatMap((testCase) => (testCase.expected?.by_model ?? []).map((m) => m.basis)),
    );
    for (const required of ['direct', 'aliased', 'floating_alias', 'fallback', 'unavailable']) {
      expect(bases, `no case exercises the ${required} basis`).toContain(required);
    }
    expect(fixture.cases).toHaveLength(22);
  });

  it('uses authoritative server prices in projections and exports even when local rates disagree', () => {
    const rateCard = projectionRates;
    const testCase = fixture.cases.find((entry) => entry.table === 'plan' && entry.harness === 'codex')!;
    const session = syntheticSession(testCase.buckets);
    const raw: RangeTotals = {
      tokens: zeroTotals(), buckets: testCase.buckets, tool_metrics: zeroToolMetrics(),
      tool_metrics_by_model: {}, optimization_findings_count: 0,
    };
    const withServer: RangeTotals = {
      ...raw,
      pricing: {
        plan: { total: 987654321, by_model: [], missing_models: [], unpriced_models: [] },
        api: { total: 123456789, by_model: [], missing_models: [], unpriced_models: [] },
      },
    };
    const before = projectSession(session, rateCard, raw, true);
    const after = projectSession(session, rateCard, withServer, true);
    expect(before.pricingAvailable).toBe(false);
    expect(before.planCost).toBeNaN();
    expect(before.displayCost).toBeNaN();
    expect(exportRows([before])[0]).toMatchObject({ codex_credits: null, codex_estimated_api_usd: null });
    expect(after.pricingAvailable).toBe(true);
    expect(after.planCost).toBe(987654321);
    expect(after.apiCost).toBe(123456789);
    expect(after.displayCost).toBe(123456789);
    expect(exportRows([after])[0]).toMatchObject({ codex_credits: 987654321, codex_estimated_api_usd: 123456789 });
    expect(after.planCost).not.toBe(testCase.expected.total);

    const changedRates: RateCard = {
      ...rateCard,
      models: Object.fromEntries(Object.entries(fixture.rate_card.models).map(([model, rate]) =>
        [model, { ...rate, input: rate.input * 2, cached_input: rate.cached_input * 2,
          cache_creation_input: (rate.cache_creation_input ?? rate.input) * 2,
          output: rate.output * 2, reasoning: rate.reasoning * 2 }])),
    };
    expect(changedRates.version).toBe(fixture.rate_card.version);
    const repriced = projectSession(session, changedRates, withServer, true);
    expect(repriced.planCost).toBe(after.planCost);
    expect(repriced.displayCost).toBe(after.displayCost);
    expect(exportRows([repriced])).toEqual(exportRows([after]));

    const summaryPricing = { ...withServer.pricing!, plan: { ...withServer.pricing!.plan, total: 456 } };
    const allTime = projectSessions([session], rateCard, { [session.storage_id]: withServer }, false,
      { [session.storage_id]: { pricing: summaryPricing, categories: {} } }, true).get(session.storage_id)!;
    expect(allTime.tokens).toEqual(session.tokens_total);
    expect(allTime.planCost).toBe(456);
    const missingSummary = projectSessions([session], rateCard, { [session.storage_id]: withServer }, false, {}, true)
      .get(session.storage_id)!;
    expect(missingSummary.pricingAvailable).toBe(false);
    expect(missingSummary.planCost).toBeNaN();

    const noApi: RangeTotals = { ...withServer, pricing: { ...withServer.pricing!, api: null } };
    const unavailableApi = projectSession(session, rateCard, noApi, true);
    expect(unavailableApi.planCost).toBe(987654321);
    expect(unavailableApi.displayCost).toBeNaN();
    expect(unavailableApi.pricingAvailable).toBe(false);
    expect(unavailableApi.apiCost).toBeNull();

    const partial: RangeTotals = { ...withServer, pricing: {
      ...withServer.pricing!, api: { total: 19, by_model: [], missing_models: ['unknown'], unpriced_models: ['unpublished'] },
    } };
    const partialProjection = projectSession(session, rateCard, partial, true);
    expect(partialProjection.displayCost).toBe(19);
    expect(partialProjection.missingModels).toEqual(['unknown']);
    expect(partialProjection.unpricedModels).toEqual(['unpublished']);
    expect(exportRows([partialProjection])[0]).toMatchObject({
      codex_estimated_api_usd: null, fallback_models: 'unknown', unpriced_models: 'unpublished',
    });
  });

  it('distinguishes a successfully empty date window from pending or missing pricing', () => {
    const session = syntheticSession([]);
    const pending = projectSessions([session], projectionRates, {}, true, {}, false).get(session.storage_id)!;
    expect(pending.pricingAvailable).toBe(false);
    expect(pending.displayCost).toBeNaN();
    const empty = projectSessions([session], projectionRates, {}, true, {}, true).get(session.storage_id)!;
    expect(empty.pricingAvailable).toBe(true);
    expect(empty.tokens).toEqual(zeroTotals());
    expect(empty.displayCost).toBe(0);
    const missing: RangeTotals = { tokens: zeroTotals(), buckets: [], tool_metrics: zeroToolMetrics(),
      tool_metrics_by_model: {}, optimization_findings_count: 0 };
    const absentPricing = projectSessions([session], projectionRates, { [session.storage_id]: missing }, true, {}, true)
      .get(session.storage_id)!;
    expect(absentPricing.pricingAvailable).toBe(false);
    expect(absentPricing.displayCost).toBeNaN();
  });

  it('uses each server model price once across service tiers and preserves its provenance', () => {
    const tokens = { ...zeroTotals(), input_tokens: 1_000_000, total_tokens: 1_000_000 };
    const buckets: TierBucket[] = [
      { model: 'gpt-5.5', service_tier: null, tokens },
      { model: 'gpt-5.5', service_tier: 'fast', tokens },
      { model: 'unpublished', service_tier: null, tokens },
    ];
    const session = syntheticSession(buckets);
    const api: PricedSurface = {
      total: 47,
      by_model: [
        { model: 'gpt-5.5', cost: 47, basis: 'fallback', unpriced: false },
        { model: 'unpublished', cost: 0, basis: 'unavailable', unpriced: true },
      ],
      missing_models: ['gpt-5.5'], unpriced_models: ['unpublished'],
    };
    const pricing: RangePricing = { plan: { ...api, total: 99 }, api };
    const range: RangeTotals = { tokens, buckets, pricing, tool_metrics: zeroToolMetrics(),
      tool_metrics_by_model: { 'tool-only': { ...zeroToolMetrics(), calls: 2 } }, optimization_findings_count: 0 };
    const metrics = aggregateModelMetrics([session], { [session.storage_id]: range }, projectionRates);
    expect(metrics.find((entry) => entry.model === 'gpt-5.5')).toMatchObject({
      cost: 47, basis: 'fallback', fallbackUsed: true, unpriced: false,
      tokens: { total_tokens: 2_000_000 },
    });
    expect(metrics.find((entry) => entry.model === 'unpublished')).toMatchObject({
      cost: 0, basis: 'unavailable', unpriced: true,
    });
    expect(metrics.find((entry) => entry.model === 'tool-only')).toMatchObject({
      cost: 0, tools: { calls: 2 }, tokens: zeroTotals(),
    });
    const missingPrice = { ...range, pricing: undefined };
    const unavailable = aggregateModelMetrics([session], { [session.storage_id]: missingPrice }, projectionRates);
    expect(unavailable.find((entry) => entry.model === 'gpt-5.5')?.cost).toBeNaN();
    expect(unavailable.find((entry) => entry.model === 'gpt-5.5')?.basis).toBe('unavailable');
  });

  it('keeps estimated server provenance when combined with a direct price in either session order', () => {
    const tokens = { ...zeroTotals(), input_tokens: 3, total_tokens: 3 };
    const buckets: TierBucket[] = [{ model: 'gpt-5.5', service_tier: null, tokens }];
    const estimated = { ...syntheticSession(buckets), storage_id: 'codex:thread:estimated' };
    const direct = { ...syntheticSession(buckets), storage_id: 'codex:thread:direct' };
    const range = (cost: number, basis: 'estimated' | 'direct'): RangeTotals => {
      const priced: PricedSurface = { total: cost, by_model: [{ model: 'gpt-5.5', cost, basis, unpriced: false }],
        missing_models: [], unpriced_models: [] };
      return { tokens, buckets, pricing: { plan: priced, api: priced }, tool_metrics: zeroToolMetrics(),
        tool_metrics_by_model: {}, optimization_findings_count: 0 };
    };
    const ranges = { [estimated.storage_id]: range(7, 'estimated'), [direct.storage_id]: range(13, 'direct') };
    for (const sessions of [[estimated, direct], [direct, estimated]]) {
      expect(aggregateModelMetrics(sessions, ranges, projectionRates)).toMatchObject([{
        model: 'gpt-5.5', cost: 20, basis: 'estimated', fallbackUsed: false, unpriced: false,
        tokens: { total_tokens: 6 },
      }]);
    }
  });

  for (const testCase of fixture.cases) {
    it(`projects the backend payload: ${testCase.name}`, () => {
      const expected = testCase.expected;
      const session = { ...syntheticSession(testCase.buckets), harness: testCase.harness };
      const surface = expected.total === null ? null : { ...expected, total: expected.total };
      const zero: PricedSurface = { total: 0, by_model: [], missing_models: [], unpriced_models: [] };
      const pricing: RangePricing = testCase.table === 'plan'
        ? { plan: surface!, api: null }
        : { plan: zero, api: surface };
      // Selecting a column is presentation; prices arrive ready to display.
      const rates = testCase.table === 'plan' ? { ...projectionRates, api_models: {} } : projectionRates;
      const range: RangeTotals = { tokens: zeroTotals(), buckets: testCase.buckets,
        tool_metrics: zeroToolMetrics(), tool_metrics_by_model: {}, optimization_findings_count: 0, pricing };
      const projection = projectSession(session, rates, range, true);
      if (expected.total === null) {
        expect(projection.apiCost).toBeNull();
        expect(exportRows([projection])[0].codex_estimated_api_usd).toBeNull();
        return;
      }
      expect(projection.displayCost).toBe(expected.total);
      expect(projection.missingModels).toEqual(expected.missing_models);
      expect(projection.unpricedModels).toEqual(expected.unpriced_models);
      const metrics = aggregateModelMetrics([session], { [session.storage_id]: range }, rates);
      for (const model of expected.by_model) {
        expect(metrics.find((metric) => metric.model === model.model)).toMatchObject({
          cost: model.cost, basis: model.basis, unpriced: model.unpriced,
        });
      }
      const exported = exportRows([projection])[0];
      if (testCase.table === 'plan' && testCase.harness === 'codex') {
        expect(exported.codex_credits).toBe(expected.total);
      } else if (testCase.table === 'api') {
        expect(exported.codex_estimated_api_usd).toBe(
          expected.missing_models.length || expected.unpriced_models.length ? null : expected.total,
        );
      }
    });
  }
});
