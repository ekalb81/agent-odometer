// Pure synthetic browser fixtures, shared with the offline Rust price generator.
// No private session data or frontend pricing engine is involved.
import type { Harness, RateCard, Session, SessionSummary, TierBucket, TokenTotals, TurnInfo, ToolMetrics } from '../lib/types';
import type { VisualScenario } from './visualScenario';

const DAY = 86_400_000;
// Codex does not report a cache-creation ("cache write") token category at
// all (see the bundled rates.json _note) — the parser can never produce a
// nonzero cache_creation_input_tokens for a codex session. A shared
// generator that gave every session cache-creation tokens regardless of
// harness was asserting something the real parser cannot produce, and it is
// exactly what let a Codex pricing regression hide inside what looked like
// "the new Claude cache-write premium" in a visual diff. Every caller must
// pass the session's harness explicitly — there is no default.
export function tok(total: number, harness: Harness): TokenTotals {
  const input = Math.round(total * 0.62);
  const cached = Math.round(total * 0.24);
  // Only Claude Code reports a cache-creation ("cache write") dimension;
  // Codex and Gemini CLI always report 0 (AGENTS.md).
  const cacheCreation = harness === 'claude_code' ? Math.round(total * 0.08) : 0;
  const output = total - input;
  const reasoning = Math.round(output * 0.3);
  return {
    input_tokens: input,
    cached_input_tokens: cached,
    cache_creation_input_tokens: cacheCreation,
    output_tokens: output,
    reasoning_output_tokens: reasoning,
    total_tokens: total,
  };
}

export function scaleTok(t: TokenTotals, f: number): TokenTotals {
  return {
    input_tokens: Math.round(t.input_tokens * f),
    cached_input_tokens: Math.round(t.cached_input_tokens * f),
    cache_creation_input_tokens: Math.round(t.cache_creation_input_tokens * f),
    output_tokens: Math.round(t.output_tokens * f),
    reasoning_output_tokens: Math.round(t.reasoning_output_tokens * f),
    total_tokens: Math.round(t.total_tokens * f),
  };
}

export function toolMetrics(calls = 0): ToolMetrics {
  return { calls, reads: Math.floor(calls / 3), searches: Math.floor(calls / 4),
    mutations: Math.floor(calls / 3), commands: Math.floor(calls / 4), other: 0,
    successes: calls, failures: 0, unknown: 0, mutation_targets: Math.floor(calls / 3),
    one_shot_mutations: Math.floor(calls / 3), retry_count: 0,
    duration_ms: calls * 400, output_bytes: calls * 250 };
}

export interface Fixture {
  id: string;
  harness: 'codex' | 'claude_code' | 'gemini_cli';
  name: string;
  started: string;
  hoursActive: number;
  model: string;
  total: number;
  turns: number;
  archived?: boolean;
  parent?: string;
  unlimited?: boolean;
  /** Reuses a Rust-priced fixture in stress mode. */
  pricingTemplate?: string;
}

export function createFixtureData(now: number, visualScenario: VisualScenario = 'default', stressN = 0, calendar: 'local' | 'UTC' = 'local') {
  function at(daysAgo: number, hours: number, minutes = 0): string {
    const d = new Date(now - daysAgo * DAY);
    if (calendar === 'UTC') d.setUTCHours(hours, minutes, 0, 0);
    else d.setHours(hours, minutes, 0, 0);
    return d.toISOString();
  }


  const FIXTURES: Fixture[] = [
    // Codex
    { id: 'caba26c9-41d2-4e08-9c1a-0000c0de0001', harness: 'codex', name: 'Add dark mode toggle', started: at(0, 8), hoursActive: 2.7, model: 'gpt-5.6-sol', total: 3_174_225, turns: 6, unlimited: true },
    { id: 'caba26c9-41d2-4e08-9c1a-0000c0de0002', harness: 'codex', name: 'Search cookbook data model', started: at(0, 6), hoursActive: 0.4, model: 'gpt-5.6-luna', total: 133_527, turns: 2, unlimited: true, parent: 'caba26c9-41d2-4e08-9c1a-0000c0de0001' },
    { id: 'caba26c9-41d2-4e08-9c1a-0000c0de0003', harness: 'codex', name: 'Parallelize tile renderer', started: at(1, 8), hoursActive: 5, model: 'gpt-5.6-sol', total: 6_021_290, turns: 9, unlimited: true },
    { id: 'caba26c9-41d2-4e08-9c1a-0000c0de0004', harness: 'codex', name: 'Fix flaky contact form test', started: at(1, 5), hoursActive: 1, model: 'gpt-5.6-terra', total: 498_383, turns: 4, unlimited: true },
    { id: 'caba26c9-41d2-4e08-9c1a-0000c0de0005', harness: 'codex', name: 'Slash command for standups', started: at(4, 10), hoursActive: 2, model: 'gpt-5.6-terra', total: 1_114_856, turns: 5, unlimited: true },
    { id: 'caba26c9-41d2-4e08-9c1a-0000c0de0006', harness: 'codex', name: 'Migrate backups to restic', started: at(5, 9), hoursActive: 6, model: 'gpt-5.6-sol', total: 4_366_318, turns: 11, unlimited: true },
    { id: 'caba26c9-41d2-4e08-9c1a-0000c0de0007', harness: 'codex', name: 'Investigate NaN in BVH split', started: at(6, 14), hoursActive: 3, model: 'gpt-5.6-sol', total: 1_380_049, turns: 7, unlimited: true },
    { id: 'caba26c9-41d2-4e08-9c1a-0000c0de0008', harness: 'codex', name: 'Import from paprika exports', started: at(9, 11), hoursActive: 1.5, model: 'gpt-5.6-luna', total: 492_776, turns: 3, unlimited: true, archived: true },
    // Claude Code
    { id: '11ad0994-22fa-41fc-8888-0000c1de0001', harness: 'claude_code', name: 'Dark mode palette sweep', started: at(0, 12, 2), hoursActive: 2, model: 'claude-fable-5', total: 3_842_110, turns: 7 },
    { id: '9fbad994-22fa-41fc-8888-0000c1de0002', harness: 'claude_code', name: 'Bitboard move generator', started: at(0, 9, 2), hoursActive: 1.5, model: 'claude-opus-4-8', total: 5_178_736, turns: 8 },
    { id: '9fbad994-22fa-41fc-8888-0000c1de0003', harness: 'claude_code', name: 'Run the test suite and summarize failures', started: at(0, 9, 17), hoursActive: 0.3, model: 'claude-haiku-4-5', total: 231_447, turns: 1, parent: '9fbad994-22fa-41fc-8888-0000c1de0002' },
    { id: '9fbad994-22fa-41fc-8888-0000c1de0004', harness: 'claude_code', name: 'Rewrite hero section copy', started: at(1, 6, 8), hoursActive: 1, model: 'claude-sonnet-5', total: 486_921, turns: 4 },
    { id: '9fbad994-22fa-41fc-8888-0000c1de0005', harness: 'claude_code', name: 'Ansible playbook for backups', started: at(4, 15), hoursActive: 2, model: 'claude-opus-4-8', total: 1_904_553, turns: 6 },
    { id: '9fbad994-22fa-41fc-8888-0000c1de0006', harness: 'claude_code', name: 'Perft debugging session', started: at(5, 13), hoursActive: 4, model: 'claude-fable-5', total: 1_922_308, turns: 9 },
    { id: '9fbad994-22fa-41fc-8888-0000c1de0007', harness: 'claude_code', name: 'Migrate to slash commands', started: at(8, 10), hoursActive: 1, model: 'claude-sonnet-5', total: 585_410, turns: 3 },
  ];

  // Issue #44: a genuine Gemini CLI session, added only for the
  // 'tool-dimensions' visual scenario so no other baseline's tab count or
  // session total shifts. Unlike the tool-dimensions.png predecessor (which
  // flipped Claude Code's capability flags — untrue, since provider.rs says
  // Claude Code supports both), this gives the panel a provider that
  // genuinely lacks mcp_dimension/shell_dimension (see the 'list_providers'
  // case below), so the "Unavailable" rendering in the baseline reflects a
  // real capability gap instead of a fixture-only override.
  if (visualScenario === 'tool-dimensions') {
    FIXTURES.push({
      id: 'ad0be994-33aa-4a1c-9999-0000cade0001',
      harness: 'gemini_cli',
      name: 'Summarize release notes',
      started: at(2, 11),
      hoursActive: 1,
      model: 'gemini-2.5-pro',
      total: 812_450,
      turns: 3,
    });
  }

  // Stress mode: `?stress=N` appends N synthetic sessions per harness so list
  // performance can be profiled at realistic history sizes. Dev-mock only.
  if (stressN > 0) {
    const NAMES = [
      'Refactor ingestion pipeline', 'Fix flaky auth test', 'Add CSV export', 'Profile slow query',
      'Migrate config format', 'Review PR feedback', 'Write release notes', 'Debug watcher leak',
      'Tune cache eviction', 'Port build to CI', 'Sketch onboarding flow', 'Harden error paths',
    ];
    for (const harness of ['codex', 'claude_code'] as const) {
      const templates = FIXTURES.filter(f => f.harness === harness);
      let parentId: string | null = null;
      for (let i = 0; i < stressN; i++) {
        const id = `57e55000-0000-4000-8000-${harness === 'codex' ? 'c0de' : 'c1de'}${String(i).padStart(8, '0')}`;
        // Every 40th session starts a cluster; the following ~30 rows become its
        // subagents, mimicking large fan-out sessions.
        const inCluster = parentId !== null && i % 40 !== 0;
        if (i % 40 === 0) parentId = id;
        const template = templates[i % templates.length];
        FIXTURES.push({
          ...template,
          pricingTemplate: pricingKey(template),
          model: fixtureModel(template),
          id,
          harness,
          name: `${NAMES[i % NAMES.length]} #${i}`,
          started: at(Math.floor(i / 30), 8 + (i % 12), (i * 7) % 60),
          hoursActive: 0.5 + (i % 5),
          archived: i % 17 === 0,
          parent: inCluster ? (parentId ?? undefined) : undefined,
          unlimited: harness === 'codex' ? true : undefined,
        });
      }
    }
  }

  function fixtureModel(f: Fixture): string {
    if (visualScenario !== 'sessions-availability-fallback') return f.model;
    if (f.id.endsWith('c0de0001')) return 'gpt-5.7-visual-preview';
    if (f.id.endsWith('c0de0004')) return 'gpt-5.3-codex-spark';
    return f.model;
  }

  function buckets(f: Fixture): TierBucket[] {
    return [{ model: fixtureModel(f), service_tier: null, tokens: tok(f.total, f.harness) }];
  }

  // Matches the Rust `storage_id_for_session` convention (model.rs): Codex
  // keeps its historical "thread" segment; every other provider, including
  // Gemini CLI, uses the generic "session" segment.
  function storageIdFor(f: Fixture): string {
    return f.harness === 'codex' ? `codex:thread:${f.id}` : `${f.harness}:session:${f.id}`;
  }

  function modelProviderFor(harness: Fixture['harness']): string {
    if (harness === 'codex') return 'openai';
    if (harness === 'claude_code') return 'anthropic';
    return 'google';
  }

  function contextWindowFor(harness: Fixture['harness']): number {
    if (harness === 'codex') return 272_000;
    if (harness === 'claude_code') return 200_000;
    return 1_000_000;
  }

  function summary(f: Fixture): SessionSummary {
    const model = fixtureModel(f);
    return {
      id: f.id,
      storage_id: storageIdFor(f),
      harness: f.harness,
      thread_name: f.name,
      forked_from_id: null,
      parent_thread_id: f.parent ?? null,
      agent_path: null,
      agent_nickname: null,
      file_path: `/home/dev/.sessions/${f.id}.jsonl`,
      source_availability: visualScenario === 'sessions-availability-fallback' && f.id.endsWith('c0de0001') ? 'missing' : 'present',
      archived: f.archived ?? false,
      started_at: f.started,
      last_event_at: new Date(new Date(f.started).getTime() + f.hoursActive * 3_600_000).toISOString(),
      working_directory: '/home/dev/projects/demo',
      // Every fixture shares one working directory, so they resolve to one
      // project — matches the 'resolve_projects' mock handler below.
      project_key: 'repo:demo-fixture',
      project_label: 'demo',
      project_provenance: 'repository_root',
      originator: f.harness === 'codex' ? 'chatgpt' : 'cli',
      source: f.parent ? 'subagent' : null,
      cli_version: '1.4.2',
      model_provider: modelProviderFor(f.harness),
      model,
      service_tier: null,
      plan_type: f.unlimited ? 'pro' : null,
      credits_unlimited: f.unlimited ?? null,
      credits_balance: null,
      context_window: contextWindowFor(f.harness),
      total_turns: f.turns,
      first_user_message: `${f.name} — please take a look.`,
      tokens_total: tok(f.total, f.harness),
      buckets: buckets(f),
      tool_metrics: toolMetrics(f.turns * 3),
      tool_metrics_by_model: { [model]: toolMetrics(f.turns * 3) },
      category_totals: { coding: { turns: f.turns, tokens: tok(f.total, f.harness), tool_calls: f.turns * 3, buckets: buckets(f) } },
      optimization_findings_count: 0,
      optimization_summary: { findings: 0, warnings: 0, likely_avoidable_calls: 0, by_rule: {} },
    };
  }

  const TURN_PROMPTS = [
    'The tests caught an edge case, take a look.',
    'One more pass for error handling please.',
    'Ship it - commit with a clear message.',
    'Can you profile the slow path first?',
    'Looks good, tighten up the naming.',
    'Add coverage for the empty case.',
  ];

  function details(f: Fixture): Session {
    const s = summary(f);
    const model = s.model ?? f.model;
    const startMs = new Date(f.started).getTime();
    const perTurn = Math.floor(f.total / f.turns);
    const turns: TurnInfo[] = Array.from({ length: f.turns }, (_, i) => {
      const tStart = startMs + (i * f.hoursActive * 3_600_000) / f.turns;
      const jitter = 0.5 + ((i * 2654435761) % 100) / 100; // deterministic variety
      return {
        turn_id: `${f.id}-t${i + 1}`,
        index: i + 1,
        model,
        reasoning_effort: null,
        collaboration_mode: null,
        service_tier: null,
        status: 'completed',
        abort_reason: null,
        started_at: new Date(tStart).toISOString(),
        completed_at: new Date(tStart + 240_000).toISOString(),
        duration_ms: 240_000,
        time_to_first_token_ms: 1800,
        user_message: TURN_PROMPTS[i % TURN_PROMPTS.length],
        last_agent_message: 'Done — summarized in the diff above.',
        tokens: tok(Math.round(perTurn * jitter), f.harness),
        tool_metrics: toolMetrics(3),
        classification: { version: 1, category: 'coding', confidence: 0.8, signals: ['fixture'] },
      };
    });
    let cumulative = 0;
    const history = turns.map((t) => {
      cumulative += t.tokens.total_tokens;
      return {
        timestamp: t.started_at!,
        model,
        service_tier: null,
        request_input_tokens: t.tokens.input_tokens,
        total_tokens: cumulative,
        delta: t.tokens,
      };
    });
    return {
      ...s,
      subagent_id_is_path_fallback: false,
      history_mode: null,
      memory_mode: null,
      model_provider: modelProviderFor(f.harness),
      latest_context_tokens: Math.round((s.context_window ?? 200_000) * 0.54),
      tokens_by_model: { [model]: tok(f.total, f.harness) },
      tokens_history: history,
      rate_limits_history: [],
      turns,
      tool_observations: [],
      optimization_findings: [],
    };
  }

  const RATES: RateCard = {
    version: 6,
    currency: 'credits',
    unit: 'per_1m_tokens',
    source_url: 'https://example.invalid/rates',
    fetched_at: new Date(now - 2 * DAY).toISOString(),
    models: {
      // Codex has no published cache-write premium (it never reports the
      // dimension at all) -- cache_creation_input stays null (absent), not 0.
      // null means "price at the ordinary input rate", not "free"; see
      // the Rust pricing service.
      'gpt-5.6-sol': { input: 1.1, cached_input: 0.11, cache_creation_input: null, output: 8.8, reasoning: 8.8 },
      'gpt-5.6-terra': { input: 0.5, cached_input: 0.05, cache_creation_input: null, output: 4, reasoning: 4 },
      'gpt-5.6-luna': { input: 0.15, cached_input: 0.015, cache_creation_input: null, output: 1.2, reasoning: 1.2 },
      'gpt-5.5': { input: 0.8, cached_input: 0.08, cache_creation_input: null, output: 6.4, reasoning: 6.4 },
      'claude-opus-4-8': { input: 3.2, cached_input: 0.32, cache_creation_input: 4, output: 16, reasoning: 16 },
      'claude-fable-5': { input: 2.4, cached_input: 0.24, cache_creation_input: 3, output: 12, reasoning: 12 },
      'claude-sonnet-5': { input: 1.2, cached_input: 0.12, cache_creation_input: 1.5, output: 6, reasoning: 6 },
      'claude-haiku-4-5': { input: 0.35, cached_input: 0.035, cache_creation_input: 0.4375, output: 1.75, reasoning: 1.75 },
    },
    fallback_model: 'gpt-5.6-sol',
    currencies: { codex: 'credits', claude_code: 'USD' },
    fallback_models: { codex: 'gpt-5.6-sol', claude_code: 'claude-sonnet-5' },
    api_models: {
      'gpt-5.6-sol': { input: 1.25, cached_input: 0.125, cache_creation_input: null, output: 10, reasoning: 10 },
      'gpt-5.6-terra': { input: 0.6, cached_input: 0.06, cache_creation_input: null, output: 4.8, reasoning: 4.8 },
      'gpt-5.6-luna': { input: 0.18, cached_input: 0.018, cache_creation_input: null, output: 1.44, reasoning: 1.44 },
      'gpt-5.5': { input: 1, cached_input: 0.1, cache_creation_input: null, output: 8, reasoning: 8 },
    },
    unpriced_models: ['gpt-5.3-codex-spark'],
    pricing_catalog: { rate_periods: [], conditional_modifiers: [], notes: [] },
    model_aliases: {},
    free_local_models: [],
    subscription_plans: {},
    display_currency: null,
    refresh: { last_success_at: null, last_attempt_at: null, last_failure_reason: null, max_cache_age_secs: 604_800 },
  };


  function pricingKey(f: Fixture): string {
    if (f.pricingTemplate) return f.pricingTemplate;
    return fixtureModel(f) === f.model ? f.id : `${f.id}/${fixtureModel(f)}`;
  }

  return { fixtures: FIXTURES, rates: RATES, summary, details, buckets, fixtureModel, pricingKey };
}

/** Stable synthetic input for the generator and the frontend freshness test. */
export function createPricingFixtureInput() {
  const now = Date.parse('2026-07-29T15:30:00.000Z');
  const rates = createFixtureData(now, 'default', 0, 'UTC').rates;
  const cases: Record<string, { summary: SessionSummary; detail: Session }> = {};
  for (const scenario of ['default', 'sessions-availability-fallback', 'tool-dimensions'] as const) {
    const data = createFixtureData(now, scenario, 0, 'UTC');
    for (const fixture of data.fixtures) {
      const key = data.pricingKey(fixture);
      cases[key] ??= { summary: data.summary(fixture), detail: data.details(fixture) };
    }
  }
  return { now: new Date(now).toISOString(), rates, cases };
}
