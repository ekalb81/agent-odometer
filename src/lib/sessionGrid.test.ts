import { describe, expect, it } from 'vitest';
import { formatStartedLocal, formatTokenCategory, groupSessionsByRepository, modelProviderVisual } from './sessionGrid';
import { exportRows, projectSession, repositoryLabel, zeroToolMetrics, zeroTotals } from './sessionProjection';
import type { RangeTotals, SessionSummary, TokenTotals } from './types';

describe('session grid formatting', () => {
  it('uses local wall-clock time and honors daylight-saving transitions', () => {
    expect(formatStartedLocal(Date.parse('2026-03-08T06:59:59Z'), 'en-US', 'America/New_York'))
      .toBe('Mar 8, 2026, 1:59:59 AM');
    expect(formatStartedLocal(Date.parse('2026-03-08T07:00:00Z'), 'en-US', 'America/New_York'))
      .toBe('Mar 8, 2026, 3:00:00 AM');
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

  it('derives a privacy-safe repository label without exposing the path', () => {
    expect(repositoryLabel({ working_directory: 'C:\\workspace\\sample-repository' })).toBe('sample-repository');
    expect(repositoryLabel({ working_directory: '/home/demo/work/repository' })).toBe('repository');
    expect(repositoryLabel({ working_directory: null })).toBeNull();
  });

  it('groups sessions by repository while keeping subagents with their parent', () => {
    const parent = { id: 'parent', parent_thread_id: null, working_directory: 'C:\\workspace\\alpha' };
    const child = { id: 'child', parent_thread_id: 'parent', working_directory: 'C:\\workspace\\beta' };
    const unscoped = { id: 'unscoped', parent_thread_id: null, working_directory: null };

    expect(groupSessionsByRepository([parent, child, unscoped])).toEqual([
      { label: 'alpha', sessions: [parent, child] },
      { label: 'No repository recorded', sessions: [unscoped] },
    ]);
  });

  it('keeps distinct repositories separate when their folder labels match', () => {
    const client = { id: 'client', parent_thread_id: null, working_directory: 'C:\\clients\\shared-name' };
    const personal = { id: 'personal', parent_thread_id: null, working_directory: 'D:\\personal\\shared-name' };
    const clientVariant = { id: 'client-variant', parent_thread_id: null, working_directory: 'c:/CLIENTS/shared-name/' };

    expect(groupSessionsByRepository([client, personal, clientVariant])).toEqual([
      { label: 'shared-name', sessions: [client, clientVariant] },
      { label: 'shared-name', sessions: [personal] },
    ]);
  });

  it('uses range token categories when the grid is date scoped', () => {
    const allTime: TokenTotals = { input_tokens: 100, cached_input_tokens: 40, output_tokens: 20, reasoning_output_tokens: 5, total_tokens: 120 };
    const scoped: TokenTotals = { input_tokens: 12, cached_input_tokens: 4, output_tokens: 3, reasoning_output_tokens: 1, total_tokens: 15 };
    const session: SessionSummary = {
      id: 'session', harness: 'codex', thread_name: null, forked_from_id: null,
      parent_thread_id: null, agent_path: null, agent_nickname: null, file_path: 'fixture.jsonl',
      archived: false, started_at: '2026-01-01T00:00:00Z', last_event_at: '2026-01-01T00:01:00Z',
      working_directory: null, originator: null, source: null, cli_version: null, model: null,
      model_provider: null,
      service_tier: null, plan_type: null, credits_unlimited: null, credits_balance: null,
      context_window: null, total_turns: 0, first_user_message: null, tokens_total: allTime,
      buckets: [], tool_metrics: zeroToolMetrics(), tool_metrics_by_model: {}, category_totals: {},
      optimization_findings_count: 0,
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
      parent_thread_id: null,
    });
  });
});
