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
