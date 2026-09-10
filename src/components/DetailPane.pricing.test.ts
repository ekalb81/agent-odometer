import { cleanup, render, screen } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import DetailPane from './DetailPane.svelte';
import { rates } from '../lib/stores/rates';
import { zeroTotals, zeroToolMetrics } from '../lib/sessionProjection';
import type { RateCard, Session, PricedSurface } from '../lib/types';

vi.mock('../lib/ipc', () => ({
  openTaskInChatGPT: vi.fn(), revealInFileManager: vi.fn(),
  resolveProjects: vi.fn().mockResolvedValue([]),
  reassignSessionProject: vi.fn(), clearSessionProjectOverride: vi.fn(),
}));

function surface(total: number): PricedSurface {
  return { total, by_model: [{ model: 'synthetic', cost: total, basis: 'direct', unpriced: false }], missing_models: [], unpriced_models: [] };
}

function session(): Session {
  const tokens = zeroTotals();
  // Zero tokens intentionally disagree with the supplied server prices.
  return {
    id: 'synthetic', storage_id: 'synthetic', harness: 'codex',
    thread_name: null, forked_from_id: null, parent_thread_id: null, agent_path: null, agent_nickname: null,
    file_path: 'synthetic.jsonl', source_availability: 'present', archived: false,
    working_directory: null, originator: null, source: null, subagent_id_is_path_fallback: false,
    history_mode: null, memory_mode: null, cli_version: null, model_provider: null, model: null,
    service_tier: null, plan_type: null, credits_balance: null, context_window: null, latest_context_tokens: null,
    total_turns: 1, first_user_message: 'Synthetic prompt', tool_observations: [], tool_metrics: zeroToolMetrics(),
    tool_metrics_by_model: {}, category_totals: {}, project_key: null, project_label: null, project_provenance: null,
    started_at: '2026-01-01T00:00:00Z', last_event_at: '2026-01-01T00:01:00Z',
    tokens_total: tokens, tokens_by_model: { synthetic: tokens },
    tokens_history: [], rate_limits_history: [], optimization_findings: [],
    turns: [{ turn_id: 'one', index: 1, model: 'synthetic', status: 'completed', tokens,
      reasoning_effort: null, collaboration_mode: null, service_tier: null, abort_reason: null,
      started_at: '2026-01-01T00:00:00Z', completed_at: null, duration_ms: null, time_to_first_token_ms: null,
      user_message: 'Synthetic prompt', last_agent_message: null, tool_metrics: zeroToolMetrics(),
      classification: { version: 1, category: 'other', confidence: 0, signals: [] } }],
    credits_unlimited: true,
    pricing: {
      plan: surface(17.25), flat_api: surface(42.5),
      turn_prices: { one: { plan: { cost: 17.25, fallback_used: false, unpriced: false, basis: 'direct' }, api: { cost: 42.5, fallback_used: false, unpriced: false, basis: 'direct' } } },
      time_aware_api: null,
    },
  };
}

beforeEach(() => { rates.set({
  version: 1, currency: 'credits', unit: 'per_1m_tokens', source_url: 'https://example.test', fetched_at: null,
  models: {}, fallback_model: 'synthetic', currencies: {}, fallback_models: {}, api_models: {}, unpriced_models: [],
  pricing_catalog: { notes: [], rate_periods: [], conditional_modifiers: [] }, model_aliases: {}, free_local_models: [],
  subscription_plans: {}, display_currency: null,
  refresh: { last_success_at: null, last_attempt_at: null, last_failure_reason: null, max_cache_age_secs: 1 },
} satisfies RateCard); });
afterEach(() => { cleanup(); rates.set(null); });

describe('DetailPane server pricing', () => {
  it('uses server API prices for headline and turns while retaining the plan model table and unlimited reference', () => {
    render(DetailPane, { session: session(), onclose: () => {} });
    expect(screen.getAllByText('$42.50').length).toBeGreaterThan(0);
    expect(screen.getByTitle('#1 · $42.50')).toBeInTheDocument();
    expect(screen.getByText('17.25 credits')).toBeInTheDocument();
    expect(screen.getByText(/Reference: 17.25 credits à-la-carte equivalent/)).toBeInTheDocument();
  });

  it('does not reconstruct absent server pricing from tokens or the local card', () => {
    const value = session();
    delete value.pricing;
    render(DetailPane, { session: value, onclose: () => {} });
    expect(screen.getByText('Unavailable')).toBeInTheDocument();
    expect(screen.queryByText('Cost per turn')).not.toBeInTheDocument();
    expect(screen.queryByText(/Flat OpenAI API reference/)).not.toBeInTheDocument();
  });

  it('falls back to the supplied plan surface when API pricing is unavailable', () => {
    const value = session();
    value.pricing!.flat_api = null;
    value.pricing!.turn_prices.one.api = null;
    render(DetailPane, { session: value, onclose: () => {} });
    expect(screen.getByTitle('#1 · 17.25 credits')).toBeInTheDocument();
    expect(screen.queryByText('$42.50')).not.toBeInTheDocument();
  });

  it('renders dated price evidence from the response without consulting local catalog rules', () => {
    const value = session();
    value.pricing!.time_aware_api = {
      ...surface(51.75), surface: 'openai_api_usd', applied_rate_periods: ['server-rule'], applied_modifiers: [],
      conditional_evidence_missing: ['missing-input-rule'], cache_write_pricing_unmodeled: true,
      unobserved_cache_write_input_multipliers: [1.25],
      rules: [{ id: 'server-rule', label: 'Server evidence only', from: '2026-01-01T00:00:00Z', to: null,
        provenance: { source_url: 'https://example.test/pricing', verified_at: '2026-01-02T00:00:00Z', evidence: 'Synthetic evidence', note: null } }],
    };
    render(DetailPane, { session: value, onclose: () => {} });
    expect(screen.getByText('$51.75')).toBeInTheDocument();
    expect(screen.getByText(/Server evidence only/)).toBeInTheDocument();
    expect(screen.getByText(/missing-input-rule/)).toBeInTheDocument();
    expect(screen.getByText(/Documented 1.25×/)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'source', hidden: true })).toHaveAttribute('href', 'https://example.test/pricing');
  });
});
