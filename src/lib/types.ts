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
}
