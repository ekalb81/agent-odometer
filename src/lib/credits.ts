import type { ModelRate, RateCard, Session, TokenTotals } from './types';

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
 * Sums the per-event deltas from `tokens_history` whose date falls within
 * [fromIso, toIso] (inclusive). When both bounds are null, returns the
 * cumulative `tokens_total` directly — same number as the session header.
 */
export function tokensInRange(
  session: Session,
  fromIso: string | null,
  toIso: string | null,
): TokenTotals {
  if (!fromIso && !toIso) return session.tokens_total;
  const acc = emptyTotals();
  for (const ev of session.tokens_history) {
    const date = ev.timestamp.slice(0, 10);
    if (fromIso && date < fromIso) continue;
    if (toIso && date > toIso) continue;
    addTokens(acc, ev.delta);
  }
  return acc;
}

/** Cost of one event's token delta under a given rate, with OpenAI subset semantics. */
function eventCost(delta: TokenTotals, rate: ModelRate): number {
  const nonCachedInput = Math.max(0, delta.input_tokens - delta.cached_input_tokens);
  const nonReasoningOutput = Math.max(0, delta.output_tokens - delta.reasoning_output_tokens);
  return (
    (nonCachedInput * rate.input +
      delta.cached_input_tokens * rate.cached_input +
      nonReasoningOutput * rate.output +
      delta.reasoning_output_tokens * rate.reasoning) /
    1_000_000
  );
}

export function computeSessionCredits(session: Session, rates: RateCard): SessionCredits {
  const entries = Object.entries(session.tokens_by_model);

  if (entries.length === 0) {
    return { total: 0, byModel: [], missingModels: [] };
  }

  const byModel: ModelCredit[] = [];
  const missingModels: string[] = [];
  let total = 0;

  for (const [model, totals] of entries) {
    const directRate = rates.models[model];
    const fallbackRate = rates.models[rates.fallback_model];
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
 * Computes credits for a session restricted to events whose date is within
 * [fromIso, toIso] (inclusive, "YYYY-MM-DD"; pass null for an open bound).
 * Walks tokens_history rather than the per-model buckets, so it can scope
 * the math to any sub-period of the session's lifetime.
 *
 * The date comparison uses each event's UTC date (timestamp.slice(0,10)) to
 * stay consistent with how the table's date filter compares started_at /
 * last_event_at.
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

  const byModelMap = new Map<string, number>();
  const missingModels = new Set<string>();
  let total = 0;

  const fallbackRate = rates.models[rates.fallback_model];

  for (const ev of session.tokens_history) {
    const date = ev.timestamp.slice(0, 10);
    if (fromIso && date < fromIso) continue;
    if (toIso && date > toIso) continue;
    if (!ev.model) continue;

    const directRate = rates.models[ev.model];
    const fallbackUsed = directRate === undefined;
    if (fallbackUsed) missingModels.add(ev.model);
    const rate = directRate ?? fallbackRate;
    if (!rate) continue;

    const cost = eventCost(ev.delta, rate);
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
