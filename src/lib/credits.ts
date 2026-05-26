import type { RateCard, Session } from './types';

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

    const cost =
      (totals.input_tokens * rate.input +
        totals.cached_input_tokens * rate.cached_input +
        totals.output_tokens * rate.output +
        totals.reasoning_output_tokens * rate.reasoning) /
      1_000_000;

    total += cost;
    byModel.push({ model, cost, fallbackUsed });
  }

  return { total, byModel, missingModels };
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
