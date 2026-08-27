// TypeScript types mirroring Rust structs in src-tauri/src/model.rs, config.rs, and rates.rs.
// Keep in sync when Rust types change.

// Open provider identity: the backend now supports an arbitrary ProviderId
// (see ProviderDescriptor / list_providers below), not just the two builtin
// providers. Kept as `Harness` for now to minimize churn across call sites
// that pass it through as an opaque id/key.
export type Harness = string;

export interface TokenTotals {
  input_tokens: number;
  cached_input_tokens: number;
  /** Anthropic cache-creation ("cache write") tokens: a subset of
   * input_tokens distinct from cached_input_tokens (cache reads). Always 0
   * for Codex. See credits.ts eventCost — never double-price this against
   * cached_input_tokens or the plain input rate. */
  cache_creation_input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
}

export type ToolKind = 'read' | 'search' | 'mutation' | 'command' | 'other';
export type ToolOutcome = 'pending' | 'success' | 'failure' | 'unknown';
export type TaskCategory = 'planning' | 'exploration' | 'coding' | 'debugging' | 'testing' | 'review' | 'other';
/** Normalized tool-origin dimension (issue #44). `unknown` means the
 * dimension was recorded before this field existed, never a real zero. */
export type ToolOrigin = 'core' | 'mcp' | 'provider' | 'unknown';

export interface ToolMetrics {
  calls: number;
  reads: number;
  searches: number;
  mutations: number;
  commands: number;
  other: number;
  successes: number;
  failures: number;
  unknown: number;
  mutation_targets: number;
  one_shot_mutations: number;
  retry_count: number;
  duration_ms: number;
  output_bytes: number;
  /** Origin-dimension breakdown (issue #44): always sums to `calls`.
   * Optional here (mirroring `resource_id` above) so existing fixtures and
   * historical serialized data need not populate it; real backend responses
   * always include it. */
  core_origin_calls?: number;
  mcp_origin_calls?: number;
  provider_origin_calls?: number;
  unknown_origin_calls?: number;
}

export interface ToolObservation {
  call_id: string;
  turn_id: string | null;
  harness: Harness;
  model: string | null;
  timestamp: string;
  kind: ToolKind;
  name: string;
  providers: string[];
  effective_tools: string[];
  target: string | null;
  resource_id?: string | null;
  origin: ToolOrigin;
  outcome: ToolOutcome;
  duration_ms: number | null;
  output_bytes: number;
}

export interface TurnClassification {
  version: number;
  category: TaskCategory;
  confidence: number;
  signals: string[];
}

export interface CategoryMetric {
  turns: number;
  tokens: TokenTotals;
  tool_calls: number;
  buckets: TierBucket[];
}

export interface OptimizationFinding {
  version: number;
  rule_id: string;
  severity: string;
  confidence?: string;
  turn_id: string | null;
  model: string | null;
  timestamp: string | null;
  evidence: string;
  remediation: string;
  occurrences?: number;
  avoidable_calls?: number;
}

export interface OptimizationSummary {
  findings: number;
  warnings: number;
  likely_avoidable_calls: number;
  by_rule: Record<string, number>;
}

export interface TurnInfo {
  turn_id: string;
  index: number;
  model: string | null;
  reasoning_effort: string | null;
  collaboration_mode: string | null;
  service_tier: string | null;
  status: 'in_progress' | 'completed' | 'aborted' | 'rolled_back';
  abort_reason: string | null;
  started_at: string | null;
  completed_at: string | null;
  duration_ms: number | null;
  time_to_first_token_ms: number | null;
  user_message: string | null;
  last_agent_message: string | null;
  tokens: TokenTotals;
  tool_metrics: ToolMetrics;
  classification: TurnClassification;
}

export interface RateLimitWindow {
  used_percent: number;
  window_minutes: number | null;
  resets_at: string | null;
}

export interface RateLimitSnapshotPoint {
  timestamp: string;
  turn_id: string | null;
  limit_id: string | null;
  primary: RateLimitWindow | null;
  secondary: RateLimitWindow | null;
  /** First observation's timestamp in this point's collapsed run of
   *  consecutive, field-identical observations (issue #153). `null` for a
   *  run of exactly one observation and for data written before #153 --
   *  both mean the same thing: treat `timestamp` as the run's only
   *  observation. */
  run_started_at: string | null;
  /** How many consecutive raw observations this point collapses (#153).
   *  Always 1 for pre-#153 data and single-observation runs. */
  observation_count: number;
}

/** Most-recent provider-reported subscription-usage snapshot for one
 *  harness, from get_subscription_usage. Harnesses with no snapshots
 *  (Claude Code transcripts, today) are simply absent from the result. */
export interface SubscriptionUsageEntry {
  harness: Harness;
  captured_at: string; // ISO8601
  plan_type: string | null;
  credits_unlimited: boolean | null;
  credits_balance: number | null;
  primary: RateLimitWindow | null;
  secondary: RateLimitWindow | null;
}

export interface Session {
  id: string;
  /** Durable, harness-namespaced storage identity; provider id remains in `id`. */
  storage_id: string;
  harness: Harness;
  thread_name: string | null;
  forked_from_id: string | null;
  parent_thread_id: string | null;
  agent_path: string | null;
  agent_nickname: string | null;
  file_path: string;
  /** Whether the recorded transcript is still available at `file_path`. */
  source_availability: 'present' | 'missing';
  archived: boolean;
  started_at: string; // ISO8601
  last_event_at: string; // ISO8601
  working_directory: string | null;
  originator: string | null;
  source: string | null;
  /** True when a legacy Claude subagent used its filename stem as identity because agentId was absent. */
  subagent_id_is_path_fallback: boolean;
  history_mode: string | null;
  memory_mode: string | null;
  cli_version: string | null;
  model_provider: string | null;
  model: string | null;
  service_tier: string | null;
  plan_type: string | null;
  credits_unlimited: boolean | null;
  credits_balance: number | null;
  context_window: number | null;
  /** Context fill of the most recent API call — comparable to context_window, unlike the cumulative tokens_total. */
  latest_context_tokens: number | null;
  total_turns: number;
  first_user_message: string | null;
  tokens_total: TokenTotals;
  tokens_by_model: Record<string, TokenTotals>;
  tokens_history: {
    timestamp: string;
    model: string | null;
    service_tier: string | null;
    /** Complete per-request input count; null for historical records without direct request evidence. */
    request_input_tokens: number | null;
    total_tokens: number;
    delta: TokenTotals;
  }[];
  rate_limits_history: RateLimitSnapshotPoint[];
  turns: TurnInfo[];
  tool_observations: ToolObservation[];
  tool_metrics: ToolMetrics;
  tool_metrics_by_model: Record<string, ToolMetrics>;
  category_totals: Partial<Record<TaskCategory, CategoryMetric>>;
  optimization_findings: OptimizationFinding[];
  /** Auto-computed project-identity key (#41); null when there is no working directory. */
  project_key: string | null;
  /** Auto-computed local display label for `project_key`. A local alias may override the effective
   *  label shown in the UI — join through `resolveProjects()`/the project store rather than reading
   *  this field directly when displaying to the user. */
  project_label: string | null;
  project_provenance: ProjectProvenance | null;
}

/** How a project identity was resolved (#41), stored alongside it rather than re-derived. */
export type ProjectProvenance =
  | 'repository_root'
  | 'workspace_root'
  | 'provider_project_id'
  | 'fallback_path_identity';

/** Token usage grouped by (model, service_tier); prices usage exactly without the full event history. */
export interface TierBucket {
  model: string;
  service_tier: string | null;
  tokens: TokenTotals;
}

/** One (dimension_kind, dimension_value) entry's additive counters (issue
 * #44). `tokens` is populated only for `context_source` values; every other
 * dimension kind populates `calls`/`failures`/`output_bytes`/`duration_ms`
 * and leaves `tokens` at 0. */
export interface ToolDimensionMetrics {
  calls: number;
  failures: number;
  output_bytes: number;
  duration_ms: number;
  tokens: number;
}

/** Issue #44 open-set dimension kind. */
export type ToolDimensionKind = 'mcp_server' | 'shell_family' | 'language' | 'context_source';

/** Date-scoped rollup returned by sessions_in_ranges. */
export interface RangeTotals {
  tokens: TokenTotals;
  buckets: TierBucket[];
  tool_metrics: ToolMetrics;
  tool_metrics_by_model: Record<string, ToolMetrics>;
  optimization_findings_count: number;
  optimization_summary?: OptimizationSummary;
  /** Outer key is the dimension kind, inner key the dimension value. A
   *  missing kind means no ledger-durable data for this window — consult
   *  `ProviderDescriptor`'s matching `*_dimension` flag to tell a real zero
   *  from a provider that cannot supply the dimension at all. */
  tool_dimensions?: Partial<Record<ToolDimensionKind, Record<string, ToolDimensionMetrics>>>;
}

export interface ToolImpactCohort {
  turn_count: number;
  session_count: number;
  completed_turn_count: number;
  duration_sample_count: number;
  total_duration_ms: number;
  ttft_sample_count: number;
  total_ttft_ms: number;
  tokens: TokenTotals;
  buckets: TierBucket[];
  tool_metrics: ToolMetrics;
}

export type ToolImpactTargetKind = 'provider' | 'tool';

export interface ToolImpactTarget {
  kind: ToolImpactTargetKind;
  key: string;
  label: string;
  turn_count: number;
  call_count: number;
}

export interface ToolImpactResult {
  target_kind: ToolImpactTargetKind;
  target_key: string;
  observed: ToolImpactCohort;
  baseline: ToolImpactCohort;
  matched_observed: ToolImpactCohort;
  matched_baseline: ToolImpactCohort;
  matched_pairs: number;
  warnings: string[];
}

/** Lightweight wire form of a Session for the list view and live updates. */
export interface SessionSummary {
  id: string;
  /** Durable, harness-namespaced storage identity; provider id remains in `id`. */
  storage_id: string;
  harness: Harness;
  thread_name: string | null;
  forked_from_id: string | null;
  parent_thread_id: string | null;
  agent_path: string | null;
  agent_nickname: string | null;
  file_path: string;
  /** Whether the recorded transcript is still available at `file_path`. */
  source_availability: 'present' | 'missing';
  archived: boolean;
  started_at: string; // ISO8601
  last_event_at: string; // ISO8601
  working_directory: string | null;
  originator: string | null;
  source: string | null;
  cli_version: string | null;
  model_provider: string | null;
  model: string | null;
  service_tier: string | null;
  plan_type: string | null;
  credits_unlimited: boolean | null;
  credits_balance: number | null;
  context_window: number | null;
  total_turns: number;
  first_user_message: string | null;
  tokens_total: TokenTotals;
  buckets: TierBucket[];
  tool_metrics: ToolMetrics;
  tool_metrics_by_model: Record<string, ToolMetrics>;
  category_totals: Partial<Record<TaskCategory, CategoryMetric>>;
  optimization_findings_count: number;
  optimization_summary?: OptimizationSummary;
  project_key: string | null;
  project_label: string | null;
  project_provenance: ProjectProvenance | null;
}

/** Why a scan's cache could not be treated as fully warm. */
export type ColdReason = 'parse_version_changed' | 'cache_missing' | 'cache_corrupt';

/** Bulk-scan progress, from get_scan_status and "scan-progress" events. */
export interface ScanStatus {
  done: number;
  total: number;
  complete: boolean;
  /** Wall-clock duration of the last completed scan; null while running. */
  elapsed_ms: number | null;
  /** Why the cache could not be treated as fully warm; null for a warm scan. */
  cold_reason: ColdReason | null;
}

/** Issue #162: re-parse every archived session from its source transcript,
 *  then VACUUM. `vacuuming` has no `done`/`total` of its own — SQLite
 *  reports no per-page VACUUM progress. */
export type HistoryRebuildPhase =
  | 'idle'
  | 'running'
  | 'vacuuming'
  | 'complete'
  | 'cancelled'
  | 'failed';

/** History-rebuild progress, from get_history_rebuild_status and
 *  "history-rebuild-progress" events. The evidence fields below `elapsed_ms`
 *  are only non-null once `phase` is 'complete' | 'cancelled' | 'failed'. */
export interface HistoryRebuildStatus {
  phase: HistoryRebuildPhase;
  done: number;
  total: number;
  elapsed_ms: number | null;
  error: string | null;
  sessions_reparsed: number | null;
  sessions_missing_transcript: number | null;
  sessions_failed: number | null;
  rate_limit_points_before: number | null;
  rate_limit_points_after: number | null;
  session_json_bytes_before: number | null;
  session_json_bytes_after: number | null;
  /** Total on-disk footprint — main database file plus its `-wal` sidecar
   *  (issue #167). Not the main file alone: a rebuild that shrinks the
   *  database while leaving an oversized WAL behind must still show up here. */
  file_size_before: number | null;
  file_size_after: number | null;
}

/**
 * Durable-history archive lifecycle (#116): `pending` until the archive has
 * finished opening/migrating, `ready` once available, `unavailable` if it
 * failed to open (live transcripts stay readable either way).
 */
export type HistoryReadinessStatus = 'pending' | 'ready' | 'unavailable';

/**
 * Durable-history open/migration progress, from get_history_status and
 * "history-progress" events (#116). `step`/`step_index`/`step_total`
 * describe the migration step most recently reported — in progress while
 * `status` is 'pending', otherwise the last one that ran, or all null if the
 * archive needed no migration at all. `items_done`/`items_total` are
 * non-null only while a step that streams per-row progress is running.
 */
export interface HistoryStatus {
  status: HistoryReadinessStatus;
  step: string | null;
  step_index: number | null;
  step_total: number | null;
  items_done: number | null;
  items_total: number | null;
  elapsed_ms: number | null;
}

/** Point-in-time evidence from the explicit, elevated Defender action. */
export interface DefenderExclusionReceipt {
  version: number;
  configured_roots: string[];
  verified_roots: string[];
  verified_at: string;
}

/** Per-provider session sources; authoritative from config_version 1. */
export interface ProviderSourceConfig {
  live_roots: string[];
  archive_roots: string[];
  session_index_path: string | null;
}

export interface Config {
  /** 0 = legacy flat-field layout; 1 = `providers` is authoritative and the
   *  flat fields mirror its builtin entries. The Settings UI still edits the
   *  flat fields; the backend treats submitted payloads as legacy-authoritative. */
  config_version: number;
  providers: Record<string, ProviderSourceConfig>;
  session_roots: string[];
  archive_roots: string[];
  session_index_path: string;
  claude_session_roots: string[];
  defender_exclusion_receipt: DefenderExclusionReceipt | null;
  performance_tracking_enabled: boolean;
  performance_log_max_mb: number;
  memory_heap_tracking_enabled: boolean;
  instructions_enabled: boolean;
  instructions_tab_visible: boolean;
  instruction_roots: InstructionRoot[];
  turn_receipts_enabled: boolean;
  turn_receipts_codex: boolean;
  turn_receipts_claude: boolean;
}

export interface InstructionRoot {
  path: string;
  recursive: boolean;
}

export interface InstructionWarning {
  kind: 'duplicate' | 'possible_conflict' | 'oversized' | 'possibly_stale' | string;
  severity: 'info' | 'warning' | string;
  message: string;
  related_paths: string[];
}

export interface InstructionFile {
  id: string;
  path_id: string;
  path: string;
  directory: string;
  file_name: string;
  harnesses: string[];
  root_path: string;
  root_source: 'global' | 'configured' | 'observed' | string;
  root_recursive: boolean;
  project_path: string | null;
  project_scope: string | null;
  relative_path: string;
  depth: number;
  size: number;
  line_count: number | null;
  modified_at: string | null;
  content_hash: string | null;
  parent_id: string | null;
  effective_ids: string[];
  warnings: InstructionWarning[];
}

export interface InstructionRootSummary {
  path: string;
  source: string;
  recursive: boolean;
  exists: boolean;
}

export interface InstructionInventory {
  files: InstructionFile[];
  roots: InstructionRootSummary[];
  truncated: boolean;
  truncation_reason: 'entry_limit' | 'file_limit' | string | null;
  entries_visited: number;
  elapsed_ms: number;
  scanned_at: string;
  /** True when served from the persisted copy while a background rescan runs. */
  stale: boolean;
}

export interface InstructionScanProgress {
  scan_id: number;
  phase: 'preparing' | 'scanning' | 'analyzing' | 'complete' | string;
  roots_done: number;
  roots_total: number;
  entries_visited: number;
  files_found: number;
  elapsed_ms: number;
  truncated: boolean;
}

export interface InstructionContent {
  path: string;
  content: string;
}

/** Registered provider, for descriptor-driven UI surfaces. */
export interface ProviderDescriptor {
  id: string;
  display_name: string;
  archived_sources: boolean;
  session_index: boolean;
  /** Rate-card currency key this provider prices into (see `harnessCurrency`
   *  in `credits.ts`). Codex uses "credits"; every other current provider
   *  prices in "USD". */
  currency: string;
  /** Whether Odometer can open this provider's session via a native deep
   *  link (`open_task_in_chatgpt`). Only Codex has one today. */
  deep_link: boolean;
  /** Whether this provider's local transcripts carry account-wide
   *  rate-limit/quota snapshots usable by the Subscription Usage view. */
  quota_source: boolean;
  /** Issue #44 open-set tool/context dimension availability. `false` means
   *  this provider's transcript shape is not corroborated to support the
   *  dimension — the panel must render "unavailable", never a fabricated
   *  zero, for a session from this provider. */
  mcp_dimension: boolean;
  shell_dimension: boolean;
  language_dimension: boolean;
  context_dimension: boolean;
}

export interface HarnessIntegrationStatus {
  requested: boolean;
  configured: boolean;
  receipt_observed: boolean;
  config_source: string;
  config_path: string;
  diagnostic_code: string;
  detail: string;
  restart_recommended: boolean;
  trust_review_recommended: boolean;
  last_run_at: string | null;
  last_run_success: boolean | null;
  last_receipt: string | null;
  last_run_detail: string | null;
}

export interface TurnReceiptIntegrationStatus {
  enabled: boolean;
  executable_path: string;
  codex: HarnessIntegrationStatus;
  claude_code: HarnessIntegrationStatus;
}

export interface PerformanceStatus {
  enabled: boolean;
  max_log_mb: number;
  stored_bytes: number;
  recorded_this_run: number;
  dropped_this_run: number;
}

/** OS-reported process memory (see `memory.rs::ProcessMemorySample`). A
 *  field is `null` where the platform/query is unavailable — never a
 *  fabricated zero. */
export interface ProcessMemorySample {
  rss_bytes: number | null;
  peak_rss_bytes: number | null;
  private_bytes: number | null;
}

/** Allocator-tracked heap (see `memory.rs::HeapSample`). Both byte fields are
 *  `null` when heap tracking itself is off, distinct from "0 bytes tracked".
 *  `possibly_undercounted` is `true` once tracking has seen a free of an
 *  allocation that predates the current enable — freeing memory `alloc`
 *  never saw is indistinguishable, without per-allocation provenance, from
 *  freeing tracked memory, so `current_bytes`/`peak_bytes` become a lower
 *  bound / delta since enable rather than a verified live total. Render that
 *  as a caveat, never silently as an ordinary heap size. */
export interface HeapSample {
  current_bytes: number | null;
  peak_bytes: number | null;
  possibly_undercounted: boolean;
}

/** One continuous-sampler tick during a long startup phase (issue #163). */
export interface PhaseSampleEvent {
  phase: string;
  sample_index: number;
  elapsed_ms: number;
  rss_bytes: number | null;
  peak_rss_bytes: number | null;
  private_bytes: number | null;
  heap_bytes: number | null;
  heap_peak_bytes: number | null;
  progress_done: number | null;
  progress_total: number | null;
  /** True on the sample that hit the phase's sample cap — the sampler
   *  stopped there rather than continuing silently. */
  capped: boolean;
}

/** On-disk database size plus volume headroom for one connection
 *  (`"history_store"` or `"scan_cache"`) — see `memory.rs::DatabaseFootprint`. */
export interface DatabaseFootprintEntry {
  connection: string;
  db_bytes: number | null;
  wal_bytes: number | null;
  volume_free_bytes: number | null;
  volume_total_bytes: number | null;
}

/** A recently recorded phase timing, privacy-scrubbed to just a name and a
 *  duration (see `performance.rs::RecentOperation`). */
export interface RecentOperation {
  operation: string;
  duration_ms: number;
  success: boolean;
  timestamp: string;
}

/** `DiagnosticsPanel`'s live-telemetry data source (issue #163). When
 *  `enabled` is false every other field is empty/null — render that as
 *  "tracking is off", never as zeros or an empty chart. */
export interface PerformanceLiveStatus {
  enabled: boolean;
  process: ProcessMemorySample | null;
  heap: HeapSample | null;
  active_phase: string | null;
  active_phase_elapsed_ms: number | null;
  progress_done: number | null;
  progress_total: number | null;
  recent_samples: PhaseSampleEvent[];
  database_footprints: DatabaseFootprintEntry[];
  recent_operations: RecentOperation[];
}

export interface ModelRate {
  input: number;
  cached_input: number;
  /** Cache-creation ("cache write") rate — a normalized dimension distinct
   * from both `input` and `cached_input`.
   *
   * Deliberately nullable, and `null`/absent is NOT "free": it means the
   * publisher has not stated a cache-write premium for this model, and
   * cache-creation tokens must be priced at the ordinary `input` rate
   * (exactly today's pre-#42 accounting) rather than at zero. `0` is a
   * real, deliberate "this is free" claim, distinct from "unknown". Always
   * resolve through `cacheCreationRate()` in credits.ts rather than reading
   * this field directly. */
  cache_creation_input: number | null;
  output: number;
  reasoning: number;
}

/** Billing surface for a catalog rule.  Rules never cross billing surfaces. */
export type PricingSurface = 'codex_plan_credits' | 'openai_api_usd' | 'anthropic_api_usd';

/** Source evidence retained with a dated or conditional pricing rule. */
export interface PricingProvenance {
  evidence: string;
  source_url: string;
  verified_at: string;
  note: string | null;
}

/** A base rate that applies over the half-open interval [from, to). */
export interface EffectiveRatePeriod {
  id: string;
  surface: PricingSurface;
  model: string;
  from: string;
  to: string | null;
  rate: ModelRate;
  /** Documented cache-write premium, when the provider publishes one. Parsed
   * telemetry has no cache-write token category, so this is provenance only. */
  cache_write_input_multiplier?: number | null;
  provenance: PricingProvenance;
  label: string;
}

export interface RequestInputTokenThresholdCondition {
  kind: 'request_input_token_threshold';
  greater_than: number;
}

export type PricingCondition = RequestInputTokenThresholdCondition;

export interface RateMultipliers {
  input: number;
  output: number;
}

/** A request-level modifier. Cache-write pricing is intentionally absent: it
 * is not observed separately by the parsers and must not be guessed. */
export interface ConditionalRateModifier {
  id: string;
  surface: PricingSurface;
  model: string;
  from: string;
  to: string | null;
  condition: PricingCondition;
  multipliers: RateMultipliers;
  provenance: PricingProvenance;
  label: string;
}

/** Versioned, source-backed, time-aware scenario pricing data. */
export interface PricingCatalog {
  rate_periods: EffectiveRatePeriod[];
  conditional_modifiers: ConditionalRateModifier[];
  notes: string[];
}

/** Provenance recorded for every priced amount — see rates.rs PricingBasis.
 * These states must render as visually and structurally distinct in the UI,
 * never collapsed into one number. */
export type PricingBasis =
  | 'direct'
  | 'aliased'
  /** Resolved via a provider-declared *floating* alias — a mapping the
   * provider repoints as new models ship. Correct as of the card's fetch and
   * priced from a real published rate, but carrying a known expiry, past
   * which resolution falls through to `fallback`. Render as a soft note, not
   * a warning: the price is right today (issue #177). */
  | 'floating_alias'
  | 'fallback'
  | 'estimated'
  | 'free_local'
  | 'subscription'
  | 'stale'
  | 'unavailable';

/** A provider-declared floating model alias: a name the provider repoints
 * without renaming, so a static mapping is right today and silently wrong
 * later. Mirrors `FloatingAlias` in rates.rs — keep both in sync. */
export interface FloatingAlias {
  target: string;
  /** Last date (inclusive, `YYYY-MM-DD`, UTC) on which `target` is trusted. */
  expires_at: string;
  source_url?: string;
}

/** The resolved pricing-table key and provenance for one raw model id. */
export interface PricedModelResolution {
  resolved_model: string;
  basis: PricingBasis;
}

/** A user-declared subscription or custom plan for one harness. Odometer
 * records exactly what the user enters; it never infers a plan-equivalent
 * token allowance. */
export interface SubscriptionPlan {
  name: string;
  monthly_price: number | null;
  currency: string | null;
  notes: string | null;
  /** User-declared estimated monthly savings from a local/proxy baseline
   * versus a metered API-equivalent cost — never derived from token counts. */
  local_baseline_savings: number | null;
}

/** A user-supplied display-currency conversion. Odometer performs no FX
 * fetch: `rate` and `as_of` are exactly what the user entered. The original
 * amount and currency are always retained separately alongside the
 * converted total. */
export interface CurrencyConversion {
  target_currency: string;
  rate: number;
  as_of: string;
  source: string;
}

/** Coarse freshness classification for RateRefreshState. */
export type RateFreshness = 'fresh' | 'stale' | 'unknown';

/** Bounded-cache-age bookkeeping for the (currently unimplemented) price
 * refresh/rollback flow — see rates.rs module docs for the network seam. */
export interface RateRefreshState {
  last_success_at: string | null;
  last_attempt_at: string | null;
  last_failure_reason: string | null;
  max_cache_age_secs: number;
}

export interface RateCard {
  version: number;
  currency: string;
  unit: string;
  source_url: string;
  fetched_at: string | null;
  models: Record<string, ModelRate>;
  fallback_model: string;
  /** Per-harness currency labels (e.g. codex -> "credits", claude_code -> "USD"). */
  currencies: Record<string, string>;
  /** Per-harness fallback models; falls back to fallback_model when absent. */
  fallback_models: Record<string, string>;
  /** OpenAI API USD rates for Codex models — powers the est.-cost column. */
  api_models: Record<string, ModelRate>;
  /** Known models without a published price; excluded rather than fallback-priced. */
  unpriced_models: string[];
  /** Dated and conditional scenario rules. Kept intact by the settings editor. */
  pricing_catalog: PricingCatalog;
  /** Raw provider model id -> canonical rate-table key, resolved before any
   * fallback lookup. */
  model_aliases: Record<string, string>;
  /** Raw provider model id -> a mapping the provider documents as temporary,
   * with the date it stops being trusted. Checked before `model_aliases`;
   * mirrors `RateCard::floating_model_aliases` in rates.rs (issue #177). */
  floating_model_aliases?: Record<string, FloatingAlias>;
  /** Models explicitly zero-cost (free tier, local/self-hosted) — distinct
   * from unpriced_models and from an ordinary unresolved rate. */
  free_local_models: string[];
  /** Per-harness user-declared subscription/custom plan configuration. */
  subscription_plans: Record<string, SubscriptionPlan>;
  /** User-supplied display-currency conversion; null means show the
   * original currency. Odometer never invents or fetches a rate. */
  display_currency: CurrencyConversion | null;
  /** Bounded-cache-age bookkeeping for the refresh flow. */
  refresh: RateRefreshState;
}

export interface ExternalEvent {
  id: string;
  timestamp: string;
  scope: string | null;
  source: string;
  kind: string;
  metadata: Record<string, string>;
}

export interface CorrelationObservation {
  session_count: number;
  turn_count: number;
  session_duration_ms: number;
  tokens: TokenTotals;
  buckets_by_harness: Partial<Record<Harness, TierBucket[]>>;
  tool_metrics: ToolMetrics;
}

export interface CorrelationQuery {
  events: ExternalEvent[];
  before_days: number;
  after_days: number;
  exclude_confounded: boolean;
  include_subagents: boolean;
}

export interface EventCorrelation {
  event: ExternalEvent;
  before: CorrelationObservation;
  after: CorrelationObservation;
  after_window_end: string;
  after_window_complete: boolean;
  minimum_session_count: number;
  sample_ready: boolean;
  token_delta: number;
  session_delta: number;
  confounding_event_ids: string[];
  warnings: string[];
}

export interface CorrelationResult { results: EventCorrelation[]; }

export type GitOutcomeKind = 'kept' | 'reverted' | 'abandoned' | 'ambiguous' | 'not_evaluated';
export interface GitOutcome {
  session_id: string;
  repository_scope: string | null;
  kind: GitOutcomeKind;
  commit_ids: string[];
  evidence: string;
}

/** How one session working directory should be labelled in the grid.
 *  A working directory is not necessarily a repository — scratch directories
 *  have none, and their final path segment identifies nothing. */
export interface WorkingDirectoryInfo {
  directory: string;
  repository_name: string | null;
  /** Location within the repository; empty string at the root itself. */
  relative_path: string | null;
  /** Shortened absolute path, home collapsed to `~`. */
  display_path: string;
}

/** One resolved project (#41), after local alias/merge/split overrides.
 *  The one backend aggregation the dashboard, tables, and export all join
 *  a session's `project_key` against — see `resolveProjects()`. */
export interface ProjectInfo {
  /** Effective (post-merge) project key. */
  project_key: string;
  /** Effective display label — a local alias when set, else the auto-computed label. */
  label: string;
  provenance: ProjectProvenance;
  /** Every auto-computed `project_key` folded into this project; more than one only after a merge. */
  member_keys: string[];
  session_count: number;
}

// ---------------------------------------------------------------------------
// Provider diagnostics (issue #39). Local display may show exact paths;
// export redaction is a frontend transform — see lib/diagnosticsExport.ts.
// ---------------------------------------------------------------------------

export type ProviderHealthState = 'ready' | 'degraded' | 'unsupported' | 'not_detected';

/** Machine-stable code plus human text explaining a state or observation. */
export interface DiagnosticReason {
  code: string;
  message: string;
}

export type DiagnosticRootKind = 'live' | 'archive' | 'session_index';

export interface DiagnosticRoot {
  kind: DiagnosticRootKind;
  /** Exact local path. Present here for local display; stripped by default
   *  when building a redacted export (see diagnosticsExport.ts). */
  path: string;
  exists: boolean;
  is_default: boolean;
}

export interface DiagnosticsCapabilities {
  archived_sources: boolean;
  session_index: boolean;
  currency: string;
  deep_link: boolean;
  quota_source: boolean;
}

export interface DiscoveryHealth {
  discovered_files: number;
  parsed_files: number;
  skipped_files: number;
  parse_failures: number;
  cache_hits: number;
  cache_misses: number;
}

export interface LedgerHealth {
  history_store_available: boolean;
  durable_sessions: number;
  available_sessions: number;
  collision_sessions: number;
}

export interface PricingHealth {
  models_observed: number;
  models_priced: number;
  /** Bounded sample of used models known to have no published price. */
  unpriced_models_used: string[];
  /** Bounded sample of used models priced only via the harness fallback rate. */
  fallback_models_used: string[];
  fallback_used: boolean;
  rates_fetched_at: string | null;
  rates_stale: boolean;
}

export type RetentionRiskLevel = 'none' | 'moderate' | 'high';

export interface RetentionHealth {
  level: RetentionRiskLevel;
  supports_archive: boolean;
  archive_roots_configured: number;
}

/** 'transcript_derived' means at least one quota window has been observed
 *  from this provider's own transcripts (see QuotaSnapshot below) — never a
 *  live-polled API; no provider has one implemented. */
export type QuotaStatus = 'not_available' | 'transcript_derived';

export interface QuotaHealth {
  status: QuotaStatus;
  reason_code: string;
  message: string;
}

export interface ProviderDiagnostic {
  id: string;
  display_name: string;
  registered: boolean;
  state: ProviderHealthState;
  /** Reasons that drove `state` (blocking). */
  reasons: DiagnosticReason[];
  /** Additional non-blocking observations. */
  notices: DiagnosticReason[];
  capabilities: DiagnosticsCapabilities;
  roots: DiagnosticRoot[];
  discovery: DiscoveryHealth;
  ledger: LedgerHealth;
  pricing: PricingHealth;
  retention: RetentionHealth;
  quota: QuotaHealth;
}

export interface DiagnosticsReport {
  generated_at: string;
  /** False when the saved session-source configuration is ambiguous or
   *  otherwise invalid, disabling scanning for every provider until it is
   *  corrected in Settings. */
  source_configuration_valid: boolean;
  cache_cold_reason: ColdReason | null;
  last_scan_at: string | null;
  providers: ProviderDiagnostic[];
}

// ---------------------------------------------------------------------------
// Quota windows, budgets, and alerts (issue #43). One backend service
// (src-tauri/src/quota.rs) computes every number here — pace, projected
// exhaustion, reserve/deficit, and budget-crossing decisions. The frontend
// only formats and renders (see lib/subscriptionUsage.ts).
// ---------------------------------------------------------------------------

export type QuotaWindowKind = 'burst' | 'daily' | 'weekly' | 'monthly' | 'credit_balance' | 'other';

export type QuotaUnit = 'percent' | 'credits';

/** Never coerced together: a snapshot always says whether its numbers came
 *  from transcripts or a (currently unimplemented) live-polled source. */
export type QuotaProvenance = 'transcript_derived' | 'live_provider';

export type QuotaConfidence = 'high' | 'medium' | 'low';

/** `no_quota_source` and `no_observation` are the only reasons the current
 *  (transcript-only) backend ever produces. The rest are reserved for a
 *  future, reviewed live-polling source — see quota.rs's module docs. */
export type QuotaUnavailableReason =
  | 'no_quota_source'
  | 'no_observation'
  | 'clock_skew'
  | 'provider_outage'
  | 'auth_expired'
  | 'rate_limited'
  | 'offline';

export interface QuotaForecast {
  /** Percentage points of the window consumed per hour. */
  pace_per_hour: number;
  /** Only set when the projection lands before the window's own reset. */
  projected_exhaustion_at: string | null;
  /** Positive = burning faster than an even pace to reset (deficit/at risk);
   *  negative = a reserve/cushion. */
  reserve_deficit_percent: number;
  evidence_points: number;
}

export interface QuotaWindow {
  kind: QuotaWindowKind;
  unit: QuotaUnit;
  window_minutes: number | null;
  /** `null` exactly when `unavailable` is set, or (for `credits`) the plan
   *  is unlimited — never a fabricated zero. */
  used: number | null;
  remaining: number | null;
  limit: number | null;
  /** A known state ("unlimited"), not a number — `used`/`remaining` stay
   *  `null` when this is true. */
  unlimited: boolean;
  resets_at: string | null;
  window_started_at: string | null;
  /** True when `window_started_at` was inferred (resets_at - window_minutes)
   *  rather than observed as an actual used-percent rollover in the data. */
  window_started_at_estimated: boolean;
  observed_at: string;
  confidence: QuotaConfidence;
  /** Numbers are still populated when stale — see QuotaSnapshot's honesty
   *  contract in quota.rs: stale is a different, more honest fact than
   *  "no reading at all". */
  stale: boolean;
  unavailable: QuotaUnavailableReason | null;
  forecast: QuotaForecast | null;
}

export interface QuotaSnapshot {
  provider: Harness;
  provenance: QuotaProvenance;
  windows: QuotaWindow[];
  /** Set only when `windows` is empty. */
  unavailable: QuotaUnavailableReason | null;
}

export type BudgetUnit = 'percent_of_window' | 'tokens';

export interface QuotaBudget {
  id: string;
  provider: Harness;
  /** `null` = provider-wide; only valid combined with `unit: 'tokens'`. */
  project_key: string | null;
  unit: BudgetUnit;
  /** Matches `QuotaWindowKind` ("burst"/"daily"/"weekly"/"monthly").
   *  Required for `percent_of_window`; ignored for `tokens`. */
  window_kind: string | null;
  /** Rolling period for a `tokens` budget; ignored for `percent_of_window`. */
  period_hours: number | null;
  /** Percent-used (0-100) for `percent_of_window`, or a raw token count for `tokens`. */
  threshold: number;
  enabled: boolean;
}

export interface NotificationSettings {
  /** Opt-in: no alert is ever surfaced while this is false. */
  enabled: boolean;
  /** Local-hour [start, end) range during which alerts are tracked but not shown. */
  quiet_hours: [number, number] | null;
}

/** get_quota_config / set_quota_config payload. Never includes the backend's
 *  internal notification dedup log. */
export interface QuotaConfigWire {
  budgets: QuotaBudget[];
  notifications: NotificationSettings;
  max_cache_age_secs: number;
}

export interface QuotaAlert {
  budget_id: string;
  provider: Harness;
  project_key: string | null;
  message: string;
  current_value: number;
  threshold: number;
  fired_at: string;
}
