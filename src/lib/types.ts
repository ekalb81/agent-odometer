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
   * for Codex. See query.rs token_cost — never double-price this against
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
  /** Attached by get_session_details only, never stored in Rust Session snapshots. */
  pricing?: SessionPricing;
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
  lifecycle?: 'present' | 'retained' | 'superseded' | 'purged';
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

/** Query-time pricing from the shared Rust service; mirrors query.rs. */
export interface PricedModel {
  model: string;
  cost: number;
  basis: PricingBasis;
  unpriced: boolean;
}

export interface PricedSurface {
  total: number;
  by_model: PricedModel[];
  missing_models: string[];
  unpriced_models: string[];
  converted?: ConvertedTotal;
}

export interface ConvertedTotal {
  from_currency: string;
  target_currency: string;
  amount: number;
  rate: number;
  as_of: string;
  source: string;
}

export interface RangePricing {
  /** Legacy reference, not a purchased-credit or included-allowance measurement. */
  plan: PricedSurface;
  api: PricedSurface | null;
  current?: CurrentPricing;
}

export interface CurrentPricing {
  as_of: string;
  /** Standard purchased-credit rates are the reference; this is never quota authority. */
  included_allowance_basis?: string;
  purchased_credits: PricedSurface;
  /** Standard-credit equivalents; does not infer an allowance size or billed allocation. */
  included_allowance: PricedSurface;
  api_estimate: PricedSurface;
}

/** Response-only cumulative pricing; never derived from an unbounded event window. */
export interface SummaryPricing {
  pricing: RangePricing;
  categories: Record<string, RangePricing>;
}

export interface TurnPrice {
  cost: number;
  fallback_used: boolean;
  unpriced: boolean;
  basis: PricingBasis;
}

export interface PricingRuleSummary {
  id: string;
  label: string;
  from: string;
  to: string | null;
  provenance: PricingProvenance;
}

export interface TimeAwarePricing extends PricedSurface {
  surface: PricingSurface;
  applied_rate_periods: string[];
  applied_modifiers: string[];
  conditional_evidence_missing: string[];
  cache_write_pricing_unmodeled: boolean;
  unobserved_cache_write_input_multipliers: number[];
  rules: PricingRuleSummary[];
}

export interface SessionPricing {
  plan: PricedSurface;
  flat_api: PricedSurface | null;
  turn_prices: Record<string, { plan: TurnPrice; api: TurnPrice | null; current?: { purchased_credits: TurnPrice; included_allowance: TurnPrice; api_estimate: TurnPrice } }>;
  time_aware_api: TimeAwarePricing | null;
  current?: CurrentPricing;
  dated_purchased_credits?: TimeAwarePricing;
  dated_included_allowance?: TimeAwarePricing;
}

/** Date-scoped rollup returned by sessions_in_ranges. */
export interface RangeTotals {
  /** Authoritative server estimate for this event window.
   * Absent in raw aggregates, older payloads, or when provider identity is unknown. */
  pricing?: RangePricing;
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
  lifecycle?: 'present' | 'retained' | 'superseded' | 'purged';
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
  coverage_complete?: boolean | null;
  failure?: HistoryFailure | null;
}

export interface HistoryFailure { kind: 'corrupt' | 'newer_schema' | 'exclusions_unverified' | 'unavailable'; message: string; }
export interface RecoveryReceipt { backup_directory: string; recovered_at_ms: number; }
export interface HistoryRecoveryStatus {
  status: HistoryReadinessStatus;
  failure: HistoryFailure | null;
  coverage_complete: boolean | null;
  backup_directory: string | null;
  can_recover: boolean;
  can_retry: boolean;
}

export interface RetentionPolicy { retained_days: number | null; }
export interface RetentionStatus {
  policy: RetentionPolicy;
  present_sessions: number;
  retained_sessions: number;
  superseded_sessions: number;
  purged_sessions: number;
  coverage_complete: boolean;
  recovered_at: string | null;
}
export interface PurgePreview {
  cutoff_utc_day: string;
  policy_revision: number;
  sessions: number;
  identity_groups: number;
  snapshot_bytes: number;
  tokens: TokenTotals;
  revision: string;
}
export interface PurgeResult { removed_keys: string[]; purged_at: string; }

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
  /** Public, unauthenticated service status; disabled unless explicitly enabled. */
  provider_status_enabled?: boolean;
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
  turn_receipts_gemini?: boolean;
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
   *  in `currency.ts`). Codex uses "credits"; every other current provider
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
  gemini_cli?: HarnessIntegrationStatus;
}

export type IntegrationClient = 'codex' | 'claude_code';
export type IntegrationScope = 'user' | 'project';
export type IntegrationChange = 'install' | 'remove' | 'restore';
export type IntegrationDiagnosticCode = 'integration_not_configured' | 'server_launch_failed' | 'protocol_version_mismatch' | 'tool_catalog_mismatch' | 'ledger_not_ready' | 'history_incomplete' | 'history_coverage_unavailable' | 'scan_in_progress' | 'session_not_found' | 'pricing_incomplete' | 'query_too_broad' | 'snapshot_expired' | 'query_failed' | 'query_cancelled';
export interface IntegrationDiagnostic { code: IntegrationDiagnosticCode; evidence: string; next_action: string; }
export interface IntegrationStatus {
  schema_version: number; server_version: string; protocol_version: string; generated_at: string;
  ledger_available: boolean; scan_status: string; sessions: number | null;
  coverage_complete?: boolean | null;
  observation: { captured_at: string | null; age_seconds: number | null; generation: string | null; token_event_from: string | null; token_event_to: string | null; };
  providers: Array<{ provider: string; registered: boolean; roots: Array<{ kind: string; path: string | null; exists: boolean; }>; ledger: { durable_sessions: number; available_sessions: number; collision_sessions: number; } | null; models: Array<{ model: string; basis: PricingBasis; resolved_model: string | null; }>; quota_status: string; }>;
  pricing_authority: string; quota_authority: string; dimensions: string[]; filters: string[];
  pricing_models: Array<{ provider: string; model: string; plan_basis: PricingBasis; api_estimate_basis: PricingBasis; resolved_plan_model: string; resolved_api_model: string; }>;
  suggested_next_calls: string[]; diagnostics: IntegrationDiagnostic[]; limitations: string[];
}
export interface IntegrationClientCard {
  client: IntegrationClient; scope: IntegrationScope; configuration_path: string;
  executable: string | null; version: string | null; installed: boolean; configured: boolean; managed: boolean;
  backup_path: string | null; restore_available: boolean; diagnostic: IntegrationDiagnostic | null;
  manual_command: string; instructions: string;
}
export interface IntegrationActivity {
  timestamp: string; initialized_at: string; client: IntegrationClient | 'verifier' | 'other';
  client_version: string | null; identity_authority: string; tool: string; duration_ms: number;
  success: boolean; error_code: string | null; result_bytes: number; result_rows: number | null;
  schema_version: number | null; fallback_or_unavailable: boolean | null;
  observation_generation?: string | null; ledger_available?: boolean | null;
}
export interface IntegrationCenterReport { schema_version: number; status: IntegrationStatus; cards: IntegrationClientCard[]; activity: IntegrationActivity[]; activity_available: boolean; supported_client_task_proof: 'not_verified'; }
export interface IntegrationPreview { id: string; client: IntegrationClient; scope: IntegrationScope; action: IntegrationChange; configuration_path: string; entry_preview: string; warning: string; }
export interface IntegrationApplyResult { configuration_path: string; backup_path: string | null; restart_required: boolean; }
export interface IntegrationVerifyReport { schema_version: number; ok: boolean; checks: Array<{ id: string; status: 'pass' | 'fail' | 'unknown'; detail: string; }>; }

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
   * price through Rust's ModelRate::cache_creation_rate; the frontend only
   * edits this field and displays returned prices. */
  cache_creation_input: number | null;
  output: number;
  reasoning: number;
}

/** Billing surface for a catalog rule.  Rules never cross billing surfaces. */
export type PricingSurface = 'codex_plan_credits' | 'codex_purchased_credits' | 'codex_included_allowance' | 'openai_api_usd' | 'anthropic_api_usd' | 'gemini_api_usd';

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

export type PricingCondition = RequestInputTokenThresholdCondition | { kind: 'service_tier'; tier: 'standard' | 'fast' | 'ultrafast' };

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
  /** Freshness must not erase the fallback identity. */
  fallback_used?: boolean;
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
  from_currency?: string | null;
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
  /** Read-time evidence; persisted/input metadata is never trusted. */
  delivery?: { source: 'embedded_app_bundle' | 'saved_override' | 'last_valid_fallback'; app_version: string; card_version: number; last_failure_reason: string | null; };
  /** Per-row source evidence; keys use the Rust table/id convention. */
  rate_provenance?: Record<string, PricingProvenance>;
  /** Retained rates/aliases requiring review; never newly verified by a merge. */
  upgrade_review?: string[];
  /** Exclusive UTC expiry of a temporary current flat reference. */
  flat_rate_expires_at?: Record<string, string>;
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
  /** Derived from each observation's buckets using the query's rate snapshot. */
  pricing_by_harness?: Partial<Record<Harness, RangePricing>>;
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
  /** Durable session keys explicitly reassigned into this project; absent in older payloads. */
  overridden_session_keys?: string[];
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
  /** True when the forecast anchor was inferred (counter decrease or reset schedule)
   *  rather than a provider-reported actual reset. */
  window_started_at_estimated: boolean;
  /** Optional for older headless/mock responses; none of these prove a last reset. */
  window_start_basis?: 'unknown' | 'counter_decrease' | 'reset_schedule_estimate';
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

export interface WidgetPreferences { visible: boolean; provider: 'codex' | 'claude_code' | 'gemini_cli'; kind: 'quota' | 'usage'; always_on_top: boolean; }
export interface WidgetSettings { version: number; revision: number; preferences: WidgetPreferences; }
export interface WidgetQuotaWindow { kind: QuotaWindowKind; unit: QuotaUnit; used: number | null; remaining: number | null; unlimited: boolean; observed_at: string; resets_at: string | null; stale: boolean; unavailable: QuotaUnavailableReason | null; }
export interface WidgetSnapshot {
  settings: WidgetSettings; computed_at: string;
  quota: { provenance: 'transcript_derived' | 'live_provider'; unavailable: QuotaUnavailableReason | null; windows: WidgetQuotaWindow[]; windows_omitted: number } | null;
  usage: { session_count: number; total_tokens: number; latest_activity_at: string | null; plan_amount: number | null; plan_currency: string; api_amount_usd: number | null; estimate_partial: boolean; scan_complete: boolean } | null;
}

export type BudgetUnit = 'percent_of_window' | 'tokens' | 'usd';

export interface QuotaBudget {
  id: string;
  provider: Harness;
  /** `null` = provider-wide; valid with token and USD estimates. */
  project_key: string | null;
  unit: BudgetUnit;
  /** Matches `QuotaWindowKind` ("burst"/"daily"/"weekly"/"monthly").
   *  Required for `percent_of_window`; ignored for `tokens`. */
  window_kind: string | null;
  /** Rolling period for token/USD budgets; ignored for `percent_of_window`. */
  period_hours: number | null;
  /** Percent-used (0-100), raw token count, or USD API estimate threshold. */
  threshold: number;
  enabled: boolean;
}

export interface AmbientCategories { attention: boolean; provider_incidents: boolean; stale_quota: boolean; retention_risk: boolean }
export type AmbientRoute = "budgets" | "attention" | "provider_status" | "retention" | "quota";
export interface AmbientNotice { id: string; route: AmbientRoute; provider: string; code: string; observed_at: string; delivered_at: string }
export interface AmbientSnapshot { available: boolean; as_of: string; notifications: NotificationSettings; alerts: AmbientNotice[]; recent: AmbientNotice[] }
export interface NotificationSettings {
  ambient?: AmbientCategories;
  /** Opt-in: no alert is ever surfaced while this is false. */
  enabled: boolean;
  /** Local-hour [start, end) range during which alerts are tracked but not shown. */
  quiet_hours: [number, number] | null;
}

/** get_quota_config / set_quota_config payload. Never includes the backend's
 *  internal notification dedup log. */
export interface QuotaConfigWire {
  /** Optimistic edit revision. Preserve it on writes to reject stale edits. */
  revision?: string | null;
  budgets: QuotaBudget[];
  notifications: NotificationSettings;
  max_cache_age_secs: number;
}

export interface QuotaBudgetStatus {
  budget_id: string;
  /** Null when no trustworthy current value is available. */
  current_value: number | null;
  /** Fixed backend reason code; never a provider body or local path. */
  unavailable: string | null;
}

export interface QuotaBudgetCheck {
  as_of: string;
  statuses: QuotaBudgetStatus[];
  alerts: QuotaAlert[];
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

/** Sanitized turn or response timing from Codex's retained local logs, separate from accounting. */
export interface SpeedSample {
  completed_at: string;
  model: string;
  reasoning_effort: string | null;
  mode: 'fast' | 'standard' | 'unknown';
  output_tokens: number;
  reasoning_tokens: number | null;
  duration_ms: number;
  output_tps: number;
  visible_tps: number | null;
  time_to_first_token_ms: number | null;
  timing_source: 'explicit' | 'timestamps' | 'response';
}

export interface SpeedReport {
  status: 'ready' | 'unavailable';
  reason: string | null;
  measurement: 'turn' | 'response';
  source: 'codex_session_logs' | 'codex_local_logs';
  generated_at: string;
  rows: SpeedSample[];
  excluded_count: number;
  scanned_rows: number;
  truncated: boolean;
}

export interface SpeedQuery {
  from: string;
  to: string;
  measurement: 'turn' | 'response';
}
/** Source inspection only; these bodies never belong in summaries or aggregates. */
export interface TranscriptCursor {
  session_id: string;
  source_id: string;
  generation: string;
  offset: number;
  record_start: number;
  partial: boolean;
}
export interface TranscriptRequest {
  session_id: string;
  cursor?: TranscriptCursor | null;
  max_records?: number;
  max_bytes?: number;
  record_id?: string | null;
}

/** Explicit search/saved-query content choice. Tool bodies are off by default. */
export interface TranscriptContentScope {
  conversation: boolean;
  tool_calls: boolean;
  tool_results: boolean;
}

export type TranscriptSearchTarget =
  | { kind: 'source_record'; session_id: string; record_id: string; block_index: number }
  | { kind: 'retained_turn'; session_id: string; session_identity: string; snapshot_revision: string; turn_id: string; field: 'user_message' | 'last_agent_message' };
export type TranscriptSearchPosition =
  | { phase: 'source'; cursor: TranscriptCursor; incomplete: boolean }
  | { phase: 'retained'; session_identity: string; snapshot_revision: string; next_turn: number; incomplete: boolean };
export interface TranscriptSearchCursor {
  session_id: string;
  query: string;
  scope: TranscriptContentScope;
  position: TranscriptSearchPosition;
}
export interface TranscriptSearchRequest {
  session_id: string;
  query: string;
  scope: TranscriptContentScope;
  cursor?: TranscriptSearchCursor | null;
}
export interface TranscriptSearchSnippet {
  text: string;
  match_start: number;
  match_end: number;
  truncated_before: boolean;
  truncated_after: boolean;
}
export interface TranscriptSearchHit {
  target: TranscriptSearchTarget;
  content_kind: string;
  snippet: TranscriptSearchSnippet;
}
export interface TranscriptSearchPage {
  phase: 'source' | 'retained';
  hits: TranscriptSearchHit[];
  next_cursor: TranscriptSearchCursor | null;
  issues: string[];
  source_complete: boolean;
  retained_complete: boolean;
  scanned_records: number;
  scanned_messages: number;
}
export interface RetainedSearchLanding {
  target: TranscriptSearchTarget;
  text: string;
  truncated: boolean;
}
export interface TranscriptBlock {
  kind: string;
  text: string;
  call_id: string | null;
  name: string | null;
  edit: { path: string | null; before: string; after: string } | null;
}
export interface TranscriptPresentation {
  role: string | null;
  timestamp: string | null;
  blocks: TranscriptBlock[];
}
export interface TranscriptRecord {
  context_evidence?: {
    contributors: string[];
    compaction: boolean;
    pre_compaction_tokens: number | null;
    input_tokens: number | null;
    output_tokens: number | null;
    context_window: number | null;
    unsupported: boolean;
  } | null;
  id: string;
  byte_offset: number;
  byte_length: number;
  raw_json: string | null;
  presentation?: TranscriptPresentation | null;
  kind: string | null;
  message_id: string | null;
  issue: string | null;
}
export interface TranscriptPage {
  provider: string | null;
  availability: 'available' | 'partial' | 'missing' | 'unreadable' | 'unsupported' | 'unknown_session' | 'cursor_invalid';
  issues: string[];
  records: TranscriptRecord[];
  next_cursor: TranscriptCursor | null;
  source_complete: boolean;
}
/** Ephemeral board metadata. No source text, arguments, edits or raw JSON. */
export interface ExecutionRecord {
  record_id: string;
  timestamp: string | null;
  role: string | null;
  issue: string | null;
  blocks: { kind: string; call_id: string | null; name: string | null }[];
}
export interface ExecutionPage {
  availability: TranscriptPage['availability'];
  issues: string[];
  records: ExecutionRecord[];
  next_cursor: TranscriptCursor | null;
  source_complete: boolean;
}
export type FindingState = 'new' | 'persistent' | 'improving' | 'resolved' | 'suppressed' | 'not_applicable';
export interface WorkflowRequest {
  session_ids: string[];
  from: string | null;
  to: string | null;
}
export interface WorkflowMetric {
  id: string;
  denominator_is: string;
  value: number | null;
  numerator: number;
  denominator: number;
}
export interface WorkflowMeasure extends WorkflowMetric {
  unit: string;
  coverage_is: string;
  covered_samples: number;
  eligible_samples: number;
  missing_data: string | null;
}
export interface WorkflowWindow {
  from: string;
  to: string;
  ledger_metrics: {
    schema_version: number;
    from: string | null;
    to: string | null;
    sessions: number;
    metrics: WorkflowMetric[];
  };
  additional_metrics: WorkflowMeasure[];
  analyzed_sessions: number;
  unavailable_sessions: number;
  drilldowns: { dimension: 'project' | 'model' | 'category'; value: string;
    sessions: number; tool_calls: number; classified_turns: number }[];
}
export interface FindingObservation {
  sessions: number;
  tool_calls: number;
  findings: number;
  likely_avoidable_calls: number;
  analyzer_version: number | null;
  coverage_complete: boolean;
  window_duration_ms: number;
}
export interface FindingLifecycle {
  first_observed_at: string; last_observed_at: string; state: FindingState;
  suppressed: boolean; revision: number; analyzer_changed: boolean;
}
export interface FindingSuppressionEdit {
  provider: string; project_id: string | null; rule_id: string;
  expected_revision: number; suppressed: boolean;
}
export interface WorkflowFinding {
  lifecycle: FindingLifecycle | null;
  id: string;
  provider: string;
  project_id: string | null;
  rule_id: string;
  before: FindingObservation;
  after: FindingObservation;
  comparison: {
    version: number;
    state: FindingState;
    comparable: boolean;
    before_calls_per_100: number | null;
    after_calls_per_100: number | null;
    observed_change_per_100_calls: number | null;
    limitations: string[];
  };
  evidence: { session_id: string; turn_id: string | null; timestamp: string | null }[];
  evidence_truncated: boolean;
}
export interface WorkflowReport {
  historical_findings: { id: string; provider: string; project_id: string | null;
    rule_id: string; lifecycle: FindingLifecycle }[];
  version: number;
  generated_at: string;
  analyzer_version: number;
  selected_sessions: number;
  coverage_complete: boolean;
  before: WorkflowWindow;
  after: WorkflowWindow;
  findings: WorkflowFinding[];
  setup_health: {
    source_configuration_valid: boolean;
    generated_at: string;
    last_scan_at: string | null;
    providers: { provider: string; state: ProviderHealthState; configured_roots: number;
      available_roots: number; parsed_files: number; parse_failures: number;
      durable_sessions: number; fallback_pricing_used: boolean; reasons: string[] }[];
  } | null;
  limitations: string[];
}
export type ControlledActionDraft =
  | { kind: 'budget_guard'; draft: { budget_id: string; expected_config_revision: string } }
  | { kind: 'workflow_remediation'; draft: { scope: WorkflowRequest; finding_id: string;
      expected_finding_revision: number } };
export interface ControlledActionPreview {
  contract_version: number;
  kind: 'budget_guard' | 'workflow_remediation';
  target_type: string;
  redacted_target: string;
  source_revision: string;
  preconditions: { code: string; satisfied: boolean }[];
  proposed_change: string;
  backup_requirement: string;
  postcondition_requirement: string;
  apply_available: false;
  undo_available: false;
  unavailable_reason: 'writes_disabled_pending_security_review';
}
/** Private desktop organization; never part of SessionSummary or exports. */
export interface AnnotationIdentity { session_key: string; fingerprint: string; anchor: string }
export interface RecordBookmark { identity: AnnotationIdentity; revision: number; bookmarked: boolean }
export interface RecordBookmarkList { identity: AnnotationIdentity; bookmarks: RecordBookmark[]; recovery_backup_unrestored: boolean }
export interface HumanOutcome {
  label: 'not_rated' | 'accepted' | 'rejected' | 'unresolved';
  repair_minutes: number | null;
  first_pass_accepted: boolean | null;
}
export interface OrganizationSummary {
  identity: AnnotationIdentity; revision: number; pinned: boolean; has_note: boolean; tags: string[];
  outcome?: HumanOutcome;
}
export interface SessionAnnotation { summary: OrganizationSummary; note: string; recovery_backup_unrestored?: boolean }
export interface AnnotationEdit {
  identity: AnnotationIdentity; revision: number; pinned: boolean; note: string; tags: string[];
  /** Omitted by older editors: preserve the existing human outcome. */
  outcome?: HumanOutcome;
}
export interface SavedSearchDefinition {
  name: string; query: string; scope: string;
  content_scope: 'summary' | 'session_content';
  content_classes: TranscriptContentScope;
  session_key: string | null; fingerprint: string | null;
  from: string | null; to: string | null; model: string;
  show_active: boolean; show_archived: boolean; show_subagents: boolean;
  pinned_only: boolean; tags: string[];
}
export interface SavedSearch { id: number; revision: number; definition: SavedSearchDefinition }

export interface CuratedContent {
  name: string; rubric: string; expected_outcome: string; source_provider: string;
  source_fingerprint_at_capture: string; source_records: string[]; captured_at: string;
  blocks: { role: string; text: string; truncated: boolean }[]; redactions: number;
}
export interface CuratedCase {
  id: number; version: number; session_key: string; fingerprint: string; content_hash: string; content: CuratedContent;
}
export interface CuratedDataset {
  export_digest?: string;
  format_version: number; revision: number; earliest_change_revision: number | null;
  cases: CuratedCase[];
  changes: { revision: number; case_id: number; change: string; content_hash: string }[];
  recovery_backup_unrestored: boolean;
}
export interface CuratedRequest {
  identity: AnnotationIdentity; outcome_revision: number; dataset_revision: number;
  case_id: number | null; record_ids: string[]; name: string; rubric: string; redact_phrases: string[];
}
export interface CuratedPreview { token: string; content: CuratedContent; replacing_case: number | null }
export interface CuratedCandidates {
  records: { record_id: string; role: string; excerpt: string }[];
  next_cursor: TranscriptCursor | null; availability: string;
}

export interface PromptVariant { id: string; prompt_id: string; prompt_version: string; prompt: string; provider: string; model: string; service_tier: string }
export interface FrozenRate { configured_version: number; currency: string; table: string; quoted_at: string; resolved_model: string; basis: PricingBasis; cache_creation_basis: PricingBasis; effective_per_million: ModelRate | null; applied_tier_multiplier: number | null; modifier: { id: string; surface: string; effective_from: string; effective_to: string | null; evidence: string; verified_at: string } | null }
export interface FrozenVariant { conditions: PromptVariant; prompt_hash: string; rate: FrozenRate }
export interface ExperimentMember { case_id: number; dataset_case_version: number; dataset_content_hash: string; input_hash: string; expected_outcome: string }
export interface ExperimentManifest { format_version: number; mode: string; active_replay: string; name: string; rubric: string; dataset_revision: number; captured_at: string; rate_snapshot_hash: string; variants: FrozenVariant[]; members: ExperimentMember[] }
export interface FreezeRequest { dataset_revision: number; name: string; rubric: string; variants: PromptVariant[]; redact_phrases: string[] }
export interface FreezePreview { token: string; manifest: ExperimentManifest; inputs: [number, CuratedContent][] }
export interface ImportedRun { case_id: number; variant: string; status: string; output?: string; elapsed_ms: number | null; actual_cost_usd: number | null; tokens: TokenTotals | null; quality: string | null; observed_input_hash?: string; observed_model?: string; observed_provider?: string; observed_service_tier?: string; observed_prompt_id?: string; observed_prompt_version?: string; observed_prompt_hash?: string; observed_rate_hash?: string }
export interface FrozenRun { record: ImportedRun; output_truncated: boolean; estimated_api_usd: number | null; estimate_basis: PricingBasis; condition_notes: string[] }
export interface ImportRequest { experiment_id: number; revision: number; rows: ImportedRun[]; redact_phrases: string[] }
export interface ImportPreview { token: string; experiment_id: number; revision: number; rows: FrozenRun[] }
export interface ExperimentHeader { id: number; revision: number; name: string; dataset_revision: number; expected_cases: number; captured_at: string }
export interface ComparisonMeasure { count: number; mean: number | null; minimum: number | null; maximum: number | null }
export interface VariantSummary { variant: string; expected_cases: number; completed: number; failed: number; missing: number; missing_output: number; accepted: number; rejected: number; unresolved: number; not_rated: number; quality_missing: number; conditions_unverified: number; elapsed_ms: ComparisonMeasure; actual_cost_usd: ComparisonMeasure; estimated_api_usd: ComparisonMeasure }
export interface ExperimentReport { export_digest: string; id: number; revision: number; manifest: ExperimentManifest; cases: { member: ExperimentMember; input: CuratedContent | null; a: FrozenRun | null; b: FrozenRun | null }[]; summaries: VariantSummary[]; removed_cases: number; current_dataset_revision: number; current_selected_pricing_differs: boolean; changed_conditions: string[]; paired_elapsed_delta_ms: ComparisonMeasure; paired_actual_cost_delta_usd: ComparisonMeasure; paired_estimated_cost_delta_usd: ComparisonMeasure; recovery_backup_unrestored: boolean }

export type ProviderStatusIndicator = 'operational' | 'minor' | 'major' | 'critical' | 'maintenance';
export interface ProviderServiceStatus {
  provider: string;
  source_url: string | null;
  state: 'disabled' | 'unsupported' | 'pending' | 'current' | 'stale' | 'unavailable';
  current_indicator: ProviderStatusIndicator | null;
  last_known_indicator: ProviderStatusIndicator | null;
  checked_at: string | null;
  source_updated_at: string | null;
  last_attempt_at: string | null;
  next_attempt_at: string | null;
  failure: 'offline_or_timeout' | 'rate_limited' | 'http_error' | 'invalid_response' | null;
}
export interface ProviderServiceStatusSnapshot {
  enabled: boolean;
  providers: ProviderServiceStatus[];
}
// Local transcript evidence, separate from quota and accounting authorities.
export type AttentionEventKind = 'turn_started' | 'input_requested' | 'tool_completed' | 'tool_failed' | 'turn_completed' | 'turn_interrupted';
export type AttentionState = 'working' | 'waiting' | 'idle' | 'error' | 'unknown';
export interface AttentionPreferences { revision: number; categories: AttentionEventKind[]; providers: string[]; tool_kind: 'read' | 'search' | 'mutation' | 'command' | 'other' | null; stale_after_seconds: number; }
export interface AttentionObservation { session_ref: string; provider: string; state: AttentionState; observed_state: AttentionState; observed_at: string; source: string; stale: boolean; partial: boolean; }
export interface AttentionAlert { id: string; session_ref: string; provider_label: string; category: AttentionEventKind; observed_at: string; source: string; }
export interface AttentionSnapshot { preferences: AttentionPreferences; available: boolean; observations: AttentionObservation[]; alerts: AttentionAlert[]; }
