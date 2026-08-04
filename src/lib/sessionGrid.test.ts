import { describe, expect, it } from 'vitest';
import { formatStartedLocal, formatTokenCategory, formatTokenTotal, modelProviderVisual } from './sessionGrid';
import { exportRows, projectSession, repositoryLabel, zeroToolMetrics, zeroTotals } from './sessionProjection';
import type { RangeTotals, SessionSummary, TokenTotals } from './types';

describe('session grid formatting', () => {
  it('uses local wall-clock time and honors daylight-saving transitions', () => {
    expect(formatStartedLocal(Date.parse('2026-03-08T06:59:59Z'), 'en-US', 'America/New_York'))
      .toBe('Mar 8, 2026, 1:59 AM');
    expect(formatStartedLocal(Date.parse('2026-03-08T07:00:00Z'), 'en-US', 'America/New_York'))
      .toBe('Mar 8, 2026, 3:00 AM');
  });

  it('keeps corpus-scale totals inside a numeric column', () => {
    // Per-row magnitudes stay fully grouped...
    expect(formatTokenTotal(0, 'en-US')).toBe('—');
    expect(formatTokenTotal(12_345, 'en-US')).toBe('12,345');
    expect(formatTokenTotal(999_999_999, 'en-US')).toBe('999,999,999');
    // ...while corpus totals compact rather than overflow into the next cell.
    expect(formatTokenTotal(1_000_000_000, 'en-US')).toBe('1B');
    expect(formatTokenTotal(652_372_161_231, 'en-US')).toBe('652.37B');
  });

  it('distinguishes unavailable token categories from measured values', () => {
    expect(formatTokenCategory(0, 'en-US')).toBe('—');
    expect(formatTokenCategory(12_345, 'en-US')).toBe('12,345');
  });

  it('maps model providers to a bounded palette with harness fallback', () => {
    expect(modelProviderVisual({ harness: 'codex', model_provider: 'openai' })).toMatchObject({ key: 'openai', label: 'OpenAI' });
    expect(modelProviderVisual({ harness: 'claude_code', model_provider: null })).toMatchObject({ key: 'anthropic', label: 'Anthropic' });
    expect(modelProviderVisual({ harness: 'codex', model_provider: 'gemini' })).toMatchObject({ key: 'google', label: 'Google' });
    expect(modelProviderVisual({ harness: 'codex', model_provider: 'local-provider' })).toMatchObject({ key: 'other', label: 'local-provider' });
  });

  it('derives the fallback label from the final path segment', () => {
    expect(repositoryLabel({ working_directory: 'C:\\workspace\\sample-repository' })).toBe('sample-repository');
    expect(repositoryLabel({ working_directory: '/home/demo/work/repository' })).toBe('repository');
    expect(repositoryLabel({ working_directory: null })).toBeNull();
  });

  it('uses range token categories when the grid is date scoped', () => {
    const allTime: TokenTotals = { input_tokens: 100, cached_input_tokens: 40, cache_creation_input_tokens: 0, output_tokens: 20, reasoning_output_tokens: 5, total_tokens: 120 };
    const scoped: TokenTotals = { input_tokens: 12, cached_input_tokens: 4, cache_creation_input_tokens: 0, output_tokens: 3, reasoning_output_tokens: 1, total_tokens: 15 };
    const session: SessionSummary = {
      id: 'session', storage_id: 'codex:thread:session', harness: 'codex', thread_name: null, forked_from_id: null,
      parent_thread_id: null, agent_path: null, agent_nickname: null, file_path: 'fixture.jsonl', source_availability: 'present',
      archived: false, started_at: '2026-01-01T00:00:00Z', last_event_at: '2026-01-01T00:01:00Z',
      working_directory: null, originator: null, source: null, cli_version: null, model: null,
      model_provider: null,
      service_tier: null, plan_type: null, credits_unlimited: null, credits_balance: null,
      context_window: null, total_turns: 0, first_user_message: null, tokens_total: allTime,
      buckets: [], tool_metrics: zeroToolMetrics(), tool_metrics_by_model: {}, category_totals: {},
      optimization_findings_count: 0,
      project_key: null, project_label: null, project_provenance: null,
    };
    const range: RangeTotals = {
      tokens: scoped,
      buckets: [],
      tool_metrics: zeroToolMetrics(),
      tool_metrics_by_model: {},
      optimization_findings_count: 0,
    };

    expect(projectSession(session, null, range, true).tokens).toEqual(scoped);
    expect(projectSession(session, null, undefined, true).tokens).toEqual(zeroTotals());
    expect(projectSession(session, null, range, false).tokens).toEqual(allTime);
    expect(exportRows([projectSession(session, null, undefined, false)])[0]).toMatchObject({
      id: 'session',
      storage_id: 'codex:thread:session',
      source_availability: 'present',
      parent_thread_id: null,
      codex_time_aware_api_usd: null,
      codex_time_aware_api_status: null,
      time_aware_api_status: null,
      pricing_catalog_available: false,
      rate_card_version: null,
      rate_card_fetched_at: null,
    });
  });
});
