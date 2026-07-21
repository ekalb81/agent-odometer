// Fixture IPC for `npm run dev` in a plain browser (no native backend).
// Loaded only when import.meta.env.DEV is set and Tauri isn't present — see
// main.ts. Production builds tree-shake this module away entirely.

import { mockIPC } from '@tauri-apps/api/mocks';
import type {
  RangeTotals,
  RateCard,
  Session,
  SessionSummary,
  TierBucket,
  TokenTotals,
  TurnInfo,
} from './lib/types';

const DAY = 86_400_000;
const now = Date.now();

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

function buckets(f: Fixture): TierBucket[] {
  return [{ model: f.model, service_tier: null, tokens: tok(f.total) }];
}

function summary(f: Fixture): SessionSummary {
  return {
    id: f.id,
    harness: f.harness,
    thread_name: f.name,
    forked_from_id: null,
    parent_thread_id: f.parent ?? null,
    agent_path: null,
    agent_nickname: null,
    file_path: `/home/dev/.sessions/${f.id}.jsonl`,
    archived: f.archived ?? false,
    started_at: f.started,
    last_event_at: new Date(new Date(f.started).getTime() + f.hoursActive * 3_600_000).toISOString(),
    working_directory: '/home/dev/projects/demo',
    originator: f.harness === 'codex' ? 'chatgpt' : 'cli',
    source: f.parent ? 'subagent' : null,
    cli_version: '1.4.2',
    model: f.model,
    service_tier: null,
    plan_type: f.unlimited ? 'pro' : null,
    credits_unlimited: f.unlimited ?? null,
    credits_balance: null,
    context_window: f.harness === 'codex' ? 272_000 : 200_000,
    total_turns: f.turns,
    first_user_message: `${f.name} — please take a look.`,
    tokens_total: tok(f.total),
    buckets: buckets(f),
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
  const startMs = new Date(f.started).getTime();
  const perTurn = Math.floor(f.total / f.turns);
  const turns: TurnInfo[] = Array.from({ length: f.turns }, (_, i) => {
    const tStart = startMs + (i * f.hoursActive * 3_600_000) / f.turns;
    const jitter = 0.5 + ((i * 2654435761) % 100) / 100; // deterministic variety
    return {
      turn_id: `${f.id}-t${i + 1}`,
      index: i + 1,
      model: f.model,
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
    };
  });
  let cumulative = 0;
  const history = turns.map((t) => {
    cumulative += t.tokens.total_tokens;
    return {
      timestamp: t.started_at!,
      model: f.model,
      service_tier: null,
      total_tokens: cumulative,
      delta: t.tokens,
    };
  });
  return {
    ...s,
    history_mode: null,
    memory_mode: null,
    model_provider: f.harness === 'codex' ? 'openai' : 'anthropic',
    latest_context_tokens: Math.round((s.context_window ?? 200_000) * 0.54),
    tokens_by_model: { [f.model]: tok(f.total) },
    tokens_history: history,
    turns,
  };
}

const RATES: RateCard = {
  version: 5,
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
};

function rangeTotals(from: string | null, to: string | null): Record<string, RangeTotals> {
  const fromMs = from ? new Date(from).getTime() : 0;
  const toMs = to ? new Date(to).getTime() : now;
  const out: Record<string, RangeTotals> = {};
  for (const f of FIXTURES) {
    const s = summary(f);
    const sStart = new Date(s.started_at).getTime();
    const sEnd = new Date(s.last_event_at).getTime();
    if (sEnd < fromMs || sStart > toMs) continue;
    // Fraction of the session window inside the queried range.
    const overlap = Math.min(sEnd, toMs) - Math.max(sStart, fromMs);
    const fraction = Math.max(0, Math.min(1, overlap / Math.max(1, sEnd - sStart)));
    out[f.id] = {
      tokens: scaleTok(tok(f.total), fraction),
      buckets: buckets(f).map((b) => ({ ...b, tokens: scaleTok(b.tokens, fraction) })),
    };
  }
  return out;
}

mockIPC((cmd, payload) => {
  switch (cmd) {
    case 'list_sessions':
      return FIXTURES.map(summary);
    case 'get_session_details': {
      const { sessionId } = payload as { sessionId: string };
      const f = FIXTURES.find((x) => x.id === sessionId);
      return f ? details(f) : null;
    }
    case 'sessions_in_range': {
      const { from, to } = payload as { from: string | null; to: string | null };
      return rangeTotals(from, to);
    }
    case 'get_scan_status':
      return { done: FIXTURES.length, total: FIXTURES.length, complete: true, elapsed_ms: 1240 };
    case 'get_config':
      return {
        session_roots: ['/home/dev/.codex/sessions'],
        archive_roots: ['/home/dev/.codex/archived_sessions'],
        session_index_path: '/home/dev/.codex/session_index.jsonl',
        claude_session_roots: ['/home/dev/.claude/projects'],
      };
    case 'get_rates':
    case 'get_bundled_rates':
      return RATES;
    case 'set_rates':
    case 'set_config':
    case 'reveal_in_file_manager':
    case 'open_task_in_chatgpt':
      return undefined;
    case 'plugin:app|version':
      return '0.0.0-dev';
    case 'plugin:event|listen':
    case 'plugin:event|unlisten':
      return 0;
    default:
      // e.g. plugin:updater|check — callers handle rejection gracefully.
      return Promise.reject(new Error(`dev-mock: unhandled command ${cmd}`));
  }
});

console.info('[dev-mock] Tauri IPC mocked with fixture data (browser dev mode)');
