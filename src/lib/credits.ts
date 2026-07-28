import type {
  Harness,
  ModelRate,
  PricingCatalog,
  PricingSurface,
  RateCard,
  Session,
  SessionSummary,
  TierBucket,
  TokenTotals,
} from './types';

export interface ModelCredit {
  model: string;
  cost: number;
  fallbackUsed: boolean;
  unpriced: boolean;
}

export interface SessionCredits {
  total: number;
  byModel: ModelCredit[];
  missingModels: string[];
  unpricedModels: string[];
}

/** An exact, event-timestamped API scenario.  It is only available for full
 * sessions because summary buckets deliberately omit request timestamps and
 * input context. */
export interface TimeAwareApiScenario extends SessionCredits {
  surface: PricingSurface;
  appliedRatePeriods: string[];
  appliedModifiers: string[];
  /** Conditional rules whose scope matched but whose per-request input
   * evidence was absent. They are not applied speculatively. */
  conditionalEvidenceMissing: string[];
  cacheWritePricingUnmodeled: boolean;
  /** Documented multipliers selected by the dated rate period. No dollars are
   * added for these because telemetry does not identify cache-write tokens. */
  unobservedCacheWriteInputMultipliers: number[];
}

/** The legacy flat reference and the separately-scoped dated scenario.
 * `timeAware` is null rather than extrapolated when any observed request has
 * no matching catalog period. */
export interface SessionApiCostScenarios {
  flat: SessionCredits | null;
  timeAware: TimeAwareApiScenario | null;
}

/** Currency label for a harness, falling back to the card-wide currency. */
export function harnessCurrency(rates: RateCard, harness: Harness): string {
  return rates.currencies?.[harness] ?? rates.currency;
}

/** Fallback model for a harness, falling back to the card-wide fallback. */
function fallbackModelFor(rates: RateCard, harness: Harness): string {
  return rates.fallback_models?.[harness] ?? rates.fallback_model;
}

function isUnpricedModel(rates: RateCard, model: string | null): boolean {
  return model !== null && (rates.unpriced_models ?? []).includes(model);
}

export function fallbackModelName(rates: RateCard, harness: Harness): string {
  return fallbackModelFor(rates, harness);
}

function serviceTierMultiplier(model: string | null, serviceTier: string | null): number {
  if (serviceTier !== 'fast') return 1;
  if (model === 'gpt-5.5') return 2.5;
  if (model === 'gpt-5.4') return 2;
  return 1;
}

/** Cost of one event's token delta under a given rate, with OpenAI subset semantics. */
function eventCost(delta: TokenTotals, rate: ModelRate, multiplier = 1): number {
  const nonCachedInput = Math.max(0, delta.input_tokens - delta.cached_input_tokens);
  const nonReasoningOutput = Math.max(0, delta.output_tokens - delta.reasoning_output_tokens);
  return (
    (nonCachedInput * rate.input +
      delta.cached_input_tokens * rate.cached_input +
      nonReasoningOutput * rate.output +
      delta.reasoning_output_tokens * rate.reasoning) /
    1_000_000
  ) * multiplier;
}

/**
 * Cost of an arbitrary token bucket attributed to a single model, e.g. one
 * turn's tokens. Falls back to the rate card's fallback model when the model
 * isn't listed; returns 0 if neither resolves. Returns `fallbackUsed` so the
 * UI can flag it. `table` selects which rate table to price against —
 * defaults to plan-credit rates; pass `rates.api_models` for API USD.
 */
export function tokensCost(
  tokens: TokenTotals,
  model: string | null,
  rates: RateCard,
  serviceTier: string | null = null,
  harness: Harness = 'codex',
  table: Record<string, ModelRate> = rates.models,
): { cost: number; fallbackUsed: boolean; unpriced: boolean } {
  if (isUnpricedModel(rates, model)) {
    return { cost: 0, fallbackUsed: false, unpriced: true };
  }
  const directRate = model ? table[model] : undefined;
  const fallbackUsed = directRate === undefined;
  const rate = directRate ?? table[fallbackModelFor(rates, harness)];
  if (!rate) return { cost: 0, fallbackUsed, unpriced: false };
  return {
    cost: eventCost(tokens, rate, serviceTierMultiplier(model, serviceTier)),
    fallbackUsed,
    unpriced: false,
  };
}

/**
 * Prices (model, service_tier) usage buckets — the summary/range wire form.
 * Credit math is linear per (model, tier), so this matches the per-event
 * history computation exactly.
 */
export function creditsFromBuckets(
  buckets: TierBucket[],
  rates: RateCard,
  harness: Harness,
): SessionCredits {
  return bucketsCost(buckets, rates.models, fallbackModelFor(rates, harness), rates.unpriced_models);
}

/**
 * The same usage priced at OpenAI API USD rates (rates.api_models) instead
 * of plan credits. Returns null when no API rate table is configured, so
 * callers can hide the column rather than show zeros.
 */
export function apiCostFromBuckets(
  buckets: TierBucket[],
  rates: RateCard,
  harness: Harness,
): SessionCredits | null {
  if (Object.keys(rates.api_models ?? {}).length === 0) return null;
  return bucketsCost(buckets, rates.api_models, fallbackModelFor(rates, harness), rates.unpriced_models);
}

function bucketsCost(
  buckets: TierBucket[],
  table: Record<string, ModelRate>,
  fallbackName: string,
  unpricedModelNames: string[] = [],
): SessionCredits {
  const byModelMap = new Map<string, number>();
  const missingModels = new Set<string>();
  const unpricedModels = new Set<string>();
  const unpriced = new Set(unpricedModelNames);
  let total = 0;

  const fallbackRate = table[fallbackName];

  for (const b of buckets) {
    if (unpriced.has(b.model)) {
      unpricedModels.add(b.model);
      byModelMap.set(b.model, byModelMap.get(b.model) ?? 0);
      continue;
    }
    const directRate = table[b.model];
    if (directRate === undefined) missingModels.add(b.model);
    const rate = directRate ?? fallbackRate;
    if (!rate) continue;

    const cost = eventCost(b.tokens, rate, serviceTierMultiplier(b.model, b.service_tier));
    total += cost;
    byModelMap.set(b.model, (byModelMap.get(b.model) ?? 0) + cost);
  }

  return {
    total,
    byModel: Array.from(byModelMap, ([model, cost]) => ({
      model,
      cost,
      fallbackUsed: table[model] === undefined && !unpriced.has(model),
      unpriced: unpriced.has(model),
    })),
    missingModels: Array.from(missingModels),
    unpricedModels: Array.from(unpricedModels),
  };
}

/** All-time credits for a list-view summary. */
export function computeSummaryCredits(summary: SessionSummary, rates: RateCard): SessionCredits {
  return creditsFromBuckets(summary.buckets, rates, summary.harness);
}

/** All-time OpenAI-API-rate cost for a full session (drawer). Null when unconfigured. */
export function computeSessionApiCost(session: Session, rates: RateCard): SessionCredits | null {
  if (Object.keys(rates.api_models ?? {}).length === 0) return null;
  return historyCost(session, rates.api_models, fallbackModelFor(rates, session.harness), rates.unpriced_models);
}

function apiSurfaceForHarness(harness: Harness): PricingSurface {
  return harness === 'codex' ? 'openai_api_usd' : 'anthropic_api_usd';
}

function inPeriod(timestamp: string, from: string, to: string | null): boolean {
  const at = Date.parse(timestamp);
  const start = Date.parse(from);
  const end = to === null ? Number.POSITIVE_INFINITY : Date.parse(to);
  return Number.isFinite(at) && Number.isFinite(start) && (to === null || Number.isFinite(end))
    && at >= start && at < end;
}

function applicablePeriod(
  catalog: PricingCatalog,
  surface: PricingSurface,
  model: string,
  timestamp: string,
) {
  return catalog.rate_periods.find((period) =>
    period.surface === surface && period.model === model && inPeriod(timestamp, period.from, period.to));
}

function applicableModifiers(
  catalog: PricingCatalog,
  surface: PricingSurface,
  model: string,
  timestamp: string,
  requestInputTokens: number | null,
) {
  if (requestInputTokens === null) return [];
  return catalog.conditional_modifiers.filter((modifier) =>
    modifier.surface === surface
      && modifier.model === model
      && inPeriod(timestamp, modifier.from, modifier.to)
      && modifier.condition.kind === 'request_input_token_threshold'
      && requestInputTokens > modifier.condition.greater_than);
}

/**
 * Returns two intentionally separate API figures for a detailed session:
 *
 * - `flat` retains the existing legacy rate-table calculation.
 * - `timeAware` selects a dated catalog period and conditional modifiers for
 *   each token event. It never uses the current rule retroactively, and is
 *   unavailable when the event history or applicable catalog coverage is
 *   incomplete.
 *
 * The modifier input multiplier applies to both ordinary and cached input;
 * the output multiplier applies to ordinary and reasoning output. Cache-write
 * premiums are not inferred because parsers do not observe that category.
 */
export function computeSessionApiCostScenarios(
  session: Session,
  rates: RateCard,
): SessionApiCostScenarios {
  const flat = session.harness === 'codex'
    ? computeSessionApiCost(session, rates)
    : historyCost(session, rates.models, fallbackModelFor(rates, session.harness), rates.unpriced_models);
  if (session.tokens_history.length === 0 || rates.pricing_catalog.rate_periods.length === 0) {
    return { flat, timeAware: null };
  }

  const surface = apiSurfaceForHarness(session.harness);
  const byModelMap = new Map<string, number>();
  const periodIds = new Set<string>();
  const modifierIds = new Set<string>();
  const missingConditionalEvidence = new Set<string>();
  const unpriced = new Set(rates.unpriced_models ?? []);
  const cacheWriteMultipliers = new Set<number>();
  let total = 0;

  for (const event of session.tokens_history) {
    if (!event.model) {
      // A full event history is only useful for dated pricing when every
      // priced request is attributable to a model.
      if (event.delta.total_tokens > 0) return { flat, timeAware: null };
      continue;
    }
    if (unpriced.has(event.model)) {
      return { flat, timeAware: null };
    }
    const period = applicablePeriod(rates.pricing_catalog, surface, event.model, event.timestamp);
    // A dated scenario must have direct evidence for every observed model and
    // request. Do not substitute a fallback or a latest/current rate here.
    if (!period) {
      return { flat, timeAware: null };
    }
    const modifiers = applicableModifiers(
      rates.pricing_catalog,
      surface,
      event.model,
      event.timestamp,
      event.request_input_tokens,
    );
    if (event.request_input_tokens === null) {
      for (const modifier of rates.pricing_catalog.conditional_modifiers) {
        if (modifier.surface === surface
          && modifier.model === event.model
          && inPeriod(event.timestamp, modifier.from, modifier.to)) {
          missingConditionalEvidence.add(modifier.id);
        }
      }
    }
    const inputMultiplier = modifiers.reduce((value, modifier) => value * modifier.multipliers.input, 1);
    const outputMultiplier = modifiers.reduce((value, modifier) => value * modifier.multipliers.output, 1);
    const nonCachedInput = Math.max(0, event.delta.input_tokens - event.delta.cached_input_tokens);
    const nonReasoningOutput = Math.max(0, event.delta.output_tokens - event.delta.reasoning_output_tokens);
    const cost = (
      (nonCachedInput * period.rate.input * inputMultiplier)
      + (event.delta.cached_input_tokens * period.rate.cached_input * inputMultiplier)
      + (nonReasoningOutput * period.rate.output * outputMultiplier)
      + (event.delta.reasoning_output_tokens * period.rate.reasoning * outputMultiplier)
    ) / 1_000_000 * serviceTierMultiplier(event.model, event.service_tier);
    total += cost;
    byModelMap.set(event.model, (byModelMap.get(event.model) ?? 0) + cost);
    periodIds.add(period.id);
    if (period.cache_write_input_multiplier !== null && period.cache_write_input_multiplier !== undefined) {
      cacheWriteMultipliers.add(period.cache_write_input_multiplier);
    }
    for (const modifier of modifiers) modifierIds.add(modifier.id);
  }

  return {
    flat,
    timeAware: {
      total,
      byModel: Array.from(byModelMap, ([model, cost]) => ({
        model,
        cost,
        fallbackUsed: false,
        unpriced: unpriced.has(model),
      })),
      missingModels: [],
      unpricedModels: [],
      surface,
      appliedRatePeriods: Array.from(periodIds),
      appliedModifiers: Array.from(modifierIds),
      conditionalEvidenceMissing: Array.from(missingConditionalEvidence),
      cacheWritePricingUnmodeled: cacheWriteMultipliers.size > 0,
      unobservedCacheWriteInputMultipliers: Array.from(cacheWriteMultipliers),
    },
  };
}

export function computeSessionCredits(session: Session, rates: RateCard): SessionCredits {
  if (session.tokens_history.length > 0) {
    return historyCost(session, rates.models, fallbackModelFor(rates, session.harness), rates.unpriced_models);
  }
  const entries = Object.entries(session.tokens_by_model);

  if (entries.length === 0) {
    return { total: 0, byModel: [], missingModels: [], unpricedModels: [] };
  }

  const byModel: ModelCredit[] = [];
  const missingModels: string[] = [];
  const unpricedModels: string[] = [];
  let total = 0;

  for (const [model, totals] of entries) {
    const unpriced = isUnpricedModel(rates, model);
    if (unpriced) {
      unpricedModels.push(model);
      byModel.push({ model, cost: 0, fallbackUsed: false, unpriced: true });
      continue;
    }
    const directRate = rates.models[model];
    const fallbackRate = rates.models[fallbackModelFor(rates, session.harness)];
    const fallbackUsed = directRate === undefined;

    if (fallbackUsed) {
      missingModels.push(model);
    }

    const rate = directRate ?? fallbackRate;

    if (!rate) {
      // Neither the model nor the fallback exists in the rate card.
      byModel.push({ model, cost: 0, fallbackUsed, unpriced: false });
      continue;
    }

    const cost = eventCost(totals, rate);

    total += cost;
    byModel.push({ model, cost, fallbackUsed, unpriced: false });
  }

  return { total, byModel, missingModels, unpricedModels };
}

function historyCost(
  session: Session,
  table: Record<string, ModelRate>,
  fallbackName: string,
  unpricedModelNames: string[] = [],
): SessionCredits {
  const byModelMap = new Map<string, number>();
  const missingModels = new Set<string>();
  const unpricedModels = new Set<string>();
  const unpriced = new Set(unpricedModelNames);
  let total = 0;

  const fallbackRate = table[fallbackName];

  for (const ev of session.tokens_history) {
    if (!ev.model) continue;
    if (unpriced.has(ev.model)) {
      unpricedModels.add(ev.model);
      byModelMap.set(ev.model, byModelMap.get(ev.model) ?? 0);
      continue;
    }

    const directRate = table[ev.model];
    const fallbackUsed = directRate === undefined;
    if (fallbackUsed) missingModels.add(ev.model);
    const rate = directRate ?? fallbackRate;
    if (!rate) continue;

    const cost = eventCost(
      ev.delta,
      rate,
      serviceTierMultiplier(ev.model, ev.service_tier),
    );
    total += cost;
    byModelMap.set(ev.model, (byModelMap.get(ev.model) ?? 0) + cost);
  }

  return {
    total,
    byModel: Array.from(byModelMap, ([model, cost]) => ({
      model,
      cost,
      fallbackUsed: table[model] === undefined && !unpriced.has(model),
      unpriced: unpriced.has(model),
    })),
    missingModels: Array.from(missingModels),
    unpricedModels: Array.from(unpricedModels),
  };
}

const ISO_CURRENCY = /^[A-Z]{3}$/;

/**
 * Formats a credit amount per the rate card's `currency` field. If `currency`
 * looks like an ISO 4217 code (e.g. "USD"), uses Intl currency formatting.
 * Otherwise (e.g. "credits"), formats as a plain decimal with the unit suffix.
 */
export function formatCredits(amount: number, currency: string): string {
  // Two decimals everywhere per the Instrument Ledger spec; amounts that
  // would round to 0.00 keep enough significant digits to stay honest.
  const subCent = amount !== 0 && Math.abs(amount) < 0.005;
  if (ISO_CURRENCY.test(currency)) {
    return new Intl.NumberFormat('en-US', {
      style: 'currency',
      currency,
      minimumFractionDigits: 2,
      maximumFractionDigits: subCent ? 4 : 2,
    }).format(amount);
  }
  const num = new Intl.NumberFormat('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: subCent ? 4 : 2,
  }).format(amount);
  return `${num} ${currency}`;
}
