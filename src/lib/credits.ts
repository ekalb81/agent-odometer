import type {
  CurrencyConversion,
  FloatingAlias,
  Harness,
  ModelRate,
  PricingBasis,
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
  /** Provenance for this priced amount — see rates.rs PricingBasis. Computed
   * by `resolveModelPricing`, the same resolver the rate-card contract uses
   * everywhere, so no call site re-derives direct/alias/fallback logic. */
  basis: PricingBasis;
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

/** Resolves the effective cache-creation rate: the published premium when
 * stated, otherwise the ordinary `input` rate — exactly today's pre-#42
 * accounting for a model with no published cache-write premium. Never zero
 * by default; `rate.cache_creation_input` must stay `null`/absent, never
 * default to `0`, or "unknown" silently becomes "free". Mirrors
 * `ModelRate::cache_creation_rate` in rates.rs — keep both in sync. */
export function cacheCreationRate(rate: ModelRate): number {
  return rate.cache_creation_input ?? rate.input;
}

/** True when `cacheCreationRate` is falling back to the ordinary input rate
 * rather than pricing at a directly published cache-write premium. Mirrors
 * `ModelRate::cache_creation_rate_is_fallback` in rates.rs. */
export function cacheCreationRateIsFallback(rate: ModelRate): boolean {
  return rate.cache_creation_input === null || rate.cache_creation_input === undefined;
}

/** A `direct`/`aliased` model resolution is only as authoritative as its
 * least-authoritative dimension. When the priced usage actually carries
 * cache-creation tokens and this model's rate has no published cache-write
 * premium, the resulting charge is partly a fallback-priced estimate, not a
 * direct price — downgrade so the UI doesn't render it as one. Other bases
 * (`fallback`, `unavailable`, `free_local`, `stale`, ...) already carry
 * their own, more specific provenance and are left untouched. */
function downgradeForCacheCreationFallback(
  basis: PricingBasis,
  rate: ModelRate,
  cacheCreationTokens: number,
): PricingBasis {
  if (
    (basis === 'direct' || basis === 'aliased' || basis === 'floating_alias')
    && cacheCreationTokens > 0
    && cacheCreationRateIsFallback(rate)
  ) {
    return 'estimated';
  }
  return basis;
}

/** Cost of one event's token delta under a given rate. Cached-read and
 * cache-creation are both disjoint subsets of input_tokens (never the same
 * tokens counted twice), and reasoning is a subset of output_tokens. */
function eventCost(delta: TokenTotals, rate: ModelRate, multiplier = 1): number {
  const nonCachedInput = Math.max(
    0,
    delta.input_tokens - delta.cached_input_tokens - delta.cache_creation_input_tokens,
  );
  const nonReasoningOutput = Math.max(0, delta.output_tokens - delta.reasoning_output_tokens);
  return (
    (nonCachedInput * rate.input +
      delta.cached_input_tokens * rate.cached_input +
      delta.cache_creation_input_tokens * cacheCreationRate(rate) +
      nonReasoningOutput * rate.output +
      delta.reasoning_output_tokens * rate.reasoning) /
    1_000_000
  ) * multiplier;
}

/**
 * Resolves a raw model id through `model_aliases` before any fallback path.
 * A chain can never take more steps than the table has entries — a longer
 * walk means a cycle, and resolution stops instead of looping forever.
 */
function resolveAlias(model: string, aliases: Record<string, string>): { resolved: string; hopped: boolean } {
  let current = model;
  const seen = new Set<string>();
  let hopped = false;
  const maxSteps = Object.keys(aliases).length;
  for (let step = 0; step <= maxSteps; step += 1) {
    const next = aliases[current];
    if (next === undefined) break;
    if (seen.has(current)) break; // Cycle: `current` was already visited.
    seen.add(current);
    current = next;
    hopped = true;
  }
  return { resolved: current, hopped };
}

function cardFreshness(rates: RateCard, now: number): 'fresh' | 'stale' | 'unknown' {
  const lastSuccessAt = rates.refresh?.last_success_at;
  if (!lastSuccessAt) return 'unknown';
  const at = Date.parse(lastSuccessAt);
  if (!Number.isFinite(at)) return 'unknown';
  const ageSecs = (now - at) / 1000;
  return ageSecs <= (rates.refresh?.max_cache_age_secs ?? Number.POSITIVE_INFINITY) ? 'fresh' : 'stale';
}

/**
 * Whether a floating alias's trust window has passed as of `now` (issue
 * #177). `expires_at` is an inclusive `YYYY-MM-DD` UTC date, so the
 * comparison is made in UTC rather than local time — otherwise the same card
 * would expire on different days either side of the date line.
 *
 * An unparseable date is treated as expired: refusing to trust a mapping we
 * cannot date is the safe direction, since the failure it guards against is
 * pricing silently against a stale target.
 */
export function floatingAliasExpired(alias: FloatingAlias, now: number = Date.now()): boolean {
  const expires = Date.parse(`${alias.expires_at}T23:59:59.999Z`);
  if (!Number.isFinite(expires)) return true;
  return now > expires;
}

/**
 * Resolves a raw model id to a rate-table key and records why that key was
 * chosen — the one place every surface should call instead of re-deriving
 * direct/alias/fallback lookup (mirrors `RateCard::resolve_model_pricing` in
 * rates.rs exactly; keep both in sync).
 *
 * Order: explicit free/local declaration, explicit known-unpriced
 * declaration, direct match, alias match, harness (or card-wide) fallback,
 * else unavailable. A resolution that would otherwise be
 * direct/aliased/fallback downgrades to `stale` when the card's own refresh
 * bookkeeping is older than its configured bound.
 */
export function resolveModelPricing(
  rates: RateCard,
  model: string,
  harness: Harness,
  table: Record<string, ModelRate>,
  now: number = Date.now(),
): { resolvedModel: string; basis: PricingBasis } {
  const downgradeIfStale = (basis: PricingBasis): PricingBasis =>
    cardFreshness(rates, now) === 'stale' ? 'stale' : basis;

  if ((rates.free_local_models ?? []).includes(model)) {
    return { resolvedModel: model, basis: 'free_local' };
  }
  if (isUnpricedModel(rates, model)) {
    return { resolvedModel: model, basis: 'unavailable' };
  }
  if (Object.hasOwn(table, model)) {
    return { resolvedModel: model, basis: downgradeIfStale('direct') };
  }
  // Before `model_aliases`, and only while unexpired: past its date the
  // mapping is deliberately ignored so resolution reaches the fallback below
  // and the UI's warning fires, rather than pricing a repointed alias
  // against its old target with provenance that raises nothing (issue #177).
  const floating = (rates.floating_model_aliases ?? {})[model];
  if (
    floating
    && !floatingAliasExpired(floating, now)
    && Object.hasOwn(table, floating.target)
  ) {
    return { resolvedModel: floating.target, basis: downgradeIfStale('floating_alias') };
  }
  const { resolved, hopped } = resolveAlias(model, rates.model_aliases ?? {});
  if (hopped && Object.hasOwn(table, resolved)) {
    return { resolvedModel: resolved, basis: downgradeIfStale('aliased') };
  }
  const fallbackModel = fallbackModelFor(rates, harness);
  if (Object.hasOwn(table, fallbackModel)) {
    return { resolvedModel: fallbackModel, basis: downgradeIfStale('fallback') };
  }
  return { resolvedModel: model, basis: 'unavailable' };
}

/** Converts one original-currency amount using a user-supplied conversion.
 * Odometer performs no FX fetch — `conversion` is exactly what the user
 * entered, or null/absent, in which case the caller must show the original
 * currency rather than inventing a rate. Returns null for a non-finite or
 * non-positive rate. */
export function convertDisplayCurrency(
  amountInOriginalCurrency: number,
  conversion: CurrencyConversion | null | undefined,
): number | null {
  if (!conversion || !Number.isFinite(conversion.rate) || conversion.rate <= 0) return null;
  return amountInOriginalCurrency * conversion.rate;
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
): { cost: number; fallbackUsed: boolean; unpriced: boolean; basis: PricingBasis } {
  if (isUnpricedModel(rates, model)) {
    return { cost: 0, fallbackUsed: false, unpriced: true, basis: 'unavailable' };
  }
  if (model === null) {
    return { cost: 0, fallbackUsed: false, unpriced: false, basis: 'unavailable' };
  }
  const { resolvedModel, basis } = resolveModelPricing(rates, model, harness, table);
  const rate = table[resolvedModel];
  const fallbackUsed = basis === 'fallback';
  if (!rate) return { cost: 0, fallbackUsed, unpriced: false, basis };
  return {
    cost: eventCost(tokens, rate, serviceTierMultiplier(model, serviceTier)),
    fallbackUsed,
    unpriced: false,
    basis,
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
  return bucketsCost(buckets, rates, harness, rates.models);
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
  if (!hasApiPricing(rates, harness)) return null;
  return bucketsCost(buckets, rates, harness, rates.api_models);
}

function bucketsCost(
  buckets: TierBucket[],
  rates: RateCard,
  harness: Harness,
  table: Record<string, ModelRate>,
): SessionCredits {
  const byModelMap = new Map<string, number>();
  const basisByModel = new Map<string, PricingBasis>();
  const missingModels = new Set<string>();
  const unpricedModels = new Set<string>();

  let total = 0;

  for (const b of buckets) {
    const { resolvedModel, basis } = resolveModelPricing(rates, b.model, harness, table);
    if (basis === 'unavailable' && isUnpricedModel(rates, b.model)) {
      basisByModel.set(b.model, basis);
      unpricedModels.add(b.model);
      byModelMap.set(b.model, byModelMap.get(b.model) ?? 0);
      continue;
    }
    if (basis === 'fallback' || basis === 'unavailable') missingModels.add(b.model);
    const rate = table[resolvedModel];
    if (!rate) {
      if (basisByModel.get(b.model) !== 'estimated') basisByModel.set(b.model, basis);
      // A free/local model has no rate row by design, and its cost is a
      // declared zero rather than an absence. Without an entry here it
      // vanished from the per-model breakdown entirely, so usage on a
      // locally hosted model was invisible in the table that is supposed to
      // account for it (issue #47). Genuinely unpriceable models stay out:
      // they are already surfaced through `missingModels`, and a 0 row would
      // read as "this was free".
      if (basis === 'free_local') byModelMap.set(b.model, byModelMap.get(b.model) ?? 0);
      continue;
    }
    // A model can appear in multiple buckets (one per service tier); once
    // any bucket downgrades to `estimated`, a later, cleaner bucket for the
    // same model must not paper back over it.
    if (basisByModel.get(b.model) !== 'estimated') {
      basisByModel.set(
        b.model,
        downgradeForCacheCreationFallback(basis, rate, b.tokens.cache_creation_input_tokens),
      );
    }

    const cost = eventCost(b.tokens, rate, serviceTierMultiplier(b.model, b.service_tier));
    total += cost;
    byModelMap.set(b.model, (byModelMap.get(b.model) ?? 0) + cost);
  }

  return {
    total,
    byModel: Array.from(byModelMap, ([model, cost]) => ({
      model,
      cost,
      fallbackUsed: basisByModel.get(model) === 'fallback',
      unpriced: unpricedModels.has(model),
      basis: basisByModel.get(model) ?? 'unavailable',
    })),
    missingModels: Array.from(missingModels),
    unpricedModels: Array.from(unpricedModels),
  };
}

/**
 * Whether API-rate pricing applies at all.
 *
 * `api_models` holds OpenAI API USD rates for Codex models, so it is only
 * meaningful for a Codex session. Without the harness check, any harness
 * priced against that table whenever it was non-empty — so a Claude session
 * could be costed from OpenAI's price list and reported as a `direct` match,
 * which raises no warning in the UI at all (issue #47).
 *
 * `usesApiPricing` in `sessionProjection.ts` already encoded this rule, and
 * most call sites went through it. Enforcing it here means a caller cannot
 * skip it — which is exactly how the two drifted apart. Mirrors
 * `query::price_tokens`'s `RateTable::Api` guard in the backend.
 */
function hasApiPricing(rates: RateCard, harness: Harness): boolean {
  return harness === 'codex' && Object.keys(rates.api_models ?? {}).length > 0;
}

/** All-time credits for a list-view summary. */
export function computeSummaryCredits(summary: SessionSummary, rates: RateCard): SessionCredits {
  return creditsFromBuckets(summary.buckets, rates, summary.harness);
}

/** All-time OpenAI-API-rate cost for a full session (drawer). Null when unconfigured. */
export function computeSessionApiCost(session: Session, rates: RateCard): SessionCredits | null {
  if (!hasApiPricing(rates, session.harness)) return null;
  return historyCost(session, rates, rates.api_models);
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
    : historyCost(session, rates, rates.models);
  if (session.tokens_history.length === 0 || rates.pricing_catalog.rate_periods.length === 0) {
    return { flat, timeAware: null };
  }

  const surface = apiSurfaceForHarness(session.harness);
  const byModelMap = new Map<string, number>();
  const basisByModel = new Map<string, PricingBasis>();
  const periodIds = new Set<string>();
  const modifierIds = new Set<string>();
  const missingConditionalEvidence = new Set<string>();
  const unpriced = new Set(rates.unpriced_models ?? []);
  const cacheWriteMultipliers = new Set<number>();
  // True once any event in this session reports cache-creation tokens under
  // a period that declares a cache_write_input_multiplier — at that point
  // the premium is priced directly via period.rate.cache_creation_input
  // rather than remaining purely documented, unobserved metadata.
  let cacheCreationObserved = false;
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
    const nonCachedInput = Math.max(
      0,
      event.delta.input_tokens - event.delta.cached_input_tokens - event.delta.cache_creation_input_tokens,
    );
    const nonReasoningOutput = Math.max(0, event.delta.output_tokens - event.delta.reasoning_output_tokens);
    const cost = (
      (nonCachedInput * period.rate.input * inputMultiplier)
      + (event.delta.cached_input_tokens * period.rate.cached_input * inputMultiplier)
      + (event.delta.cache_creation_input_tokens * cacheCreationRate(period.rate) * inputMultiplier)
      + (nonReasoningOutput * period.rate.output * outputMultiplier)
      + (event.delta.reasoning_output_tokens * period.rate.reasoning * outputMultiplier)
    ) / 1_000_000 * serviceTierMultiplier(event.model, event.service_tier);
    total += cost;
    byModelMap.set(event.model, (byModelMap.get(event.model) ?? 0) + cost);
    // A period with a direct catalog match is normally `direct`, but when
    // its cache-creation rate is itself falling back to the ordinary input
    // rate (no published premium) and this event actually carries
    // cache-creation tokens, that charge is not a directly published price
    // — downgrade to `estimated` so the UI doesn't render it as one. Once a
    // model is marked `estimated` a later, cleaner event must not paper
    // over it.
    const eventBasis: PricingBasis =
      cacheCreationRateIsFallback(period.rate) && event.delta.cache_creation_input_tokens > 0
        ? 'estimated'
        : 'direct';
    if (basisByModel.get(event.model) !== 'estimated') {
      basisByModel.set(event.model, eventBasis);
    }
    periodIds.add(period.id);
    if (period.cache_write_input_multiplier !== null && period.cache_write_input_multiplier !== undefined) {
      cacheWriteMultipliers.add(period.cache_write_input_multiplier);
      if (event.delta.cache_creation_input_tokens > 0) cacheCreationObserved = true;
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
        basis: basisByModel.get(model) ?? 'direct',
      })),
      missingModels: [],
      unpricedModels: [],
      surface,
      appliedRatePeriods: Array.from(periodIds),
      appliedModifiers: Array.from(modifierIds),
      conditionalEvidenceMissing: Array.from(missingConditionalEvidence),
      cacheWritePricingUnmodeled: cacheWriteMultipliers.size > 0 && !cacheCreationObserved,
      unobservedCacheWriteInputMultipliers: Array.from(cacheWriteMultipliers),
    },
  };
}

export function computeSessionCredits(session: Session, rates: RateCard): SessionCredits {
  if (session.tokens_history.length > 0) {
    return historyCost(session, rates, rates.models);
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
    const { resolvedModel, basis } = resolveModelPricing(rates, model, session.harness, rates.models);
    if (basis === 'unavailable' && isUnpricedModel(rates, model)) {
      unpricedModels.push(model);
      byModel.push({ model, cost: 0, fallbackUsed: false, unpriced: true, basis });
      continue;
    }
    const fallbackUsed = basis === 'fallback';
    if (fallbackUsed || basis === 'unavailable') {
      missingModels.push(model);
    }

    const rate = rates.models[resolvedModel];

    if (!rate) {
      // Neither the model, an alias, nor the fallback exists in the rate card.
      byModel.push({ model, cost: 0, fallbackUsed, unpriced: false, basis });
      continue;
    }
    const resolvedBasis = downgradeForCacheCreationFallback(
      basis,
      rate,
      totals.cache_creation_input_tokens,
    );

    const cost = eventCost(totals, rate);

    total += cost;
    byModel.push({ model, cost, fallbackUsed, unpriced: false, basis: resolvedBasis });
  }

  return { total, byModel, missingModels, unpricedModels };
}

function historyCost(
  session: Session,
  rates: RateCard,
  table: Record<string, ModelRate>,
): SessionCredits {
  const byModelMap = new Map<string, number>();
  const basisByModel = new Map<string, PricingBasis>();
  const missingModels = new Set<string>();
  const unpricedModels = new Set<string>();
  let total = 0;

  for (const ev of session.tokens_history) {
    if (!ev.model) continue;
    const { resolvedModel, basis } = resolveModelPricing(rates, ev.model, session.harness, table);
    if (basis === 'unavailable' && isUnpricedModel(rates, ev.model)) {
      basisByModel.set(ev.model, basis);
      unpricedModels.add(ev.model);
      byModelMap.set(ev.model, byModelMap.get(ev.model) ?? 0);
      continue;
    }
    if (basis === 'fallback' || basis === 'unavailable') missingModels.add(ev.model);
    const rate = table[resolvedModel];
    if (!rate) {
      if (basisByModel.get(ev.model) !== 'estimated') basisByModel.set(ev.model, basis);
      // Same declared-zero rule as `bucketsCost` — the per-event path had
      // the identical omission.
      if (basis === 'free_local') byModelMap.set(ev.model, byModelMap.get(ev.model) ?? 0);
      continue;
    }
    // Multiple events can share a model across a session's history; once any
    // one of them downgrades to `estimated` a later, cleaner event must not
    // paper back over it.
    if (basisByModel.get(ev.model) !== 'estimated') {
      basisByModel.set(
        ev.model,
        downgradeForCacheCreationFallback(basis, rate, ev.delta.cache_creation_input_tokens),
      );
    }

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
      fallbackUsed: basisByModel.get(model) === 'fallback',
      unpriced: unpricedModels.has(model),
      basis: basisByModel.get(model) ?? 'unavailable',
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
