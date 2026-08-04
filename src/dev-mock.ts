// Fixture IPC for `npm run dev` in a plain browser (no native backend).
// Loaded only when import.meta.env.DEV is set and Tauri isn't present — see
// main.ts. Production builds tree-shake this module away entirely.

import { mockIPC } from '@tauri-apps/api/mocks';
import { isUpdaterVisualScenario, selectVisualScenario, type VisualScenario } from './dev-mock/visualScenario';
import type {
  DiagnosticsReport,
  ProviderDiagnostic,
  RangeTotals,
  RateCard,
  Session,
  SessionSummary,
  SubscriptionUsageEntry,
  TierBucket,
  TokenTotals,
  TurnReceiptIntegrationStatus,
  TurnInfo,
  ToolMetrics,
} from './lib/types';

const DAY = 86_400_000;
const now = import.meta.env.VITE_VISUAL_TEST === '1'
  ? Date.parse('2026-07-29T15:30:00.000Z')
  : Date.now();
const visualScenarioSelection = selectVisualScenario(location.search);
const visualScenario: VisualScenario = visualScenarioSelection.scenario;

if (visualScenarioSelection.warning) console.warn(visualScenarioSelection.warning);
// This attribute is intentionally installed only by main.ts's browser fixture
// branch. It gives screenshot tests a stable readiness/state marker without
// allowing query parameters to change native application behaviour.
document.documentElement.dataset.visualScenario = visualScenario;

function tok(total: number): TokenTotals {
  const input = Math.round(total * 0.62);
  const cached = Math.round(total * 0.24);
  const output = total - input;
  const reasoning = Math.round(output * 0.3);
  return {
    input_tokens: input,
    cached_input_tokens: cached,
    output_tokens: output,
    reasoning_output_tokens: reasoning,
    total_tokens: total,
  };
}

function scaleTok(t: TokenTotals, f: number): TokenTotals {
  return {
    input_tokens: Math.round(t.input_tokens * f),
    cached_input_tokens: Math.round(t.cached_input_tokens * f),
    output_tokens: Math.round(t.output_tokens * f),
    reasoning_output_tokens: Math.round(t.reasoning_output_tokens * f),
    total_tokens: Math.round(t.total_tokens * f),
  };
}

function toolMetrics(calls = 0): ToolMetrics {
  return { calls, reads: Math.floor(calls / 3), searches: Math.floor(calls / 4),
    mutations: Math.floor(calls / 3), commands: Math.floor(calls / 4), other: 0,
    successes: calls, failures: 0, unknown: 0, mutation_targets: Math.floor(calls / 3),
    one_shot_mutations: Math.floor(calls / 3), retry_count: 0,
    duration_ms: calls * 400, output_bytes: calls * 250 };
}

function at(daysAgo: number, hours: number, minutes = 0): string {
  const d = new Date(now - daysAgo * DAY);
  d.setHours(hours, minutes, 0, 0);
  return d.toISOString();
}

interface Fixture {
  id: string;
  harness: 'codex' | 'claude_code';
  name: string;
  started: string;
  hoursActive: number;
  model: string;
  total: number;
  turns: number;
  archived?: boolean;
  parent?: string;
  unlimited?: boolean;
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

// Stress mode: `?stress=N` appends N synthetic sessions per harness so list
// performance can be profiled at realistic history sizes. Dev-mock only.
const stressN = Number(new URLSearchParams(location.search).get('stress') ?? 0);
if (stressN > 0) {
  const CODEX_MODELS = ['gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna'];
  const CLAUDE_MODELS = ['claude-fable-5', 'claude-opus-4-8', 'claude-sonnet-5', 'claude-haiku-4-5'];
  const NAMES = [
    'Refactor ingestion pipeline', 'Fix flaky auth test', 'Add CSV export', 'Profile slow query',
    'Migrate config format', 'Review PR feedback', 'Write release notes', 'Debug watcher leak',
    'Tune cache eviction', 'Port build to CI', 'Sketch onboarding flow', 'Harden error paths',
  ];
  for (const harness of ['codex', 'claude_code'] as const) {
    const models = harness === 'codex' ? CODEX_MODELS : CLAUDE_MODELS;
    let parentId: string | null = null;
    for (let i = 0; i < stressN; i++) {
      const id = `57e55000-0000-4000-8000-${harness === 'codex' ? 'c0de' : 'c1de'}${String(i).padStart(8, '0')}`;
      // Every 40th session starts a cluster; the following ~30 rows become its
      // subagents, mimicking large fan-out sessions.
      const inCluster = parentId !== null && i % 40 !== 0;
      if (i % 40 === 0) parentId = id;
      const total = 50_000 + ((i * 2654435761) % 5_000_000);
      FIXTURES.push({
        id,
        harness,
        name: `${NAMES[i % NAMES.length]} #${i}`,
        started: at(Math.floor(i / 30), 8 + (i % 12), (i * 7) % 60),
        hoursActive: 0.5 + (i % 5),
        model: models[i % models.length],
        total,
        turns: 1 + (i % 12),
        archived: i % 17 === 0,
        parent: inCluster ? (parentId ?? undefined) : undefined,
        unlimited: harness === 'codex' ? true : undefined,
      });
    }
  }
  console.info(`[dev-mock] stress mode: +${stressN * 2} synthetic sessions`);
}

function fixtureModel(f: Fixture): string {
  if (visualScenario !== 'sessions-availability-fallback') return f.model;
  if (f.id.endsWith('c0de0001')) return 'gpt-5.7-visual-preview';
  if (f.id.endsWith('c0de0004')) return 'gpt-5.3-codex-spark';
  return f.model;
}

function buckets(f: Fixture): TierBucket[] {
  return [{ model: fixtureModel(f), service_tier: null, tokens: tok(f.total) }];
}

function summary(f: Fixture): SessionSummary {
  const model = fixtureModel(f);
  return {
    id: f.id,
    storage_id: f.harness === 'codex' ? `codex:thread:${f.id}` : `claude_code:session:${f.id}`,
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
    originator: f.harness === 'codex' ? 'chatgpt' : 'cli',
    source: f.parent ? 'subagent' : null,
    cli_version: '1.4.2',
    model_provider: f.harness === 'codex' ? 'openai' : 'anthropic',
    model,
    service_tier: null,
    plan_type: f.unlimited ? 'pro' : null,
    credits_unlimited: f.unlimited ?? null,
    credits_balance: null,
    context_window: f.harness === 'codex' ? 272_000 : 200_000,
    total_turns: f.turns,
    first_user_message: `${f.name} — please take a look.`,
    tokens_total: tok(f.total),
    buckets: buckets(f),
    tool_metrics: toolMetrics(f.turns * 3),
    tool_metrics_by_model: { [model]: toolMetrics(f.turns * 3) },
    category_totals: { coding: { turns: f.turns, tokens: tok(f.total), tool_calls: f.turns * 3, buckets: buckets(f) } },
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
      tokens: tok(Math.round(perTurn * jitter)),
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
    model_provider: f.harness === 'codex' ? 'openai' : 'anthropic',
    latest_context_tokens: Math.round((s.context_window ?? 200_000) * 0.54),
    tokens_by_model: { [model]: tok(f.total) },
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
    'gpt-5.6-sol': { input: 1.1, cached_input: 0.11, output: 8.8, reasoning: 8.8 },
    'gpt-5.6-terra': { input: 0.5, cached_input: 0.05, output: 4, reasoning: 4 },
    'gpt-5.6-luna': { input: 0.15, cached_input: 0.015, output: 1.2, reasoning: 1.2 },
    'gpt-5.5': { input: 0.8, cached_input: 0.08, output: 6.4, reasoning: 6.4 },
    'claude-opus-4-8': { input: 3.2, cached_input: 0.32, output: 16, reasoning: 16 },
    'claude-fable-5': { input: 2.4, cached_input: 0.24, output: 12, reasoning: 12 },
    'claude-sonnet-5': { input: 1.2, cached_input: 0.12, output: 6, reasoning: 6 },
    'claude-haiku-4-5': { input: 0.35, cached_input: 0.035, output: 1.75, reasoning: 1.75 },
  },
  fallback_model: 'gpt-5.6-sol',
  currencies: { codex: 'credits', claude_code: 'USD' },
  fallback_models: { codex: 'gpt-5.6-sol', claude_code: 'claude-sonnet-5' },
  api_models: {
    'gpt-5.6-sol': { input: 1.25, cached_input: 0.125, output: 10, reasoning: 10 },
    'gpt-5.6-terra': { input: 0.6, cached_input: 0.06, output: 4.8, reasoning: 4.8 },
    'gpt-5.6-luna': { input: 0.18, cached_input: 0.018, output: 1.44, reasoning: 1.44 },
    'gpt-5.5': { input: 1, cached_input: 0.1, output: 8, reasoning: 8 },
  },
  unpriced_models: ['gpt-5.3-codex-spark'],
  pricing_catalog: { rate_periods: [], conditional_modifiers: [], notes: [] },
};

function subscriptionUsage(): SubscriptionUsageEntry[] {
  return [
    {
      harness: 'codex',
      captured_at: new Date(now - 3 * 60_000).toISOString(),
      plan_type: 'pro',
      credits_unlimited: null,
      credits_balance: null,
      primary: {
        used_percent: 63,
        window_minutes: 300,
        resets_at: new Date(now + 2 * 3_600_000 + 12 * 60_000).toISOString(),
      },
      secondary: {
        used_percent: 22,
        window_minutes: 10_080,
        resets_at: new Date(now + 4 * DAY).toISOString(),
      },
    },
  ];
}

function rangeTotals(
  from: string | null,
  to: string | null,
  sessionIds?: string[],
): Record<string, RangeTotals> {
  const fromMs = from ? new Date(from).getTime() : 0;
  const toMs = to ? new Date(to).getTime() : now;
  const out: Record<string, RangeTotals> = {};
  const requestedIds = sessionIds ? new Set(sessionIds) : null;
  for (const f of visibleFixtures()) {
    const s = summary(f);
    if (requestedIds && !requestedIds.has(s.storage_id)) continue;
    const sStart = new Date(s.started_at).getTime();
    const sEnd = new Date(s.last_event_at).getTime();
    if (sEnd < fromMs || sStart > toMs) continue;
    // Fraction of the session window inside the queried range.
    const overlap = Math.min(sEnd, toMs) - Math.max(sStart, fromMs);
    const fraction = Math.max(0, Math.min(1, overlap / Math.max(1, sEnd - sStart)));
    out[s.storage_id] = {
      tokens: scaleTok(tok(f.total), fraction),
      buckets: buckets(f).map((b) => ({ ...b, tokens: scaleTok(b.tokens, fraction) })),
      tool_metrics: toolMetrics(Math.round(f.turns * 3 * fraction)),
      tool_metrics_by_model: { [fixtureModel(f)]: toolMetrics(Math.round(f.turns * 3 * fraction)) },
      optimization_findings_count: 0,
      optimization_summary: { findings: 0, warnings: 0, likely_avoidable_calls: 0, by_rule: {} },
    };
  }
  return out;
}

function visibleFixtures(): Fixture[] {
  return visualScenario === 'sessions-empty' || visualScenario === 'sessions-scanning'
    ? []
    : FIXTURES;
}

function scanStatus() {
  if (visualScenario === 'sessions-scanning') {
    return { done: 3, total: 12, complete: false, elapsed_ms: null, cold_reason: null };
  }
  if (visualScenario === 'defender-slow' || visualScenario === 'defender-error') {
    return {
      done: FIXTURES.length,
      total: FIXTURES.length,
      complete: true,
      elapsed_ms: 31_000,
      cold_reason: null,
    };
  }
  const sessions = visibleFixtures();
  return { done: sessions.length, total: sessions.length, complete: true, elapsed_ms: 1240, cold_reason: null };
}

function turnReceiptStatus(): TurnReceiptIntegrationStatus {
  return {
    enabled: false,
    executable_path: '/opt/odometer/odometer',
    codex: {
      requested: false,
      configured: false,
      receipt_observed: false,
      config_source: 'codex_hooks_json',
      config_path: '/home/dev/.codex/hooks.json',
      diagnostic_code: 'hook_not_requested',
      detail: 'Off. Harness configuration is unchanged.',
      restart_recommended: false,
      trust_review_recommended: false,
      last_run_at: null,
      last_run_success: null,
      last_receipt: null,
      last_run_detail: null,
    },
    claude_code: {
      requested: false,
      configured: false,
      receipt_observed: false,
      config_source: 'claude_settings_json',
      config_path: '/home/dev/.claude/settings.json',
      diagnostic_code: 'hook_not_requested',
      detail: 'Off. Harness configuration is unchanged.',
      restart_recommended: false,
      trust_review_recommended: false,
      last_run_at: null,
      last_run_success: null,
      last_receipt: null,
      last_run_detail: null,
    },
  };
}

function providerDiagnostics(): DiagnosticsReport {
  const generatedAt = new Date(now).toISOString();
  const healthy: ProviderDiagnostic = {
    id: 'codex',
    display_name: 'Codex',
    registered: true,
    state: 'ready',
    reasons: [],
    notices: [{ code: 'healthy', message: 'No issues detected for this provider.' }],
    capabilities: { archived_sources: true, session_index: true },
    roots: [
      { kind: 'live', path: '/home/dev/.codex/sessions', exists: true, is_default: true },
      { kind: 'archive', path: '/home/dev/.codex/archived_sessions', exists: true, is_default: true },
      { kind: 'session_index', path: '/home/dev/.codex/session_index.jsonl', exists: true, is_default: true },
    ],
    discovery: { discovered_files: 42, parsed_files: 42, skipped_files: 0, parse_failures: 0, cache_hits: 39, cache_misses: 3 },
    ledger: { history_store_available: true, durable_sessions: 42, available_sessions: 42, collision_sessions: 0 },
    pricing: { models_observed: 3, models_priced: 3, unpriced_models_used: [], fallback_models_used: [], fallback_used: false, rates_fetched_at: generatedAt, rates_stale: false },
    retention: { level: 'none', supports_archive: true, archive_roots_configured: 1 },
    quota: { status: 'not_available', reason_code: 'quota_source_not_implemented', message: 'Odometer does not read a live quota or rate-limit API for this provider yet.' },
  };
  const degraded: ProviderDiagnostic = {
    id: 'claude_code',
    display_name: 'Claude Code',
    registered: true,
    state: 'degraded',
    reasons: [
      { code: 'parse_failures_detected', message: 'The most recent scan could not parse one or more files for this provider.' },
      { code: 'pricing_fallback_used', message: 'One or more models used by this provider have no published rate and are estimated with the configured fallback rate.' },
    ],
    notices: [],
    capabilities: { archived_sources: false, session_index: false },
    roots: [{ kind: 'live', path: '/home/dev/.claude/projects', exists: true, is_default: true }],
    discovery: { discovered_files: 18, parsed_files: 18, skipped_files: 1, parse_failures: 2, cache_hits: 16, cache_misses: 2 },
    ledger: { history_store_available: true, durable_sessions: 16, available_sessions: 16, collision_sessions: 0 },
    pricing: { models_observed: 2, models_priced: 1, unpriced_models_used: [], fallback_models_used: ['claude-preview'], fallback_used: true, rates_fetched_at: generatedAt, rates_stale: false },
    retention: { level: 'moderate', supports_archive: false, archive_roots_configured: 0 },
    quota: { status: 'not_available', reason_code: 'quota_source_not_implemented', message: 'Odometer does not read a live quota or rate-limit API for this provider yet.' },
  };
  return {
    generated_at: generatedAt,
    source_configuration_valid: true,
    cache_cold_reason: null,
    last_scan_at: generatedAt,
    providers: [healthy, degraded],
  };
}

function emptyInstructionInventory() {
  return {
    files: [],
    roots: [{ path: '/home/dev/projects', source: 'configured', recursive: true, exists: true }],
    truncated: false,
    truncation_reason: null,
    entries_visited: 0,
    elapsed_ms: 0,
    scanned_at: new Date(now).toISOString(),
    stale: false,
  };
}

function fixtureUpdateMetadata() {
  return {
    rid: 1,
    currentVersion: '0.6.4',
    version: '9.9.9',
    date: '2026-07-01',
    body: 'Synthetic visual fixture',
    rawJson: {},
  };
}

function emitUpdateProgress(channelId: number) {
  const internals = (window as unknown as {
    __TAURI_INTERNALS__: { runCallback(id: number, data: unknown): void };
  }).__TAURI_INTERNALS__;
  internals.runCallback(channelId, {
    index: 0,
    message: { event: 'Started', data: { contentLength: 100 } },
  });
  internals.runCallback(channelId, {
    index: 1,
    message: { event: 'Progress', data: { chunkLength: 40 } },
  });
}

mockIPC((cmd, payload) => {
  switch (cmd) {
    case 'list_sessions':
      return visibleFixtures().map(summary);
    case 'get_session_details': {
      const { sessionId } = payload as { sessionId: string };
      const f = visibleFixtures().find((x) => summary(x).storage_id === sessionId);
      return f ? details(f) : null;
    }
    case 'get_subscription_usage':
      return subscriptionUsage();
    case 'resolve_working_directories':
      // Matches the fixture sessions' working directory, so the grid renders
      // the resolved repository rather than its unresolved path fallback.
      return [
        {
          directory: '/home/dev/projects/demo',
          repository_name: 'demo',
          relative_path: '',
          display_path: '~/projects/demo',
        },
        {
          directory: '/home/dev/Documents/Codex/2026-08-04/ser',
          repository_name: null,
          relative_path: null,
          display_path: '…/Codex/2026-08-04/ser',
        },
      ];
    case 'list_providers':
      return [
        { id: 'codex', display_name: 'Codex', archived_sources: true, session_index: true },
        { id: 'claude_code', display_name: 'Claude Code', archived_sources: false, session_index: false },
      ];
    case 'get_provider_diagnostics':
      return providerDiagnostics();
    case 'sessions_in_ranges': {
      const { ranges, sessionIds } = payload as {
        ranges: { from: string | null; to: string | null }[];
        sessionIds?: string[];
      };
      return ranges.map((r) => rangeTotals(r.from, r.to, sessionIds));
    }
    case 'list_tool_impact_targets':
      return [
        { kind: 'provider', key: 'alpha_server', label: 'alpha_server', turn_count: 8, call_count: 22 },
        { kind: 'provider', key: 'code_graph', label: 'code_graph', turn_count: 5, call_count: 14 },
        { kind: 'tool', key: 'mcp__alpha_server__code_search', label: 'mcp__alpha_server__code_search', turn_count: 7, call_count: 18 },
        { kind: 'tool', key: 'exec_command', label: 'exec_command', turn_count: 6, call_count: 11 },
        { kind: 'tool', key: 'mcp__code_graph__search', label: 'mcp__code_graph__search', turn_count: 5, call_count: 14 },
      ];
    case 'compare_tool_impact': {
      const { query } = payload as {
        query: { target_kind: 'provider' | 'tool'; target_key: string };
      };
      const cohort = (turns: number, tokens: number, duration: number, calls: number) => ({
        turn_count: turns,
        session_count: Math.max(1, Math.floor(turns / 2)),
        completed_turn_count: turns,
        duration_sample_count: turns,
        total_duration_ms: duration * turns,
        ttft_sample_count: 0,
        total_ttft_ms: 0,
        tokens: tok(tokens * turns),
        buckets: [],
        tool_metrics: toolMetrics(calls * turns),
      });
      return {
        target_kind: query.target_kind,
        target_key: query.target_key,
        observed: cohort(8, 184_000, 420_000, 7),
        baseline: cohort(21, 231_000, 510_000, 9),
        matched_observed: cohort(6, 176_000, 390_000, 7),
        matched_baseline: cohort(6, 224_000, 495_000, 9),
        matched_pairs: 6,
        warnings: ['Transcripts prove observed use, not whether the selected target was installed or available.'],
      };
    }
    case 'get_scan_status':
      return scanStatus();
    case 'get_performance_status':
      return { enabled: false, max_log_mb: 64, stored_bytes: 0, recorded_this_run: 0, dropped_this_run: 0 };
    case 'get_turn_receipt_status':
    case 'repair_turn_receipt_integrations':
      return turnReceiptStatus();
    case 'get_config':
      // Mirrors the backend's normalized shape: versioned provider map with
      // the legacy flat fields as its builtin mirror.
      return {
        config_version: 1,
        providers: {
          codex: {
            live_roots: ['/home/dev/.codex/sessions'],
            archive_roots: ['/home/dev/.codex/archived_sessions'],
            session_index_path: '/home/dev/.codex/session_index.jsonl',
          },
          claude_code: {
            live_roots: ['/home/dev/.claude/projects'],
            archive_roots: [],
            session_index_path: null,
          },
        },
        session_roots: ['/home/dev/.codex/sessions'],
        archive_roots: ['/home/dev/.codex/archived_sessions'],
        session_index_path: '/home/dev/.codex/session_index.jsonl',
        claude_session_roots: ['/home/dev/.claude/projects'],
        defender_exclusion_receipt: null,
        performance_tracking_enabled: false,
        performance_log_max_mb: 64,
        instructions_enabled: true,
        instructions_tab_visible: true,
        instruction_roots: [{ path: '/home/dev/projects', recursive: true }],
        turn_receipts_enabled: false,
        turn_receipts_codex: true,
        turn_receipts_claude: true,
      };
    case 'list_instruction_files': {
      if (visualScenario === 'instructions-empty') return emptyInstructionInventory();
      if (visualScenario === 'instructions-loading') return new Promise<never>(() => {});
      if (visualScenario === 'instructions-error') {
        return Promise.reject(new Error('Fixture instruction inventory failed.'));
      }
      const root = '/home/dev/projects/demo';
      return {
        files: [
          {
            id: 'mock-agents-root', path_id: 'mock-agents-root', path: `${root}/AGENTS.md`,
            directory: root, file_name: 'AGENTS.md', harnesses: ['codex'],
            root_path: '/home/dev/projects', root_source: 'configured', root_recursive: true,
            project_path: root, project_scope: 'mock-project', relative_path: 'demo/AGENTS.md',
            depth: 2, size: 240, line_count: 9, modified_at: new Date(now - DAY).toISOString(),
            content_hash: 'mock-content-root', parent_id: null, effective_ids: ['mock-agents-root'], warnings: [],
          },
          {
            id: 'mock-agents-app', path_id: 'mock-agents-app', path: `${root}/packages/app/AGENTS.md`,
            directory: `${root}/packages/app`, file_name: 'AGENTS.md', harnesses: ['codex'],
            root_path: '/home/dev/projects', root_source: 'configured', root_recursive: true,
            project_path: root, project_scope: 'mock-project', relative_path: 'demo/packages/app/AGENTS.md',
            depth: 4, size: 138, line_count: 5, modified_at: new Date(now - 2 * DAY).toISOString(),
            content_hash: 'mock-content-app', parent_id: 'mock-agents-root',
            effective_ids: ['mock-agents-root', 'mock-agents-app'],
            warnings: [{ kind: 'possible_conflict', severity: 'warning', message: 'Possible opposite directives in the effective chain: run tests.', related_paths: [`${root}/AGENTS.md`] }],
          },
          {
            id: 'mock-claude-root', path_id: 'mock-claude-root', path: `${root}/CLAUDE.md`,
            directory: root, file_name: 'CLAUDE.md', harnesses: ['claude_code'],
            root_path: '/home/dev/projects', root_source: 'configured', root_recursive: true,
            project_path: root, project_scope: 'mock-project', relative_path: 'demo/CLAUDE.md',
            depth: 2, size: 190, line_count: 7, modified_at: new Date(now - 3 * DAY).toISOString(),
            content_hash: 'mock-content-claude', parent_id: null, effective_ids: ['mock-claude-root'], warnings: [],
          },
        ],
        roots: [{ path: '/home/dev/projects', source: 'configured', recursive: true, exists: true }],
        truncated: false,
        truncation_reason: null,
        entries_visited: 24,
        elapsed_ms: 120,
        scanned_at: new Date(now).toISOString(),
        stale: false,
      };
    }
    case 'read_instruction_file': {
      if (visualScenario === 'instructions-content-error') {
        return Promise.reject(new Error('Fixture instruction content could not be loaded.'));
      }
      const { path } = payload as { path: string };
      const nested = path.includes('/packages/app/');
      const claude = path.endsWith('CLAUDE.md');
      const content = claude
        ? '# Claude Code instructions\n\n- Keep changes focused.\n- Run the relevant checks.\n'
        : nested
          ? '# Package instructions\n\nNever run tests.\n\nUse the package-level validation command.\n'
          : '# Project instructions\n\nAlways run tests.\n\n## Boundaries\n\n- Keep filesystem access in Rust.\n- Keep presentation in Svelte.\n';
      return { path, content };
    }
    case 'get_rates':
    case 'get_bundled_rates':
      return RATES;
    case 'set_rates':
    case 'set_config':
      if (visualScenario === 'settings-save-error') {
        return Promise.reject(new Error('Fixture settings save failed.'));
      }
      return true;
    case 'add_defender_exclusions':
      if (visualScenario === 'defender-error') {
        return Promise.reject(new Error('Fixture Defender exclusion request failed.'));
      }
      return {
        version: 1,
        configured_roots: [
          '/home/dev/.codex/sessions',
          '/home/dev/.codex/archived_sessions',
          '/home/dev/.claude/projects',
        ],
        verified_roots: [
          '/home/dev/.codex/sessions',
          '/home/dev/.codex/archived_sessions',
          '/home/dev/.claude/projects',
        ],
        verified_at: new Date(now).toISOString(),
      };
    case 'reveal_in_file_manager':
    case 'open_instruction_file':
    case 'open_task_in_chatgpt':
    case 'write_export':
    case 'export_performance_data':
      return true;
    case 'record_frontend_performance':
      return undefined;
    case 'list_external_events':
      return cmd === 'list_external_events' ? [] : undefined;
    case 'correlate_events':
      return { results: [] };
    case 'scan_git_outcomes':
      return [];
    case 'set_tray_totals':
      return undefined;
    case 'plugin:app|version':
      return '0.0.0-dev';
    case 'plugin:updater|check':
      return isUpdaterVisualScenario(visualScenario) ? fixtureUpdateMetadata() : null;
    case 'plugin:updater|download_and_install': {
      const { onEvent } = payload as { onEvent: { id: number } };
      emitUpdateProgress(onEvent.id);
      if (visualScenario === 'updater-error') {
        return Promise.reject(new Error('Fixture updater install failed.'));
      }
      if (visualScenario === 'updater-installing') return new Promise<never>(() => {});
      return undefined;
    }
    case 'plugin:process|restart':
      return undefined;
    case 'plugin:event|listen':
    case 'plugin:event|unlisten':
      return 0;
    default:
      // e.g. plugin:updater|check — callers handle rejection gracefully.
      return Promise.reject(new Error(`dev-mock: unhandled command ${cmd}`));
  }
});

console.info(`[dev-mock] Tauri IPC mocked with ${visualScenario} fixture data (browser mode)`);
