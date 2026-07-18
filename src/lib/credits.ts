import type {
  Harness,
  ModelRate,
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
}

export interface SessionCredits {
  total: number;
  byModel: ModelCredit[];
  missingModels: string[];
}

/** Currency label for a harness, falling back to the card-wide currency. */
export function harnessCurrency(rates: RateCard, harness: Harness): string {
  return rates.currencies?.[harness] ?? rates.currency;
}

/** Fallback model for a harness, falling back to the card-wide fallback. */
function fallbackModelFor(rates: RateCard, harness: Harness): string {
  return rates.fallback_models?.[harness] ?? rates.fallback_model;
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
 * UI can flag it.
 */
export function tokensCost(
  tokens: TokenTotals,
  model: string | null,
  rates: RateCard,
  serviceTier: string | null = null,
  harness: Harness = 'codex',
): { cost: number; fallbackUsed: boolean } {
  const directRate = model ? rates.models[model] : undefined;
  const fallbackUsed = directRate === undefined;
  const rate = directRate ?? rates.models[fallbackModelFor(rates, harness)];
  if (!rate) return { cost: 0, fallbackUsed };
  return {
    cost: eventCost(tokens, rate, serviceTierMultiplier(model, serviceTier)),
    fallbackUsed,
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
  const byModelMap = new Map<string, number>();
  const missingModels = new Set<string>();
  let total = 0;

  const fallbackRate = rates.models[fallbackModelFor(rates, harness)];

  for (const b of buckets) {
    const directRate = rates.models[b.model];
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
      fallbackUsed: rates.models[model] === undefined,
    })),
    missingModels: Array.from(missingModels),
  };
}

/** All-time credits for a list-view summary. */
export function computeSummaryCredits(summary: SessionSummary, rates: RateCard): SessionCredits {
  return creditsFromBuckets(summary.buckets, rates, summary.harness);
}

export function computeSessionCredits(session: Session, rates: RateCard): SessionCredits {
  if (session.tokens_history.length > 0) {
    return computeHistoryCredits(session, rates, null, null);
  }
  const entries = Object.entries(session.tokens_by_model);

  if (entries.length === 0) {
    return { total: 0, byModel: [], missingModels: [] };
  }

  const byModel: ModelCredit[] = [];
  const missingModels: string[] = [];
  let total = 0;

  for (const [model, totals] of entries) {
    const directRate = rates.models[model];
    const fallbackRate = rates.models[fallbackModelFor(rates, session.harness)];
    const fallbackUsed = directRate === undefined;

    if (fallbackUsed) {
      missingModels.push(model);
    }

    const rate = directRate ?? fallbackRate;

    if (!rate) {
      // Neither the model nor the fallback exists in the rate card.
      byModel.push({ model, cost: 0, fallbackUsed });
      continue;
    }

    const cost = eventCost(totals, rate);

    total += cost;
    byModel.push({ model, cost, fallbackUsed });
  }

  return { total, byModel, missingModels };
}

function computeHistoryCredits(
  session: Session,
  rates: RateCard,
  fromIso: string | null,
  toIso: string | null,
): SessionCredits {

  const byModelMap = new Map<string, number>();
  const missingModels = new Set<string>();
  let total = 0;

  const fallbackRate = rates.models[fallbackModelFor(rates, session.harness)];

  for (const ev of session.tokens_history) {
    if (fromIso && ev.timestamp < fromIso) continue;
    if (toIso && ev.timestamp > toIso) continue;
    if (!ev.model) continue;

    const directRate = rates.models[ev.model];
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
      fallbackUsed: rates.models[model] === undefined,
    })),
    missingModels: Array.from(missingModels),
  };
}

const ISO_CURRENCY = /^[A-Z]{3}$/;

/**
 * Formats a credit amount per the rate card's `currency` field. If `currency`
 * looks like an ISO 4217 code (e.g. "USD"), uses Intl currency formatting.
 * Otherwise (e.g. "credits"), formats as a plain decimal with the unit suffix.
 */
export function formatCredits(amount: number, currency: string): string {
  if (ISO_CURRENCY.test(currency)) {
    return new Intl.NumberFormat('en-US', {
      style: 'currency',
      currency,
      minimumFractionDigits: 2,
      maximumFractionDigits: 4,
    }).format(amount);
  }
  const num = new Intl.NumberFormat('en-US', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  }).format(amount);
  return `${num} ${currency}`;
}
