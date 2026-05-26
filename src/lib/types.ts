// TypeScript types mirroring Rust structs in src-tauri/src/model.rs, config.rs, and rates.rs.
// Keep in sync when Rust types change.

export interface TokenTotals {
  input_tokens: number;
  cached_input_tokens: number;
  output_tokens: number;
  reasoning_output_tokens: number;
  total_tokens: number;
}

export interface Session {
  id: string;
  thread_name: string | null;
  forked_from_id: string | null;
  file_path: string;
  archived: boolean;
  started_at: string; // ISO8601
  last_event_at: string; // ISO8601
  working_directory: string | null;
  originator: string | null;
  source: string | null;
  cli_version: string | null;
  model_provider: string | null;
  model: string | null;
  plan_type: string | null;
  credits_unlimited: boolean | null;
  credits_balance: number | null;
  context_window: number | null;
  total_turns: number;
  first_user_message: string | null;
  tokens_total: TokenTotals;
  tokens_by_model: Record<string, TokenTotals>;
  tokens_history: { timestamp: string; total_tokens: number }[];
}

export interface Config {
  session_roots: string[];
  archive_roots: string[];
  session_index_path: string;
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
}
