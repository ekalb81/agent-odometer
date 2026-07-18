import type { Harness, ModelRate, RateCard, Session, TokenTotals } from './types';

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

function emptyTotals(): TokenTotals {
  return {
    input_tokens: 0,
    cached_input_tokens: 0,
    output_tokens: 0,
    reasoning_output_tokens: 0,
    total_tokens: 0,
  };
}

function addTokens(acc: TokenTotals, src: TokenTotals): void {
  acc.input_tokens += src.input_tokens;
  acc.cached_input_tokens += src.cached_input_tokens;
  acc.output_tokens += src.output_tokens;
  acc.reasoning_output_tokens += src.reasoning_output_tokens;
  acc.total_tokens += src.total_tokens;
}

/**
 * Sums the per-event deltas from `tokens_history` whose timestamp falls
 * within [fromIso, toIso] (inclusive). Pass null for an open bound. When
 * both bounds are null, returns the cumulative `tokens_total` directly —
 * same number as the session header.
 *
 * Both bounds and event timestamps are expected to be full UTC ISO 8601
 * strings (e.g. "2026-05-26T19:30:00.000Z"); lexical comparison on this
 * format sorts chronologically.
 */
export function tokensInRange(
  session: Session,
  fromIso: string | null,
  toIso: string | null,
): TokenTotals {
  if (!fromIso && !toIso) return session.tokens_total;
  const acc = emptyTotals();
  for (const ev of session.tokens_history) {
    if (fromIso && ev.timestamp < fromIso) continue;
    if (toIso && ev.timestamp > toIso) continue;
    addTokens(acc, ev.delta);
  }
  return acc;
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

/**
 * Computes credits for a session restricted to events whose timestamp is
 * within [fromIso, toIso] (inclusive, full UTC ISO 8601; pass null for an
 * open bound). Walks tokens_history rather than the per-model buckets, so
 * it can scope the math to any sub-period of the session's lifetime —
 * down to the minute.
 */
export function computeSessionCreditsInRange(
  session: Session,
  rates: RateCard,
  fromIso: string | null,
  toIso: string | null,
): SessionCredits {
  if (!fromIso && !toIso) {
    return computeSessionCredits(session, rates);
  }

  return computeHistoryCredits(session, rates, fromIso, toIso);
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
