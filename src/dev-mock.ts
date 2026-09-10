// Fixture IPC for `npm run dev` in a plain browser (no native backend).
// Loaded only when import.meta.env.DEV is set and Tauri isn't present — see
// main.ts. Production builds tree-shake this module away entirely.

import { mockIPC } from '@tauri-apps/api/mocks';
import { mockRangePricing, mockSessionPricing, mockSummaryPricing, assertFixtureRates } from './dev-mock/pricing';
import { createFixtureData, tok, scaleTok, toolMetrics, type Fixture } from './dev-mock/fixtures';
import { isUpdaterVisualScenario, selectVisualScenario, type VisualScenario } from './dev-mock/visualScenario';
import type {
  DiagnosticsReport,
  HistoryRebuildStatus,
  HistoryStatus,
  ProviderDiagnostic,
  ProjectInfo,
  QuotaAlert,
  QuotaConfigWire,
  QuotaSnapshot,
  RangeTotals,
  RateCard,
  SubscriptionUsageEntry,
  TurnReceiptIntegrationStatus,
} from './lib/types';

const DAY = 86_400_000;
let rateOverride: RateCard | null = null;
const projectAssignments = new Map<string, string>();
const currentRates = () => rateOverride ?? RATES;
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

const { fixtures: FIXTURES, rates: RATES, summary, details, buckets, fixtureModel, pricingKey } =
  createFixtureData(now, visualScenario, Number(new URLSearchParams(location.search).get('stress') ?? 0));

// Issue #43: quota windows/budgets, mirroring subscriptionUsage()'s figures
// so the two panels agree in the browser dev fixture. `quotaConfigMock` is
// mutable so set_quota_config round-trips within one dev session, matching
// the real backend's persist-then-return contract.
function quotaSnapshots(): QuotaSnapshot[] {
  return [
    {
      provider: 'codex',
      provenance: 'transcript_derived',
      unavailable: null,
      windows: [
        {
          kind: 'burst',
          unit: 'percent',
          window_minutes: 300,
          used: 63,
          remaining: 37,
          limit: 100,
          unlimited: false,
          resets_at: new Date(now + 2 * 3_600_000 + 12 * 60_000).toISOString(),
          window_started_at: new Date(now - 2 * 3_600_000 - 48 * 60_000).toISOString(),
          window_started_at_estimated: true,
          observed_at: new Date(now - 3 * 60_000).toISOString(),
          confidence: 'medium',
          stale: false,
          unavailable: null,
          forecast: {
            pace_per_hour: 8.2,
            projected_exhaustion_at: null,
            reserve_deficit_percent: 4.5,
            evidence_points: 9,
          },
        },
        {
          kind: 'weekly',
          unit: 'percent',
          window_minutes: 10_080,
          used: 22,
          remaining: 78,
          limit: 100,
          unlimited: false,
          resets_at: new Date(now + 4 * DAY).toISOString(),
          window_started_at: null,
          window_started_at_estimated: false,
          observed_at: new Date(now - 3 * 60_000).toISOString(),
          confidence: 'medium',
          stale: false,
          unavailable: null,
          forecast: null,
        },
      ],
    },
    {
      provider: 'claude_code',
      provenance: 'transcript_derived',
      unavailable: 'no_quota_source',
      windows: [],
    },
  ];
}

let quotaConfigMock: QuotaConfigWire = {
  budgets: [],
  notifications: { enabled: false, quiet_hours: null },
  max_cache_age_secs: 21_600,
};

function checkQuotaAlerts(): QuotaAlert[] {
  return [];
}

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

// Issue #44: only populated for the 'tool-dimensions' visual scenario, so
// every other baseline is byte-for-byte unaffected. Codex and Claude Code
// both get real mcp_server/shell_family/language/context_source data — both
// providers are genuinely capable of every dimension (provider.rs). Gemini
// CLI deliberately omits mcp_server/shell_family here: it genuinely lacks
// those two capabilities (mcp_dimension/shell_dimension: false in the
// 'tool-dimensions' branch of the 'list_providers' case below, matching
// provider.rs's real GEMINI_CLI_DESCRIPTOR), so the panel must render
// "Unavailable" from that capability flag, not from an absent-vs-zero guess
// at missing keys here — there is no override anywhere in this file.
function dimensionTotalsFor(f: Fixture): RangeTotals['tool_dimensions'] {
  if (visualScenario !== 'tool-dimensions') return undefined;
  const dim = (calls: number, failures: number, outputBytes: number, durationMs: number) =>
    ({ calls, failures, output_bytes: outputBytes, duration_ms: durationMs, tokens: 0 });
  const tokenDim = (tokens: number) => ({ calls: 0, failures: 0, output_bytes: 0, duration_ms: 0, tokens });
  const contextSource = {
    conversation_cache: tokenDim(Math.round(f.total * 0.24)),
    ...(f.harness === 'claude_code' ? { newly_cached_context: tokenDim(Math.round(f.total * 0.08)) } : {}),
    unknown: tokenDim(Math.round(f.total * 0.1)),
  };
  if (f.harness === 'codex') {
    return {
      mcp_server: {
        code_search: dim(18, 1, 42_000, 7_200),
        repo_index: dim(6, 0, 9_500, 2_100),
      },
      shell_family: {
        git: dim(9, 0, 3_200, 1_800),
        npm: dim(5, 1, 61_000, 42_000),
        other: dim(2, 0, 800, 400),
      },
      language: {
        typescript: dim(14, 0, 88_000, 0),
        rust: dim(6, 1, 31_000, 0),
      },
      context_source: contextSource,
    };
  }
  if (f.harness === 'claude_code') {
    return {
      mcp_server: {
        docs_search: dim(7, 0, 15_000, 3_100),
      },
      shell_family: {
        pytest: dim(4, 1, 22_000, 18_500),
        git: dim(3, 0, 1_100, 600),
      },
      language: {
        svelte: dim(11, 0, 54_000, 0),
        typescript: dim(9, 0, 47_000, 0),
      },
      context_source: contextSource,
    };
  }
  // Gemini CLI: mcp_server/shell_family keys are omitted entirely (not
  // zeroed) — the capability is genuinely absent, not merely unused this
  // session. language/context_source stay real: those two dimensions are
  // generic, provider-agnostic signals (see provider.rs's
  // language_dimension/context_dimension, both true for Gemini CLI).
  return {
    language: {
      python: dim(5, 0, 26_000, 0),
    },
    context_source: contextSource,
  };
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
    const scopedBuckets = buckets(f).map((b) => ({ ...b, tokens: scaleTok(b.tokens, fraction) }));
    out[s.storage_id] = {
      tokens: scaleTok(tok(f.total, f.harness), fraction),
      buckets: scopedBuckets,
      pricing: mockRangePricing(pricingKey(f), fraction),
      tool_metrics: toolMetrics(Math.round(f.turns * 3 * fraction)),
      tool_metrics_by_model: { [fixtureModel(f)]: toolMetrics(Math.round(f.turns * 3 * fraction)) },
      optimization_findings_count: 0,
      optimization_summary: { findings: 0, warnings: 0, likely_avoidable_calls: 0, by_rule: {} },
      tool_dimensions: dimensionTotalsFor(f),
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

function historyStatus(): HistoryStatus {
  // No visual scenario models a still-migrating archive: every fixture
  // scenario represents an already-warm install, and browser dev mode has
  // no Rust backend to actually migrate. `get_history_status` always
  // resolves 'ready' here so the mount sequence proceeds exactly like a
  // normal warm start.
  return {
    status: 'ready',
    step: null,
    step_index: null,
    step_total: null,
    items_done: null,
    items_total: null,
    elapsed_ms: 42,
  };
}

function historyRebuildStatus(): HistoryRebuildStatus {
  // No visual scenario models an in-progress or completed rebuild — browser
  // dev mode has no Rust backend to actually run one, and `rebuild_history`/
  // `cancel_history_rebuild` are unhandled below (they fall through to
  // mockIPC's default, an error), so a click in dev mode surfaces as an
  // error rather than silently doing nothing.
  return {
    phase: 'idle',
    done: 0,
    total: 0,
    elapsed_ms: null,
    error: null,
    sessions_reparsed: null,
    sessions_missing_transcript: null,
    sessions_failed: null,
    rate_limit_points_before: null,
    rate_limit_points_after: null,
    session_json_bytes_before: null,
    session_json_bytes_after: null,
    file_size_before: null,
    file_size_after: null,
  };
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
    gemini_cli: {
      requested: false,
      configured: false,
      receipt_observed: false,
      config_source: 'gemini_settings_json',
      config_path: '/home/dev/.gemini/settings.json',
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
    capabilities: { archived_sources: true, session_index: true, currency: 'credits', deep_link: true, quota_source: true },
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
    capabilities: { archived_sources: false, session_index: false, currency: 'USD', deep_link: false, quota_source: false },
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
    case 'get_session_pricing': {
      const { sessionIds } = payload as { sessionIds: string[] };
      const ids = new Set(sessionIds);
      return Object.fromEntries(visibleFixtures().filter(f => ids.has(summary(f).storage_id))
        .map(f => [summary(f).storage_id, mockSummaryPricing(pricingKey(f))]));
    }
    case 'get_session_details': {
      const { sessionId } = payload as { sessionId: string };
      const f = visibleFixtures().find((x) => summary(x).storage_id === sessionId);
      if (!f) return null;
      const session = details(f);
      return { ...session, pricing: mockSessionPricing(pricingKey(f), session) };
    }
    case 'get_subscription_usage':
      return subscriptionUsage();
    case 'get_quota_snapshots':
      return quotaSnapshots();
    case 'get_quota_config':
      return quotaConfigMock;
    case 'set_quota_config':
      quotaConfigMock = (payload as { config: QuotaConfigWire }).config;
      return quotaConfigMock;
    case 'check_quota_alerts':
      return checkQuotaAlerts();
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
    case 'resolve_projects': {
      const projects = new Map<string, ProjectInfo>();
      for (const fixture of visibleFixtures()) {
        const session = summary(fixture);
        const override = projectAssignments.get(session.storage_id);
        const key = override ?? session.project_key ?? 'repo:demo-fixture';
        let project = projects.get(key);
        if (!project) {
          project = {
            project_key: key,
            label: key === 'repo:demo-fixture' ? 'demo' : 'Standalone project',
            provenance: key === 'repo:demo-fixture' ? 'repository_root' : 'fallback_path_identity',
            member_keys: [key],
            session_count: 0,
            overridden_session_keys: [],
          };
          projects.set(key, project);
        }
        project.session_count++;
        if (override) project.overridden_session_keys!.push(session.storage_id);
      }
      return [...projects.values()];
    }
    case 'set_project_alias':
    case 'merge_projects':
    case 'unmerge_project':
      return null;
    case 'clear_session_project_override': {
      const { sessionKey } = payload as { sessionKey: string };
      projectAssignments.delete(sessionKey);
      return null;
    }
    case 'reassign_session_project': {
      const { sessionKey, projectKey } = payload as { sessionKey: string; projectKey: string | null };
      const key = projectKey ?? `manual:${sessionKey}`;
      projectAssignments.set(sessionKey, key);
      return key;
    }
    case 'list_providers': {
      // Kept to the two shipped-and-visually-baselined providers for every
      // other scenario; Gemini CLI is registered but intentionally left out
      // so it does not shift tab counts in the Playwright visual suite.
      const providers = [
        { id: 'codex', display_name: 'Codex', archived_sources: true, session_index: true, currency: 'credits', deep_link: true, quota_source: true, mcp_dimension: true, shell_dimension: true, language_dimension: true, context_dimension: true },
        { id: 'claude_code', display_name: 'Claude Code', archived_sources: false, session_index: false, currency: 'USD', deep_link: false, quota_source: false, mcp_dimension: true, shell_dimension: true, language_dimension: true, context_dimension: true },
      ];
      // Issue #44: the 'tool-dimensions' scenario adds Gemini CLI so the
      // panel can show a real, uncontrived "Unavailable" state — no
      // capability-flag override on any provider anywhere in this file.
      // These flags match provider.rs's real GEMINI_CLI_DESCRIPTOR exactly:
      // mcp_dimension/shell_dimension false (not corroborated against a
      // real transcript in this codebase), language_dimension/
      // context_dimension true (generic, provider-agnostic signals).
      if (visualScenario === 'tool-dimensions') {
        providers.push({
          id: 'gemini_cli', display_name: 'Gemini CLI', archived_sources: false, session_index: false, currency: 'USD', deep_link: false, quota_source: false,
          mcp_dimension: false, shell_dimension: false, language_dimension: true, context_dimension: true,
        });
      }
      return providers;
    }
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
        // Synthetic cohort data, not tied to a specific session/model
        // (buckets stays empty below), so cache-creation harness-awareness
        // doesn't matter for pricing here — 'claude_code' is an arbitrary
        // but harmless choice.
        tokens: tok(tokens * turns, 'claude_code'),
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
    case 'get_history_status':
      return historyStatus();
    case 'get_history_rebuild_status':
      return historyRebuildStatus();
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
        memory_heap_tracking_enabled: false,
        instructions_enabled: true,
        instructions_tab_visible: true,
        instruction_roots: [{ path: '/home/dev/projects', recursive: true }],
        turn_receipts_enabled: false,
        turn_receipts_codex: true,
        turn_receipts_claude: true,
        turn_receipts_gemini: false,
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
      return currentRates();
    case 'get_bundled_rates':
      return RATES;
    case 'set_rates': {
      if (visualScenario === 'settings-save-error') {
        return Promise.reject(new Error('Fixture settings save failed.'));
      }
      const updatedRates = (payload as { rates: RateCard }).rates;
      assertFixtureRates(updatedRates, RATES);
      rateOverride = updatedRates;
      return true;
    }
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
