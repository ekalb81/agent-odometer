use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TokenTotals {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub thread_name: Option<String>,
    pub forked_from_id: Option<String>,
    pub file_path: String,
    pub archived: bool,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub last_event_at: chrono::DateTime<chrono::Utc>,
    pub working_directory: Option<String>,
    pub originator: Option<String>,
    pub source: Option<String>,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub model: Option<String>,
    pub plan_type: Option<String>,
    pub credits_unlimited: Option<bool>,
    pub credits_balance: Option<f64>,
    pub context_window: Option<u32>,
    pub total_turns: u32,
    /// Truncated first user message, used as a display-name fallback.
    pub first_user_message: Option<String>,
    pub tokens_total: TokenTotals,
    pub tokens_by_model: HashMap<String, TokenTotals>,
}
