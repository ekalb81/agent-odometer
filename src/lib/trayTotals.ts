// Today's tray totals, computed from cached per-session range rollups.
// Extracted from the App tray effect so the summing logic is unit-testable
// and can rerun on rate-card changes without refetching range data.

import { apiCostFromBuckets, creditsFromBuckets, formatCredits } from './credits';
import type { Harness, RangeTotals, RateCard } from './types';

export interface TrayTotals {
  tokens: string;
  codex_credits: string;
  codex_api_usd: string;
  claude_usd: string;
  /** Live-quota headroom label (issue #43), e.g. "codex 5h 37% left". Empty
   *  string when nothing is available to show — the tray keeps its
   *  placeholder text rather than rendering a blank menu item. The label
   *  itself is formatted by `quotaTrayLabel` (lib/subscriptionUsage.ts) from
   *  already-backend-computed numbers; this module never re-derives one. */
  quota: string;
}

export interface TraySessionLike {
  storage_id: string;
  harness: Harness;
  credits_unlimited: boolean | null;
}

export function computeTrayTotals(
  sessions: Iterable<TraySessionLike>,
  ranges: Record<string, RangeTotals>,
  rateCard: RateCard,
  quotaLabel: string | null = null,
): TrayTotals {
  let tokens = 0; let codexCredits = 0; let codexApi = 0; let claudeUsd = 0;
  let unlimited = 0; let missingCredits = false; let missingApi = false; let missingClaude = false;
  let unpricedCredits = false; let unpricedApi = false; let unpricedClaude = false;
  for (const session of sessions) {
    const range = ranges[session.storage_id]; if (!range) continue;
    tokens += range.tokens.total_tokens;
    const plan = creditsFromBuckets(range.buckets, rateCard, session.harness);
    if (session.harness === 'codex') {
      if (session.credits_unlimited) unlimited++; else codexCredits += plan.total;
      missingCredits ||= plan.missingModels.length > 0;
      unpricedCredits ||= plan.unpricedModels.length > 0;
      const api = apiCostFromBuckets(range.buckets, rateCard, session.harness);
      codexApi += api?.total ?? 0; missingApi ||= !api || api.missingModels.length > 0;
      unpricedApi ||= (api?.unpricedModels.length ?? 0) > 0;
    } else {
      claudeUsd += plan.total; missingClaude ||= plan.missingModels.length > 0;
      unpricedClaude ||= plan.unpricedModels.length > 0;
    }
  }
  const creditText = unlimited > 0 && codexCredits === 0 ? `unlimited (${unlimited})` : `${codexCredits.toFixed(2)}${unlimited ? ` + ${unlimited} unlimited` : ''}${unpricedCredits ? ' · excludes unpriced' : missingCredits ? ' · fallback' : ''}`;
  return {
    tokens: tokens.toLocaleString(),
    codex_credits: creditText,
    codex_api_usd: missingApi ? 'unavailable · missing direct rate' : `${formatCredits(codexApi, 'USD')}${unpricedApi ? ' · excludes unpriced' : ''}`,
    claude_usd: `${formatCredits(claudeUsd, 'USD')}${unpricedClaude ? ' · excludes unpriced' : missingClaude ? ' · fallback' : ''}`,
    quota: quotaLabel ?? '',
  };
}
