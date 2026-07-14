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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenHistoryPoint {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Active model at the moment of this event (for per-period credit math).
    /// None only if no turn_context had set a model yet.
    pub model: Option<String>,
    pub service_tier: Option<String>,
    /// Cumulative total_tokens at this event — drives the sparkline.
    pub total_tokens: u64,
    /// last_token_usage for this event — the per-call delta. All zeros if absent.
    pub delta: TokenTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    #[default]
    InProgress,
    Completed,
    Aborted,
    RolledBack,
}

/// One turn = one user prompt and the agent's work until task_complete.
/// Identified by the `turn_id` that Codex stamps on turn_context /
/// task_started / task_complete events.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TurnInfo {
    pub turn_id: String,
    /// 1-based ordinal in the session.
    pub index: u32,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub collaboration_mode: Option<String>,
    pub service_tier: Option<String>,
    pub status: TurnStatus,
    pub abort_reason: Option<String>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub duration_ms: Option<u64>,
    pub time_to_first_token_ms: Option<u64>,
    /// Truncated user prompt that opened the turn.
    pub user_message: Option<String>,
    /// Truncated final agent message from task_complete.
    pub last_agent_message: Option<String>,
    /// Tokens attributed to this turn (sum of reconciled per-event deltas).
    pub tokens: TokenTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub thread_name: Option<String>,
    pub forked_from_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub agent_path: Option<String>,
    pub agent_nickname: Option<String>,
    pub file_path: String,
    pub archived: bool,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub last_event_at: chrono::DateTime<chrono::Utc>,
    pub working_directory: Option<String>,
    pub originator: Option<String>,
    pub source: Option<String>,
    pub history_mode: Option<String>,
    pub memory_mode: Option<String>,
    pub cli_version: Option<String>,
    pub model_provider: Option<String>,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub plan_type: Option<String>,
    pub credits_unlimited: Option<bool>,
    pub credits_balance: Option<f64>,
    pub context_window: Option<u32>,
    pub total_turns: u32,
    /// Truncated first user message, used as a display-name fallback.
    pub first_user_message: Option<String>,
    pub tokens_total: TokenTotals,
    pub tokens_by_model: HashMap<String, TokenTotals>,
    pub tokens_history: Vec<TokenHistoryPoint>,
    pub turns: Vec<TurnInfo>,
}
