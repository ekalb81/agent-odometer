// TypeScript types mirroring Rust structs in src-tauri/src/model.rs, config.rs, and rates.rs.
// Keep in sync when Rust types change.

export type Harness = 'codex' | 'claude_code';

export interface TokenTotals {
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
}

export type ToolKind = 'read' | 'search' | 'mutation' | 'command' | 'other';
export type ToolOutcome = 'pending' | 'success' | 'failure' | 'unknown';
export type TaskCategory = 'planning' | 'exploration' | 'coding' | 'debugging' | 'testing' | 'review' | 'other';

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

export interface Session {
  id: string;
  harness: Harness;
  thread_name: string | null;
  forked_from_id: string | null;
  parent_thread_id: string | null;
  agent_path: string | null;
  agent_nickname: string | null;
  file_path: string;
  archived: boolean;
  started_at: string; // ISO8601
  last_event_at: string; // ISO8601
  working_directory: string | null;
  originator: string | null;
  source: string | null;
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
    total_tokens: number;
    delta: TokenTotals;
  }[];
  turns: TurnInfo[];
  tool_observations: ToolObservation[];
  tool_metrics: ToolMetrics;
  tool_metrics_by_model: Record<string, ToolMetrics>;
  category_totals: Partial<Record<TaskCategory, CategoryMetric>>;
  optimization_findings: OptimizationFinding[];
}

/** Token usage grouped by (model, service_tier); prices usage exactly without the full event history. */
export interface TierBucket {
  model: string;
  service_tier: string | null;
  tokens: TokenTotals;
}

/** Date-scoped rollup returned by sessions_in_ranges. */
export interface RangeTotals {
  tokens: TokenTotals;
  buckets: TierBucket[];
  tool_metrics: ToolMetrics;
  tool_metrics_by_model: Record<string, ToolMetrics>;
  optimization_findings_count: number;
  optimization_summary?: OptimizationSummary;
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
  harness: Harness;
  thread_name: string | null;
  forked_from_id: string | null;
  parent_thread_id: string | null;
  agent_path: string | null;
  agent_nickname: string | null;
  file_path: string;
  archived: boolean;
  started_at: string; // ISO8601
  last_event_at: string; // ISO8601
  working_directory: string | null;
  originator: string | null;
  source: string | null;
  cli_version: string | null;
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
}

/** Bulk-scan progress, from get_scan_status and "scan-progress" events. */
export interface ScanStatus {
  done: number;
  total: number;
  complete: boolean;
  /** Wall-clock duration of the last completed scan; null while running. */
  elapsed_ms: number | null;
}

export interface Config {
  session_roots: string[];
  archive_roots: string[];
  session_index_path: string;
  claude_session_roots: string[];
  performance_tracking_enabled: boolean;
  performance_log_max_mb: number;
  instructions_enabled: boolean;
  instructions_tab_visible: boolean;
  instruction_roots: InstructionRoot[];
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
  scanned_at: string;
}

export interface InstructionContent {
  path: string;
  content: string;
}

export interface PerformanceStatus {
  enabled: boolean;
  max_log_mb: number;
  stored_bytes: number;
  recorded_this_run: number;
  dropped_this_run: number;
}

export interface ModelRate {
  input: number;
  cached_input: number;
  output: number;
  reasoning: number;
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
