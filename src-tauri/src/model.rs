use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use crate::provider::{claude_code_provider_id, codex_provider_id, ProviderId};

/// Open provider identity for a session's transcript. The name `Harness` is
/// retained crate-wide during the migration off the closed two-variant enum;
/// the wire form is unchanged (bare snake_case strings such as "codex" and
/// "claude_code"), and sessions persisted without the field still default to
/// codex via the serde defaults on the structs below.
pub type Harness = ProviderId;

/// Whether Odometer can currently read the transcript that supplied this
/// session. Durable storage retains the parsed session even after a source is
/// no longer available, so this is deliberately distinct from `archived`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    #[default]
    Present,
    Missing,
}

/// Stable identity for a provider session. Unlike `Session::id`, this is
/// namespaced by harness and is safe to use as a durable storage key.
pub fn storage_id_for_session(provider: &ProviderId, provider_session_id: &str) -> String {
    // Codex threads keep their historical "thread" segment; every other
    // provider (including claude_code) uses the generic "session" segment,
    // which preserves all previously persisted storage ids byte-for-byte.
    if *provider == codex_provider_id() {
        format!("codex:thread:{provider_session_id}")
    } else {
        format!("{provider}:session:{provider_session_id}")
    }
}

/// Stable identity for a Claude Code subagent transcript. Claude records use
/// the parent `sessionId`, so the provider `agentId` distinguishes siblings.
/// `agent_id` may be a filename-stem fallback for historical transcripts that
/// did not record an `agentId`.
pub fn storage_id_for_claude_subagent(parent_session_id: &str, agent_id: &str) -> String {
    format!("claude_code:subagent:{parent_session_id}:{agent_id}")
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Search,
    Mutation,
    Command,
    #[default]
    Other,
}

impl ToolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Search => "search",
            Self::Mutation => "mutation",
            Self::Command => "command",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Pending,
    Success,
    Failure,
    #[default]
    Unknown,
}

/// Versioned, normalized tool-origin dimension (issue #44). Computed once
/// during parsing from data `ToolObservation` already carries (`kind` and
/// `providers`) — never by reparsing a transcript in the UI or optimizer.
///
/// - `Mcp`: the call resolved at least one MCP server identity.
/// - `Core`: a generic file/search/shell primitive (`kind` is `Read`,
///   `Search`, `Mutation`, or `Command`) with no MCP provider.
/// - `Provider`: a harness-native tool that is not one of the generic
///   primitives above and not MCP (e.g. subagent orchestration, todo lists,
///   web search) — `kind` is `Other` with no MCP provider.
/// - `Unknown`: the default for historical data that predates this field
///   (deserialized snapshots, and durable-ledger facts recorded before the
///   dimension existed). Never emitted by fresh parsing; unknown means
///   "not available", not zero.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolOrigin {
    Core,
    Mcp,
    Provider,
    #[default]
    Unknown,
}

impl ToolOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Core => "core",
            Self::Mcp => "mcp",
            Self::Provider => "provider",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_str_or_unknown(value: &str) -> Self {
        match value {
            "core" => Self::Core,
            "mcp" => Self::Mcp,
            "provider" => Self::Provider,
            _ => Self::Unknown,
        }
    }

    /// Classifies origin from data already collected by `observe_call`.
    /// Deterministic and total: fresh observations always resolve to
    /// `Core`, `Mcp`, or `Provider`, never `Unknown` (that variant is
    /// reserved for data recorded before this dimension existed).
    pub fn classify(kind: ToolKind, providers: &[String]) -> Self {
        if !providers.is_empty() {
            Self::Mcp
        } else if kind == ToolKind::Other {
            Self::Provider
        } else {
            Self::Core
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolObservation {
    pub call_id: String,
    pub turn_id: Option<String>,
    #[serde(default = "crate::provider::codex_provider_id")]
    pub harness: Harness,
    pub model: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub kind: ToolKind,
    pub name: String,
    /// Normalized MCP server names inferred from the tool name or an
    /// orchestration payload. Values are bounded and contain no arguments.
    #[serde(default)]
    pub providers: Vec<String>,
    /// Effective tool names inferred from the direct call or an orchestration
    /// payload. Values are bounded and contain no arguments or source text.
    #[serde(default)]
    pub effective_tools: Vec<String>,
    /// Stable hashed identity; raw arguments and paths are never retained.
    pub target: Option<String>,
    /// Stable hashed identity for the underlying resource, excluding read
    /// selectors such as line ranges. This lets a mutation invalidate prior
    /// reads without retaining the resource name or path.
    #[serde(default)]
    pub resource_id: Option<String>,
    /// Normalized tool-origin dimension (issue #44). Defaulted to `Unknown`
    /// for data recorded before this field existed; fresh observations
    /// always compute a real value via `ToolOrigin::classify`.
    #[serde(default)]
    pub origin: ToolOrigin,
    /// Safe shell command family (issue #44) — the leading token of a
    /// `Command`-kind call's command line, matched against a bounded
    /// allowlist (`telemetry::classify_shell_family`) and bucketed to
    /// `"other"` when not recognized. Never the raw command text. `None`
    /// for non-command calls or when no command text was available.
    #[serde(default)]
    pub shell_family: Option<String>,
    /// Language inferred from a read/write target's file extension (issue
    /// #44), matched against a bounded allowlist
    /// (`telemetry::classify_language`). Never the file path itself. `None`
    /// when no path was available or its extension is not recognized.
    #[serde(default)]
    pub language: Option<String>,
    pub outcome: ToolOutcome,
    pub duration_ms: Option<u64>,
    pub output_bytes: u64,
}

/// Additive counters for one (dimension_kind, dimension_value) pair in the
/// issue #44 open-set dimensions — MCP server identity, shell command
/// family, language, and context source. Unlike `ToolMetrics`, these
/// dimensions have unbounded label sets, so they are not flat
/// `rollup_tool_metrics` columns; see `RangeTotals::tool_dimensions` and the
/// `rollup_tool_dimensions` ledger table (`history_store.rs`).
///
/// `tokens` is populated only for the `context_source` dimension kind, which
/// is derived from `TokenTotals` rather than tool calls and so has no
/// `calls`/`failures`/`output_bytes`/`duration_ms` of its own; every other
/// dimension kind (`mcp_server`, `shell_family`, `language`) populates the
/// first four fields and leaves `tokens` at 0. There is no
/// `retries`/`mutation_targets`-style chain-derived field here: this struct
/// carries no `turn_id`/`target`, so — unlike `ToolMetrics::retry_count` —
/// there is no chain to reconstruct, and a plain sum of `calls` across
/// buckets is always correct.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ToolDimensionMetrics {
    pub calls: u64,
    pub failures: u64,
    pub output_bytes: u64,
    pub duration_ms: u64,
    #[serde(default)]
    pub tokens: u64,
}

impl ToolDimensionMetrics {
    pub fn add_assign(&mut self, other: &Self) {
        self.calls += other.calls;
        self.failures += other.failures;
        self.output_bytes += other.output_bytes;
        self.duration_ms += other.duration_ms;
        self.tokens += other.tokens;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ToolMetrics {
    pub calls: u64,
    pub reads: u64,
    pub searches: u64,
    pub mutations: u64,
    pub commands: u64,
    pub other: u64,
    pub successes: u64,
    pub failures: u64,
    pub unknown: u64,
    pub mutation_targets: u64,
    pub one_shot_mutations: u64,
    pub retry_count: u64,
    pub duration_ms: u64,
    pub output_bytes: u64,
    /// Origin-dimension breakdown (issue #44). These four fields always sum
    /// to `calls`: every observation resolves to exactly one origin,
    /// including `unknown_origin_calls` for data recorded before the
    /// dimension existed. Plain additive counters at the existing
    /// `(session_key, hour_bucket, model)` rollup grain — this does not
    /// change `rollup_tool_metrics` cardinality.
    #[serde(default)]
    pub core_origin_calls: u64,
    #[serde(default)]
    pub mcp_origin_calls: u64,
    #[serde(default)]
    pub provider_origin_calls: u64,
    #[serde(default)]
    pub unknown_origin_calls: u64,
}

impl ToolMetrics {
    pub fn add_assign(&mut self, value: &Self) {
        self.calls += value.calls;
        self.reads += value.reads;
        self.searches += value.searches;
        self.mutations += value.mutations;
        self.commands += value.commands;
        self.other += value.other;
        self.successes += value.successes;
        self.failures += value.failures;
        self.unknown += value.unknown;
        self.mutation_targets += value.mutation_targets;
        self.one_shot_mutations += value.one_shot_mutations;
        self.retry_count += value.retry_count;
        self.duration_ms += value.duration_ms;
        self.output_bytes += value.output_bytes;
        self.core_origin_calls += value.core_origin_calls;
        self.mcp_origin_calls += value.mcp_origin_calls;
        self.provider_origin_calls += value.provider_origin_calls;
        self.unknown_origin_calls += value.unknown_origin_calls;
    }
}

#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum TaskCategory {
    Planning,
    Exploration,
    Coding,
    Debugging,
    Testing,
    Review,
    #[default]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnClassification {
    pub version: u32,
    pub category: TaskCategory,
    pub confidence: f32,
    pub signals: Vec<String>,
}

impl Default for TurnClassification {
    fn default() -> Self {
        Self {
            version: 1,
            category: TaskCategory::Other,
            confidence: 0.0,
            signals: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CategoryMetric {
    pub turns: u64,
    pub tokens: TokenTotals,
    pub tool_calls: u64,
    #[serde(default)]
    pub buckets: Vec<TierBucket>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct OptimizationFinding {
    pub version: u32,
    pub rule_id: String,
    pub severity: String,
    /// Analyzer confidence, not a quality score: high, medium, or low.
    #[serde(default)]
    pub confidence: String,
    pub turn_id: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,
    pub evidence: String,
    pub remediation: String,
    /// Number of observations represented by this finding.
    #[serde(default)]
    pub occurrences: u64,
    /// Conservative estimate of calls that could likely have been avoided.
    #[serde(default)]
    pub avoidable_calls: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct OptimizationSummary {
    pub findings: u64,
    pub warnings: u64,
    pub likely_avoidable_calls: u64,
    pub by_rule: BTreeMap<String, u64>,
}

impl OptimizationSummary {
    pub fn from_findings<'a>(findings: impl IntoIterator<Item = &'a OptimizationFinding>) -> Self {
        let mut out = Self::default();
        for finding in findings {
            out.findings += 1;
            if finding.severity == "warning" {
                out.warnings += 1;
            }
            out.likely_avoidable_calls += finding.avoidable_calls;
            *out.by_rule.entry(finding.rule_id.clone()).or_default() += 1;
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TokenTotals {
    pub input_tokens: u64,
    pub cached_input_tokens: u64,
    /// Anthropic cache-creation ("cache write") tokens. A subset of
    /// `input_tokens`, disjoint from `cached_input_tokens` (cache reads).
    /// Always 0 for Codex, whose provider does not report this dimension.
    /// Defaulted for backward compatibility with durable/cached data written
    /// before this field existed.
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_output_tokens: u64,
    pub total_tokens: u64,
}

/// Field-wise accumulation, defined once.
///
/// This fold was previously written out per module — a new field had to be
/// added to every copy, and any copy that was missed would have silently
/// under-counted on whichever surface used it. For a tool whose entire output
/// is token accounting, that is the wrong failure mode, so the operation lives
/// with the type.
impl std::ops::AddAssign<&TokenTotals> for TokenTotals {
    fn add_assign(&mut self, other: &TokenTotals) {
        self.input_tokens += other.input_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.output_tokens += other.output_tokens;
        self.reasoning_output_tokens += other.reasoning_output_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TokenHistoryPoint {
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Active model at the moment of this event (for per-period credit math).
    /// None only if no turn_context had set a model yet.
    pub model: Option<String>,
    pub service_tier: Option<String>,
    /// Complete input-token count reported for this provider request. This is
    /// kept separate from `delta`, which can include a reconciliation
    /// remainder when a resumed rollout first becomes observable.
    #[serde(default)]
    pub request_input_tokens: Option<u64>,
    /// Cumulative total_tokens at this event — drives the sparkline.
    pub total_tokens: u64,
    /// last_token_usage for this event — the per-call delta. All zeros if absent.
    pub delta: TokenTotals,
}

/// One provider-reported subscription-usage window. The value is deliberately
/// kept as a percentage reported by the harness; Odometer does not derive a
/// quota charge from tokens or an assumed subscription cap.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RateLimitWindow {
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub resets_at: Option<DateTime<Utc>>,
}

/// Account-wide rate-limit snapshot attached to a Codex token event.
///
/// Issue #153: Codex attaches `rate_limits` to every token-count event, which
/// fires far more often than the window values actually change — a
/// 600-transcript field measurement found 81.6% of points byte-identical to
/// their immediate predecessor in both `primary` and `secondary`. Rather than
/// storing one point per event, [`RateLimitSnapshotPoint::record`] collapses
/// a run of consecutive events that report identical `turn_id`, `limit_id`,
/// `primary`, and `secondary` into a single stored point, using
/// `run_started_at`/`observation_count` to remember what the run covered.
///
/// `timestamp` keeps its pre-#153 meaning unchanged: for a collapsed run it
/// is the run's *last* observation, which is what staleness detection and
/// `series.last()`-style "latest observation" reads have always used. The
/// two fields below carry exactly what `quota.rs`'s consumers need to
/// reconstruct pre-#153 behavior even though interior duplicate points are
/// gone:
///
/// - `forecast_from_series` computes pace from only the first and last
///   in-window point, so the run's first timestamp must survive for pace-
///   start math even though the point carrying it is no longer stored
///   separately.
/// - `MIN_EVIDENCE_POINTS` gates forecasting on observation *count* — if
///   collapsing silently shrank that count, a forecast that used to clear
///   the gate could vanish even though nothing about the underlying evidence
///   changed.
///
/// Both new fields are `#[serde(default)]`, so `session_json` rows written
/// before #153 (every point implicitly a run of one) deserialize correctly
/// without a migration or rescan — see `run_started_at()`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RateLimitSnapshotPoint {
    pub timestamp: DateTime<Utc>,
    pub turn_id: Option<String>,
    pub limit_id: Option<String>,
    pub primary: Option<RateLimitWindow>,
    pub secondary: Option<RateLimitWindow>,
    /// First observation's timestamp in this point's collapsed run of
    /// consecutive, field-identical observations (same `turn_id`,
    /// `limit_id`, `primary`, and `secondary`). `None` for a run of exactly
    /// one observation and for any point written before #153 — both cases
    /// mean the same thing (`run_started_at()` below), so there is no
    /// reason to pay the serialized-bytes cost of writing `timestamp` again
    /// as `Some(timestamp)` for the overwhelmingly common single-point case.
    #[serde(default)]
    pub run_started_at: Option<DateTime<Utc>>,
    /// How many consecutive raw observations this point collapses (#153).
    /// Defaults to 1 both for pre-#153 data (which only ever stored one
    /// observation per point) and for a freshly-collapsed run of exactly
    /// one observation — in both cases the true historical count *is* 1, so
    /// the default is exact, not a placeholder.
    #[serde(default = "one_observation")]
    pub observation_count: u32,
}

fn one_observation() -> u32 {
    1
}

impl RateLimitSnapshotPoint {
    /// This run's first observation, falling back to `timestamp` when
    /// `run_started_at` is unset — the correct value for both a genuine
    /// one-observation run and pre-#153 data, which are indistinguishable
    /// and were always semantically "a run of one" (see the struct doc).
    pub fn run_started_at(&self) -> DateTime<Utc> {
        self.run_started_at.unwrap_or(self.timestamp)
    }

    /// Appends one raw observation to `history`, collapsing it into the
    /// current last run when `turn_id`, `limit_id`, `primary`, and
    /// `secondary` all match that run's — otherwise starting a new run.
    /// `history` must already be in ascending-timestamp order (true of
    /// every call site: `parser.rs` calls this once per event, in the
    /// order events appear in an append-only rollout file).
    ///
    /// Requiring every field (not just `primary`/`secondary`) to match
    /// before merging is deliberately conservative: it keeps
    /// `turn_receipts.rs`'s per-turn `turn_id` lookups exact (a run can
    /// never straddle a turn boundary, so "the last point with this
    /// turn_id" still resolves to a single, correctly-valued point) and
    /// keeps `limit_id` identity checks exact too, at a small cost to the
    /// collapse ratio — measured negligible in practice, since Codex fires
    /// far more token-count events within one turn than across turns.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        history: &mut Vec<RateLimitSnapshotPoint>,
        timestamp: DateTime<Utc>,
        turn_id: Option<String>,
        limit_id: Option<String>,
        primary: Option<RateLimitWindow>,
        secondary: Option<RateLimitWindow>,
    ) {
        if let Some(last) = history.last_mut() {
            if last.turn_id == turn_id
                && last.limit_id == limit_id
                && last.primary == primary
                && last.secondary == secondary
            {
                if last.run_started_at.is_none() {
                    last.run_started_at = Some(last.timestamp);
                }
                last.timestamp = timestamp;
                last.observation_count += 1;
                return;
            }
        }
        history.push(RateLimitSnapshotPoint {
            timestamp,
            turn_id,
            limit_id,
            primary,
            secondary,
            run_started_at: None,
            observation_count: 1,
        });
    }

    /// Folds a raw (uncollapsed) sequence of observations through
    /// [`Self::record`], one at a time. A pure convenience for building the
    /// collapsed equivalent of a raw history in one call — tests use this to
    /// derive "the collapsed form of this raw fixture" for differential
    /// comparison against `quota.rs`'s from-scratch oracle.
    pub fn collapse(raw: &[RateLimitSnapshotPoint]) -> Vec<RateLimitSnapshotPoint> {
        let mut collapsed = Vec::new();
        for point in raw {
            Self::record(
                &mut collapsed,
                point.timestamp,
                point.turn_id.clone(),
                point.limit_id.clone(),
                point.primary.clone(),
                point.secondary.clone(),
            );
        }
        collapsed
    }
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
    #[serde(default)]
    pub tool_metrics: ToolMetrics,
    #[serde(default)]
    pub classification: TurnClassification,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    /// Durable, harness-namespaced identity. This never incorporates a source
    /// path, allowing a transcript to move or temporarily disappear.
    #[serde(default)]
    pub storage_id: String,
    #[serde(default = "crate::provider::codex_provider_id")]
    pub harness: Harness,
    pub thread_name: Option<String>,
    pub forked_from_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub agent_path: Option<String>,
    pub agent_nickname: Option<String>,
    pub file_path: String,
    /// Filesystem availability of `file_path`; retained sessions can be
    /// missing their original source without being deleted from storage.
    #[serde(default)]
    pub source_availability: SourceAvailability,
    pub archived: bool,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub last_event_at: chrono::DateTime<chrono::Utc>,
    pub working_directory: Option<String>,
    pub originator: Option<String>,
    pub source: Option<String>,
    /// True only when a Claude subagent lacked a provider `agentId` and its
    /// filename stem had to supply the identity. Durable reconciliation may
    /// use event lineage across source renames only for this legacy case.
    #[serde(default)]
    pub subagent_id_is_path_fallback: bool,
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
    /// Context fill of the most recent API call (raw input+output of the
    /// latest usage event). Unlike `tokens_total` — which is cumulative
    /// throughput and can exceed the window many times over — this is
    /// comparable to `context_window`.
    #[serde(default)]
    pub latest_context_tokens: Option<u64>,
    pub total_turns: u32,
    /// Truncated first user message, used as a display-name fallback.
    pub first_user_message: Option<String>,
    pub tokens_total: TokenTotals,
    pub tokens_by_model: HashMap<String, TokenTotals>,
    pub tokens_history: Vec<TokenHistoryPoint>,
    /// Provider-reported account quota snapshots. Full sessions only: list
    /// summaries intentionally omit this history.
    #[serde(default)]
    pub rate_limits_history: Vec<RateLimitSnapshotPoint>,
    pub turns: Vec<TurnInfo>,
    #[serde(default)]
    pub tool_observations: Vec<ToolObservation>,
    #[serde(default)]
    pub tool_metrics: ToolMetrics,
    #[serde(default)]
    pub tool_metrics_by_model: BTreeMap<String, ToolMetrics>,
    #[serde(default)]
    pub category_totals: BTreeMap<TaskCategory, CategoryMetric>,
    #[serde(default)]
    pub optimization_findings: Vec<OptimizationFinding>,
    /// Stable project-identity key resolved from `working_directory` (#41).
    /// `None` when the session has no working directory to resolve from.
    /// This is the auto-computed identity; user aliases/merges/splits are a
    /// separate overlay applied at read time and never mutate this field.
    #[serde(default)]
    pub project_key: Option<String>,
    /// Local display label for `project_key`. Never redacted here — see
    /// `crate::project_identity` for the redaction/aggregate-export contract.
    #[serde(default)]
    pub project_label: Option<String>,
    #[serde(default)]
    pub project_provenance: Option<crate::project_identity::ProjectProvenance>,
}

/// Token usage grouped by (model, service_tier). Credit math is linear per
/// (model, tier), so these buckets price usage exactly without shipping the
/// full event history.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TierBucket {
    pub model: String,
    pub service_tier: Option<String>,
    pub tokens: TokenTotals,
}

/// Inclusive [from, to] window for range rollups; None is an open bound.
pub type RangeWindow = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Date-scoped rollup of one session's usage, returned by the
/// `sessions_in_ranges` command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RangeTotals {
    /// Sum of all event deltas in range (including events with no model yet).
    pub tokens: TokenTotals,
    /// Priceable usage in range, grouped by (model, tier). Events without a
    /// model are excluded here — their usage is reconciled into a model
    /// bucket by a later event, mirroring the credit math the frontend has
    /// always used.
    pub buckets: Vec<TierBucket>,
    #[serde(default)]
    pub tool_metrics: ToolMetrics,
    #[serde(default)]
    pub tool_metrics_by_model: BTreeMap<String, ToolMetrics>,
    #[serde(default)]
    pub optimization_findings_count: u64,
    #[serde(default)]
    pub optimization_summary: OptimizationSummary,
    /// Issue #44 open-set tool/context dimensions: outer key is the
    /// dimension kind (`"mcp_server"`, `"shell_family"`, `"language"`,
    /// `"context_source"`), inner key is the dimension value. Absent kinds
    /// mean "no ledger-durable data for this window" — the frontend must
    /// still consult `ProviderCapabilities` to distinguish a real zero from
    /// a provider that cannot supply the dimension at all.
    #[serde(default)]
    pub tool_dimensions: BTreeMap<String, BTreeMap<String, ToolDimensionMetrics>>,
}

/// Lightweight wire form of a Session for the list view and live update
/// events. Excludes `turns` and `tokens_history`, which dominate payload
/// size (a large session serializes to ~2 MB; its summary to ~1 KB). The
/// full Session is fetched on demand via `get_session_details`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    #[serde(default)]
    pub storage_id: String,
    pub harness: Harness,
    pub thread_name: Option<String>,
    pub forked_from_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub agent_path: Option<String>,
    pub agent_nickname: Option<String>,
    pub file_path: String,
    #[serde(default)]
    pub source_availability: SourceAvailability,
    pub archived: bool,
    pub started_at: DateTime<Utc>,
    pub last_event_at: DateTime<Utc>,
    pub working_directory: Option<String>,
    pub originator: Option<String>,
    pub source: Option<String>,
    pub cli_version: Option<String>,
    #[serde(default)]
    pub model_provider: Option<String>,
    pub model: Option<String>,
    pub service_tier: Option<String>,
    pub plan_type: Option<String>,
    pub credits_unlimited: Option<bool>,
    pub credits_balance: Option<f64>,
    pub context_window: Option<u32>,
    pub total_turns: u32,
    pub first_user_message: Option<String>,
    pub tokens_total: TokenTotals,
    pub buckets: Vec<TierBucket>,
    #[serde(default)]
    pub tool_metrics: ToolMetrics,
    #[serde(default)]
    pub tool_metrics_by_model: BTreeMap<String, ToolMetrics>,
    #[serde(default)]
    pub category_totals: BTreeMap<TaskCategory, CategoryMetric>,
    #[serde(default)]
    pub optimization_findings_count: u64,
    #[serde(default)]
    pub optimization_summary: OptimizationSummary,
    #[serde(default)]
    pub project_key: Option<String>,
    #[serde(default)]
    pub project_label: Option<String>,
    #[serde(default)]
    pub project_provenance: Option<crate::project_identity::ProjectProvenance>,
}

impl SessionSummary {
    pub fn of(s: &Session) -> Self {
        let optimization_summary = OptimizationSummary::from_findings(&s.optimization_findings);
        Self {
            id: s.id.clone(),
            storage_id: s.effective_storage_id(),
            harness: s.harness.clone(),
            thread_name: s.thread_name.clone(),
            forked_from_id: s.forked_from_id.clone(),
            parent_thread_id: s.parent_thread_id.clone(),
            agent_path: s.agent_path.clone(),
            agent_nickname: s.agent_nickname.clone(),
            file_path: s.file_path.clone(),
            source_availability: s.source_availability,
            archived: s.archived,
            started_at: s.started_at,
            last_event_at: s.last_event_at,
            working_directory: s.working_directory.clone(),
            originator: s.originator.clone(),
            source: s.source.clone(),
            cli_version: s.cli_version.clone(),
            model_provider: s.model_provider.clone(),
            model: s.model.clone(),
            service_tier: s.service_tier.clone(),
            plan_type: s.plan_type.clone(),
            credits_unlimited: s.credits_unlimited,
            credits_balance: s.credits_balance,
            context_window: s.context_window,
            total_turns: s.total_turns,
            first_user_message: s.first_user_message.clone(),
            tokens_total: s.tokens_total.clone(),
            buckets: s.tier_buckets(),
            tool_metrics: s.tool_metrics.clone(),
            tool_metrics_by_model: s.tool_metrics_by_model.clone(),
            category_totals: s.category_totals.clone(),
            optimization_findings_count: s.optimization_findings.len() as u64,
            optimization_summary,
            project_key: s.project_key.clone(),
            project_label: s.project_label.clone(),
            project_provenance: s.project_provenance,
        }
    }
}

/// In-memory resident projection of one session (issue #139).
/// `AppState.sessions` holds this instead of a full [`Session`] so steady-
/// state residency and startup hydration scale with summary size, not with
/// turn/token-event volume — a full session measured ~1 MB resident across a
/// real corpus; its summary well under 1% of that.
///
/// This is deliberately *not* the wire-shape [`SessionSummary`]: it adds a
/// little quota-related state, beyond `SessionSummary`, that
/// `session-updated`'s IPC payload omits on purpose (`AGENTS.md`'s
/// contract). The two consumers that need it after the resident-summary
/// switch — `commands::get_subscription_usage` and
/// `diagnostics::collect_session_stats` — read it from here without ever
/// growing the wire payload.
///
/// Issue #152: this used to hold the session's *entire*
/// `rate_limits_history: Vec<RateLimitSnapshotPoint>`, on the claim (now
/// known false) that it was "typically a handful of points per session —
/// bounded by turn count, not token-event volume". `parser.rs` actually
/// pushes one point per rate-limit-bearing *event*, not per turn: a real
/// session can carry up to 22,395 points, and cloning that whole `Vec` into
/// every resident session was ~0.7 GB of a 3.4 GB process, retained to serve
/// two consumers that only ever look at the newest point (or whether any
/// point exists at all). Neither needs the series, so only the newest point
/// plus a count are kept.
#[derive(Debug, Clone)]
pub struct ResidentSession {
    pub summary: SessionSummary,
    /// The most recent entry of `Session::rate_limits_history` by timestamp
    /// (not necessarily the last element — see [`ResidentSession::of`]),
    /// or `None` if the session has never reported a rate-limit snapshot.
    pub newest_rate_limit_point: Option<RateLimitSnapshotPoint>,
    /// How many points `Session::rate_limits_history` held when this
    /// resident projection was built. A count rather than a bool: it costs
    /// nothing extra over a bool and is strictly more informative for any
    /// future consumer (e.g. diagnostics reporting a distribution), so
    /// there is no reason to throw the information away.
    pub rate_limits_history_len: usize,
}

impl ResidentSession {
    pub fn of(session: &Session) -> Self {
        Self {
            summary: SessionSummary::of(session),
            // Matches the prior behavior of scanning every point for the
            // max timestamp (see `commands::newest_subscription_usage_by_harness`,
            // which used to do this across the whole `Vec` itself): points
            // are pushed in file-parse order, which is normally
            // chronological, but this does not assume that.
            newest_rate_limit_point: session
                .rate_limits_history
                .iter()
                .max_by_key(|point| point.timestamp)
                .cloned(),
            rate_limits_history_len: session.rate_limits_history.len(),
        }
    }
}

pub(crate) fn add_totals(dst: &mut TokenTotals, src: &TokenTotals) {
    *dst += src;
}

/// Groups history deltas by (model, tier), skipping events that had no model
/// attributed yet (their usage is folded into a model bucket by the parser's
/// reconciliation on a later event).
fn bucket_history<'a, I>(events: I) -> Vec<TierBucket>
where
    I: Iterator<Item = &'a TokenHistoryPoint>,
{
    let mut map: BTreeMap<(&str, Option<&str>), TokenTotals> = BTreeMap::new();
    for ev in events {
        let Some(model) = &ev.model else { continue };
        let entry = map
            .entry((model.as_str(), ev.service_tier.as_deref()))
            .or_default();
        add_totals(entry, &ev.delta);
    }
    map.into_iter()
        .map(|((model, service_tier), tokens)| TierBucket {
            model: model.to_owned(),
            service_tier: service_tier.map(str::to_owned),
            tokens,
        })
        .collect()
}

impl Session {
    /// Returns the persisted storage identity, deriving a compatible key for
    /// sessions serialized before this field existed.
    pub fn effective_storage_id(&self) -> String {
        if !self.storage_id.is_empty() {
            return self.storage_id.clone();
        }

        if self.harness == claude_code_provider_id() && self.source.as_deref() == Some("subagent") {
            if let Some(parent_session_id) = self.parent_thread_id.as_deref() {
                return storage_id_for_claude_subagent(parent_session_id, &self.id);
            }
        }

        storage_id_for_session(&self.harness, &self.id)
    }

    /// All-time (model, tier) usage buckets. Derived from history when
    /// present (which preserves per-event service tiers for fast-mode
    /// pricing); falls back to the per-model buckets for sessions without
    /// history events.
    pub fn tier_buckets(&self) -> Vec<TierBucket> {
        if self.tokens_history.is_empty() {
            let mut buckets: Vec<TierBucket> = self
                .tokens_by_model
                .iter()
                .map(|(model, tokens)| TierBucket {
                    model: model.clone(),
                    service_tier: None,
                    tokens: tokens.clone(),
                })
                .collect();
            buckets.sort_by(|a, b| a.model.cmp(&b.model));
            return buckets;
        }
        bucket_history(self.tokens_history.iter())
    }

    /// Usage restricted to history events inside [from, to] (inclusive;
    /// None = open bound). `tokens` sums every delta in range; `buckets`
    /// holds only model-attributed usage, for credit math.
    pub fn range_totals(
        &self,
        from: Option<DateTime<Utc>>,
        to: Option<DateTime<Utc>>,
    ) -> RangeTotals {
        self.range_totals_multi(&[(from, to)])
            .pop()
            .expect("one window in, one rollup out")
    }

    /// `range_totals` for several windows at once. Histories emitted by both
    /// harnesses are chronological, so binary partitioning narrows each range
    /// to its matching slice instead of testing every event against every
    /// window. The defensive unsorted path preserves correctness for unusual
    /// or historical fixtures.
    pub fn range_totals_multi(&self, ranges: &[RangeWindow]) -> Vec<RangeTotals> {
        let history_sorted = self
            .tokens_history
            .windows(2)
            .all(|pair| pair[0].timestamp <= pair[1].timestamp);
        let observations_sorted = self
            .tool_observations
            .windows(2)
            .all(|pair| pair[0].timestamp <= pair[1].timestamp);
        let mut results = Vec::with_capacity(ranges.len());

        for (from, to) in ranges {
            let mut tokens = TokenTotals::default();
            let mut buckets: Vec<TierBucket> = Vec::new();
            let mut add_event = |event: &TokenHistoryPoint| {
                add_totals(&mut tokens, &event.delta);
                let Some(model) = &event.model else { return };
                match buckets.iter_mut().find(|bucket| {
                    &bucket.model == model && bucket.service_tier == event.service_tier
                }) {
                    Some(bucket) => add_totals(&mut bucket.tokens, &event.delta),
                    None => buckets.push(TierBucket {
                        model: model.clone(),
                        service_tier: event.service_tier.clone(),
                        tokens: event.delta.clone(),
                    }),
                }
            };
            if history_sorted {
                let start = from.as_ref().map_or(0, |from| {
                    self.tokens_history.partition_point(|event| {
                        event.timestamp.timestamp_millis() < from.timestamp_millis()
                    })
                });
                let end = to.as_ref().map_or(self.tokens_history.len(), |to| {
                    self.tokens_history.partition_point(|event| {
                        event.timestamp.timestamp_millis() <= to.timestamp_millis()
                    })
                });
                for event in &self.tokens_history[start.min(end)..end] {
                    add_event(event);
                }
            } else {
                for event in &self.tokens_history {
                    if (from.is_none()
                        || from.as_ref().is_some_and(|start| {
                            event.timestamp.timestamp_millis() >= start.timestamp_millis()
                        }))
                        && (to.is_none()
                            || to.as_ref().is_some_and(|end| {
                                event.timestamp.timestamp_millis() <= end.timestamp_millis()
                            }))
                    {
                        add_event(event);
                    }
                }
            }

            let (tool_metrics, tool_metrics_by_model) = if observations_sorted {
                let start = from.as_ref().map_or(0, |from| {
                    self.tool_observations.partition_point(|item| {
                        item.timestamp.timestamp_millis() < from.timestamp_millis()
                    })
                });
                let end = to.as_ref().map_or(self.tool_observations.len(), |to| {
                    self.tool_observations.partition_point(|item| {
                        item.timestamp.timestamp_millis() <= to.timestamp_millis()
                    })
                });
                let selected = &self.tool_observations[start.min(end)..end];
                crate::telemetry::metrics_with_models(selected.iter())
            } else {
                crate::telemetry::metrics_with_models(self.tool_observations.iter().filter(
                    |item| {
                        (from.is_none()
                            || from.as_ref().is_some_and(|start| {
                                item.timestamp.timestamp_millis() >= start.timestamp_millis()
                            }))
                            && (to.is_none()
                                || to.as_ref().is_some_and(|end| {
                                    item.timestamp.timestamp_millis() <= end.timestamp_millis()
                                }))
                    },
                ))
            };
            let selected_findings: Vec<_> = self
                .optimization_findings
                .iter()
                .filter(|finding| match finding.timestamp {
                    Some(timestamp) => {
                        from.is_none_or(|from| {
                            timestamp.timestamp_millis() >= from.timestamp_millis()
                        }) && to
                            .is_none_or(|to| timestamp.timestamp_millis() <= to.timestamp_millis())
                    }
                    None => from.is_none() && to.is_none(),
                })
                .collect();
            let optimization_findings_count = selected_findings.len() as u64;
            let optimization_summary = OptimizationSummary::from_findings(selected_findings);

            buckets.sort_by(|a, b| {
                a.model
                    .cmp(&b.model)
                    .then_with(|| a.service_tier.cmp(&b.service_tier))
            });
            let tool_dimensions = if observations_sorted {
                let start = from.as_ref().map_or(0, |from| {
                    self.tool_observations.partition_point(|item| {
                        item.timestamp.timestamp_millis() < from.timestamp_millis()
                    })
                });
                let end = to.as_ref().map_or(self.tool_observations.len(), |to| {
                    self.tool_observations.partition_point(|item| {
                        item.timestamp.timestamp_millis() <= to.timestamp_millis()
                    })
                });
                crate::telemetry::dimension_totals(
                    self.tool_observations[start.min(end)..end].iter(),
                    &tokens,
                )
            } else {
                crate::telemetry::dimension_totals(
                    self.tool_observations.iter().filter(|item| {
                        (from.is_none()
                            || from.as_ref().is_some_and(|start| {
                                item.timestamp.timestamp_millis() >= start.timestamp_millis()
                            }))
                            && (to.is_none()
                                || to.as_ref().is_some_and(|end| {
                                    item.timestamp.timestamp_millis() <= end.timestamp_millis()
                                }))
                    }),
                    &tokens,
                )
            };
            results.push(RangeTotals {
                tokens,
                buckets,
                tool_metrics,
                tool_metrics_by_model,
                optimization_findings_count,
                optimization_summary,
                tool_dimensions,
            });
        }
        results
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn delta(n: u64) -> TokenTotals {
        TokenTotals {
            input_tokens: n,
            cached_input_tokens: 0,
            cache_creation_input_tokens: 0,
            output_tokens: n / 2,
            reasoning_output_tokens: 0,
            total_tokens: n + n / 2,
        }
    }

    fn point(ts: &str, model: Option<&str>, tier: Option<&str>, n: u64) -> TokenHistoryPoint {
        TokenHistoryPoint {
            timestamp: ts.parse().unwrap(),
            model: model.map(str::to_owned),
            service_tier: tier.map(str::to_owned),
            request_input_tokens: Some(n),
            total_tokens: 0,
            delta: delta(n),
        }
    }

    fn session_with_history(history: Vec<TokenHistoryPoint>) -> Session {
        Session {
            id: "s".into(),
            storage_id: storage_id_for_session(&codex_provider_id(), "s"),
            harness: codex_provider_id(),
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            source_availability: SourceAvailability::Present,
            archived: false,
            started_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            last_event_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            working_directory: None,
            originator: None,
            source: None,
            subagent_id_is_path_fallback: false,
            history_mode: None,
            memory_mode: None,
            cli_version: None,
            model_provider: None,
            model: None,
            service_tier: None,
            plan_type: None,
            credits_unlimited: None,
            credits_balance: None,
            context_window: None,
            latest_context_tokens: None,
            total_turns: 0,
            first_user_message: None,
            tokens_total: TokenTotals::default(),
            tokens_by_model: HashMap::new(),
            tokens_history: history,
            rate_limits_history: Vec::new(),
            turns: Vec::new(),
            tool_observations: Vec::new(),
            tool_metrics: ToolMetrics::default(),
            tool_metrics_by_model: BTreeMap::new(),
            category_totals: BTreeMap::new(),
            optimization_findings: Vec::new(),
            project_key: None,
            project_label: None,
            project_provenance: None,
        }
    }

    #[test]
    fn tier_buckets_group_by_model_and_tier_and_skip_unattributed() {
        let s = session_with_history(vec![
            point("2026-01-01T00:00:01Z", None, None, 10),
            point("2026-01-01T00:00:02Z", Some("m1"), None, 100),
            point("2026-01-01T00:00:03Z", Some("m1"), Some("fast"), 50),
            point("2026-01-01T00:00:04Z", Some("m1"), None, 100),
            point("2026-01-01T00:00:05Z", Some("m2"), None, 7),
        ]);
        let buckets = s.tier_buckets();
        assert_eq!(buckets.len(), 3);
        let m1_std = buckets
            .iter()
            .find(|b| b.model == "m1" && b.service_tier.is_none())
            .unwrap();
        assert_eq!(m1_std.tokens.input_tokens, 200);
        let m1_fast = buckets
            .iter()
            .find(|b| b.model == "m1" && b.service_tier.as_deref() == Some("fast"))
            .unwrap();
        assert_eq!(m1_fast.tokens.input_tokens, 50);
        let m2 = buckets.iter().find(|b| b.model == "m2").unwrap();
        assert_eq!(m2.tokens.input_tokens, 7);
    }

    #[test]
    fn tier_buckets_fall_back_to_model_buckets_without_history() {
        let mut s = session_with_history(vec![]);
        s.tokens_by_model.insert("m1".into(), delta(42));
        let buckets = s.tier_buckets();
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].model, "m1");
        assert_eq!(buckets[0].service_tier, None);
        assert_eq!(buckets[0].tokens.input_tokens, 42);
    }

    #[test]
    fn range_totals_respect_inclusive_bounds() {
        let s = session_with_history(vec![
            point("2026-01-01T00:00:01Z", Some("m1"), None, 1),
            point("2026-01-01T00:00:02Z", Some("m1"), None, 2),
            point("2026-01-01T00:00:03Z", Some("m1"), None, 4),
        ]);
        let rt = s.range_totals(
            Some("2026-01-01T00:00:02Z".parse().unwrap()),
            Some("2026-01-01T00:00:03Z".parse().unwrap()),
        );
        assert_eq!(rt.tokens.input_tokens, 6);
        assert_eq!(rt.buckets.len(), 1);
        assert_eq!(rt.buckets[0].tokens.input_tokens, 6);

        let all = s.range_totals(None, None);
        assert_eq!(all.tokens.input_tokens, 7);
    }

    #[test]
    fn range_totals_multi_matches_per_window_calls() {
        let s = session_with_history(vec![
            point("2026-01-01T00:00:01Z", Some("m1"), None, 1),
            point("2026-01-01T00:00:02Z", Some("m2"), Some("fast"), 2),
            point("2026-01-01T00:00:03Z", None, None, 4),
        ]);
        let windows = [
            (None, Some("2026-01-01T00:00:02Z".parse().unwrap())),
            (Some("2026-01-01T00:00:02Z".parse().unwrap()), None),
            (None, None),
            // Empty window.
            (Some("2027-01-01T00:00:00Z".parse().unwrap()), None),
        ];
        let multi = s.range_totals_multi(&windows);
        assert_eq!(multi.len(), windows.len());
        for (rt, (from, to)) in multi.iter().zip(windows) {
            let single = s.range_totals(from, to);
            assert_eq!(rt.tokens, single.tokens);
            assert_eq!(rt.buckets, single.buckets);
        }
        assert_eq!(multi[2].tokens.input_tokens, 7);
        assert_eq!(multi[2].buckets.len(), 2);
        assert_eq!(multi[3].tokens.total_tokens, 0);
    }

    #[test]
    fn range_tokens_include_unattributed_but_buckets_exclude_them() {
        let s = session_with_history(vec![
            point("2026-01-01T00:00:01Z", None, None, 10),
            point("2026-01-01T00:00:02Z", Some("m1"), None, 5),
        ]);
        let rt = s.range_totals(None, None);
        assert_eq!(rt.tokens.input_tokens, 15);
        assert_eq!(rt.buckets.len(), 1);
        assert_eq!(rt.buckets[0].tokens.input_tokens, 5);
    }

    #[test]
    fn range_totals_preserve_correctness_for_unsorted_history() {
        let s = session_with_history(vec![
            point("2026-01-01T00:00:03Z", Some("m1"), None, 4),
            point("2026-01-01T00:00:01Z", Some("m1"), None, 1),
            point("2026-01-01T00:00:02Z", Some("m1"), None, 2),
        ]);
        let rt = s.range_totals(
            Some("2026-01-01T00:00:01Z".parse().unwrap()),
            Some("2026-01-01T00:00:02Z".parse().unwrap()),
        );
        assert_eq!(rt.tokens.input_tokens, 3);
    }

    #[test]
    fn range_totals_count_only_findings_timestamped_in_range() {
        let mut s = session_with_history(vec![]);
        s.tool_observations = (1..=3)
            .map(|second| ToolObservation {
                call_id: second.to_string(),
                turn_id: Some("turn".into()),
                harness: codex_provider_id(),
                model: Some("m".into()),
                timestamp: format!("2026-01-01T00:00:0{second}Z").parse().unwrap(),
                kind: ToolKind::Read,
                name: "read".into(),
                providers: Vec::new(),
                effective_tools: vec!["read".into()],
                target: Some("read:synthetic".into()),
                resource_id: Some("synthetic".into()),
                origin: ToolOrigin::Core,
                shell_family: None,
                language: None,
                outcome: ToolOutcome::Success,
                duration_ms: None,
                output_bytes: 0,
            })
            .collect();
        crate::telemetry::refresh_session(&mut s);
        let partial = s.range_totals(
            Some("2026-01-01T00:00:01Z".parse().unwrap()),
            Some("2026-01-01T00:00:02Z".parse().unwrap()),
        );
        assert_eq!(partial.optimization_findings_count, 0);
        let all = s.range_totals(None, None);
        assert_eq!(all.optimization_findings_count, 1);
        assert_eq!(all.optimization_summary.findings, 1);
        assert_eq!(all.optimization_summary.likely_avoidable_calls, 2);
        assert_eq!(all.optimization_summary.by_rule["repeated-read"], 1);
    }

    #[test]
    #[ignore = "performance probe; run with --release --ignored --nocapture"]
    fn performance_range_rollup_100k_points_16_windows() {
        let base: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let history = (0..100_000)
            .map(|second| TokenHistoryPoint {
                timestamp: base + chrono::Duration::seconds(second),
                model: Some("m".into()),
                service_tier: None,
                request_input_tokens: Some(1),
                total_tokens: second as u64,
                delta: delta(1),
            })
            .collect();
        let session = session_with_history(history);
        let windows: Vec<RangeWindow> = (0..16)
            .map(|index| {
                let start = base + chrono::Duration::seconds(index * 6_000);
                (Some(start), Some(start + chrono::Duration::seconds(5_999)))
            })
            .collect();
        let started = std::time::Instant::now();
        let totals = session.range_totals_multi(&windows);
        eprintln!("100k points x 16 windows: {:?}", started.elapsed());
        assert_eq!(totals.len(), 16);
        assert!(totals
            .iter()
            .all(|total| total.tokens.input_tokens == 6_000));
    }

    #[test]
    fn summary_carries_metadata_and_buckets() {
        let mut s =
            session_with_history(vec![point("2026-01-01T00:00:02Z", Some("m1"), None, 100)]);
        s.thread_name = Some("t".into());
        let summary = SessionSummary::of(&s);
        assert_eq!(summary.id, "s");
        assert_eq!(summary.storage_id, "codex:thread:s");
        assert_eq!(summary.source_availability, SourceAvailability::Present);
        assert_eq!(summary.thread_name.as_deref(), Some("t"));
        assert_eq!(summary.buckets.len(), 1);
        assert_eq!(summary.buckets[0].tokens.input_tokens, 100);
    }

    #[test]
    fn storage_fields_default_when_deserializing_legacy_sessions() {
        let session = session_with_history(vec![]);
        let mut serialized = serde_json::to_value(&session).unwrap();
        let fields = serialized.as_object_mut().unwrap();
        fields.remove("storage_id");
        fields.remove("source_availability");

        let restored: Session = serde_json::from_value(serialized).unwrap();
        assert!(restored.storage_id.is_empty());
        assert_eq!(restored.source_availability, SourceAvailability::Present);
        assert_eq!(restored.effective_storage_id(), "codex:thread:s");

        let summary = SessionSummary::of(&session);
        let mut serialized = serde_json::to_value(&summary).unwrap();
        let fields = serialized.as_object_mut().unwrap();
        fields.remove("storage_id");
        fields.remove("source_availability");
        let restored: SessionSummary = serde_json::from_value(serialized).unwrap();
        assert!(restored.storage_id.is_empty());
        assert_eq!(restored.source_availability, SourceAvailability::Present);
    }

    // -- Resident heap-cost probe (issue #139 follow-up) -------------------
    //
    // These probes used to define their own `#[cfg(test)]`-only counting
    // `#[global_allocator]`, active only in the test binary since the
    // production binary was a separate compiled unit that never linked it.
    // The memory-instrumentation effort promoted that allocator to a single
    // production-safe one (`crate::memory::TrackingAllocator`, declared once
    // in `lib.rs`) so a real build can report allocator-tracked heap
    // alongside OS-reported RSS — a binary can only ever have one global
    // allocator, so the two could not coexist. These wrappers keep the exact
    // names every probe below (and `history_store::tests`'s streaming-
    // hydration and overlay probes) already called, delegating to that
    // shared allocator's `pub(crate)` raw accessors instead of a private
    // static. Unlike before, tracking is runtime-gated and off by default
    // (`crate::memory::configure_heap_tracking`); every probe that reads
    // these now enables it explicitly near its start.
    pub(crate) fn allocated_bytes() -> usize {
        crate::memory::heap_allocated_bytes_raw() as usize
    }

    /// High-water mark of [`allocated_bytes`] observed since heap tracking
    /// was enabled or the last [`reset_peak_to_current`] call, whichever is
    /// more recent.
    pub(crate) fn peak_bytes() -> usize {
        crate::memory::heap_peak_bytes_raw() as usize
    }

    /// Rebases the peak tracker to the current allocation level, so a
    /// subsequent [`peak_bytes`] read reflects only growth from this point
    /// forward — otherwise an earlier, unrelated region's high-water mark
    /// (or a prior test's, if allocations from two `#[test]` functions
    /// interleave) would still be visible.
    pub(crate) fn reset_peak_to_current() {
        crate::memory::reset_heap_peak_to_current();
    }

    /// Builds one session shaped like real agentic-loop usage: several
    /// turns, each carrying near-the-truncation-limit `user_message`/
    /// `last_agent_message` text (`TURN_MESSAGE_LIMIT` is 500 chars in
    /// `parser.rs`/`claude_parser.rs`/`gemini_parser.rs`), several
    /// `tokens_history` events, and several `tool_observations` — unlike
    /// this crate's existing synthetic-corpus probes
    /// (`history_store::tests::rich_session` and its
    /// `probe_hydration_and_post_scan_rehydrate_at_field_recording_scale`
    /// caller), which never populate `turns` at all and so understate a
    /// real session's size. `hot` sessions get more of everything, mirroring
    /// the hot/cold split those existing probes already use for a corpus
    /// with a minority of large sessions and a majority of small ones.
    fn realistic_session(id: &str, hot: bool) -> Session {
        let (turn_count, tokens_per_turn, tools_per_turn) =
            if hot { (25, 5, 6) } else { (6, 2, 2) };
        realistic_session_with_shape(id, turn_count, tokens_per_turn, tools_per_turn)
    }

    /// Generalized [`realistic_session`], parameterized on shape rather than
    /// a fixed hot/cold split, so other probes (`history_store::tests`'s
    /// streaming-hydration and thread-name-overlay probes) can dial the
    /// per-session turn/event volume up to field-recording scale — a real
    /// corpus averaged ~482 KB/session (2,151,487,813 bytes / 4,463
    /// sessions) — without duplicating this builder or perturbing
    /// `realistic_session`'s own fixed shape, which
    /// `probe_resident_summary_vs_full_session_heap_cost` is calibrated
    /// against. `pub(crate)` for that cross-module reuse.
    pub(crate) fn realistic_session_with_shape(
        id: &str,
        turn_count: usize,
        tokens_per_turn: usize,
        tools_per_turn: usize,
    ) -> Session {
        let base: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let user_message: String = "Please investigate why the incremental parser occasionally \
             drops the trailing partial record when a rollout file is truncated mid-write, and \
             fix the fast-skip path so it still advances last_event_at from the truncated line. "
            .chars()
            .cycle()
            .take(450)
            .collect();
        let agent_message: String = "I found the issue in apply_line_without_refresh: the fast \
             path returned before updating last_event_at when the JSON parse failed on a \
             partial trailing line. I added a guard that still extracts the timestamp first. "
            .chars()
            .cycle()
            .take(450)
            .collect();

        let mut turns = Vec::with_capacity(turn_count);
        let mut tokens_history = Vec::with_capacity(turn_count * tokens_per_turn);
        let mut tool_observations = Vec::with_capacity(turn_count * tools_per_turn);
        for turn_index in 0..turn_count {
            let turn_id = format!("turn-{id}-{turn_index}");
            let turn_start = base + chrono::Duration::minutes(turn_index as i64 * 5);
            for call in 0..tokens_per_turn {
                tokens_history.push(TokenHistoryPoint {
                    timestamp: turn_start + chrono::Duration::seconds(call as i64 * 10),
                    model: Some("gpt-5.1-codex".into()),
                    service_tier: Some("default".into()),
                    request_input_tokens: Some(1_200 + call as u64 * 50),
                    total_tokens: 0,
                    delta: delta(600 + call as u64 * 25),
                });
            }
            for call in 0..tools_per_turn {
                tool_observations.push(ToolObservation {
                    call_id: format!("call-{id}-{turn_index}-{call}"),
                    turn_id: Some(turn_id.clone()),
                    harness: codex_provider_id(),
                    model: Some("gpt-5.1-codex".into()),
                    timestamp: turn_start + chrono::Duration::seconds(call as i64 * 12),
                    kind: ToolKind::Read,
                    name: "shell".into(),
                    providers: vec!["core".into()],
                    effective_tools: vec!["bash".into()],
                    target: Some(format!("target-hash-{id}-{turn_index}-{call}")),
                    resource_id: Some(format!("resource-{id}-{turn_index}")),
                    origin: ToolOrigin::Core,
                    shell_family: Some("bash".into()),
                    language: None,
                    outcome: ToolOutcome::Success,
                    duration_ms: Some(420),
                    output_bytes: 2_048,
                });
            }
            turns.push(TurnInfo {
                turn_id,
                index: turn_index as u32 + 1,
                model: Some("gpt-5.1-codex".into()),
                reasoning_effort: Some("medium".into()),
                collaboration_mode: None,
                service_tier: Some("default".into()),
                status: TurnStatus::Completed,
                abort_reason: None,
                started_at: Some(turn_start),
                completed_at: Some(turn_start + chrono::Duration::minutes(4)),
                duration_ms: Some(240_000),
                time_to_first_token_ms: Some(850),
                user_message: Some(user_message.clone()),
                last_agent_message: Some(agent_message.clone()),
                tokens: delta(1_800),
                tool_metrics: ToolMetrics {
                    calls: tools_per_turn as u64,
                    reads: tools_per_turn as u64,
                    successes: tools_per_turn as u64,
                    duration_ms: 420 * tools_per_turn as u64,
                    output_bytes: 2_048 * tools_per_turn as u64,
                    ..Default::default()
                },
                classification: TurnClassification {
                    version: 1,
                    category: TaskCategory::Debugging,
                    confidence: 0.82,
                    signals: vec!["error-message".into(), "stack-trace".into()],
                },
            });
        }

        Session {
            id: id.into(),
            storage_id: storage_id_for_session(&codex_provider_id(), id),
            harness: codex_provider_id(),
            thread_name: Some(format!("Investigate {id}")),
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: format!("{id}.jsonl"),
            source_availability: SourceAvailability::Present,
            archived: false,
            started_at: base,
            last_event_at: base + chrono::Duration::minutes(turn_count as i64 * 5),
            working_directory: Some("D:/projects/agent-odometer".into()),
            originator: Some("cli".into()),
            source: None,
            subagent_id_is_path_fallback: false,
            history_mode: Some("default".into()),
            memory_mode: Some("default".into()),
            cli_version: Some("0.45.0".into()),
            model_provider: Some("openai".into()),
            model: Some("gpt-5.1-codex".into()),
            service_tier: Some("default".into()),
            plan_type: Some("plus".into()),
            credits_unlimited: Some(false),
            credits_balance: Some(128.5),
            context_window: Some(272_000),
            latest_context_tokens: Some(48_000),
            total_turns: turn_count as u32,
            first_user_message: Some(user_message.chars().take(200).collect()),
            tokens_total: delta(1_800 * turn_count as u64),
            tokens_by_model: HashMap::from([(
                "gpt-5.1-codex".to_string(),
                delta(1_800 * turn_count as u64),
            )]),
            tokens_history,
            rate_limits_history: vec![RateLimitSnapshotPoint {
                timestamp: base,
                turn_id: None,
                limit_id: Some("primary".into()),
                primary: Some(RateLimitWindow {
                    used_percent: 42.0,
                    window_minutes: Some(300),
                    resets_at: Some(base + chrono::Duration::hours(5)),
                }),
                secondary: None,
                run_started_at: None,
                observation_count: 1,
            }],
            turns,
            tool_observations,
            tool_metrics: ToolMetrics {
                calls: (turn_count * tools_per_turn) as u64,
                ..Default::default()
            },
            tool_metrics_by_model: BTreeMap::new(),
            category_totals: BTreeMap::from([(
                TaskCategory::Debugging,
                CategoryMetric {
                    turns: turn_count as u64,
                    tokens: delta(1_800 * turn_count as u64),
                    tool_calls: (turn_count * tools_per_turn) as u64,
                    buckets: Vec::new(),
                },
            )]),
            optimization_findings: Vec::new(),
            project_key: Some("repo:agent-odometer".into()),
            project_label: Some("agent-odometer".into()),
            project_provenance: Some(crate::project_identity::ProjectProvenance::RepositoryRoot),
        }
    }

    #[test]
    #[ignore = "issue #139 heap-cost probe; run with `cargo test --release --lib \
                probe_resident_summary_vs_full_session_heap_cost -- --ignored --nocapture` \
                (filtering to this exact test name keeps the process-wide counting allocator \
                from also seeing another concurrently-running test's allocations)"]
    fn probe_resident_summary_vs_full_session_heap_cost() {
        const SESSIONS: usize = 4_400;
        const HOT_SESSIONS: usize = 440;

        crate::memory::configure_heap_tracking(true);

        // Measures two disjoint deltas in sequence: building `sessions`
        // (what `AppState.sessions` used to hold, in full) first, then
        // building `residents` from it (what `AppState.sessions` holds
        // now). Building `residents` from `&sessions` never touches
        // `sessions`' own allocations (it clones out into fresh `String`s/
        // `Vec`s on the summary side), so the second delta is exactly the
        // resident type's own additional cost — not contaminated by the
        // first.
        let before_full = allocated_bytes();
        let sessions: Vec<Session> = (0..SESSIONS)
            .map(|i| realistic_session(&format!("probe-{i}"), i < HOT_SESSIONS))
            .collect();
        let after_full = allocated_bytes();
        let full_bytes = after_full - before_full;

        let residents: Vec<ResidentSession> = sessions.iter().map(ResidentSession::of).collect();
        let after_resident = allocated_bytes();
        let resident_bytes = after_resident - after_full;

        let full_per_session = full_bytes as f64 / SESSIONS as f64;
        let resident_per_session = resident_bytes as f64 / SESSIONS as f64;
        eprintln!(
            "heap cost at {SESSIONS} sessions ({HOT_SESSIONS} hot / {} cold):\n\
             \x20 Session:         {full_bytes} bytes total, {full_per_session:.0} bytes/session\n\
             \x20 ResidentSession: {resident_bytes} bytes total, {resident_per_session:.0} bytes/session\n\
             \x20 reduction: {:.1}x, {:.1}% smaller, {:.0} bytes/session saved",
            SESSIONS - HOT_SESSIONS,
            full_bytes as f64 / resident_bytes.max(1) as f64,
            100.0 * (1.0 - resident_bytes as f64 / full_bytes.max(1) as f64),
            full_per_session - resident_per_session,
        );

        assert!(
            resident_bytes < full_bytes / 10,
            "expected the resident summary to be well under 10% of the full session's heap \
             cost; got {resident_bytes} vs {full_bytes} bytes"
        );

        drop(sessions);
        drop(residents);
    }

    // -- Issue #153: RateLimitSnapshotPoint collapsing -----------------------

    fn rl_window(used_percent: f64) -> RateLimitWindow {
        RateLimitWindow {
            used_percent,
            window_minutes: Some(300),
            resets_at: None,
        }
    }

    #[test]
    fn record_collapses_consecutive_field_identical_observations() {
        let base: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let mut history = Vec::new();
        for i in 0..3 {
            RateLimitSnapshotPoint::record(
                &mut history,
                base + chrono::Duration::minutes(i),
                Some("t1".into()),
                Some("codex".into()),
                Some(rl_window(10.0)),
                None,
            );
        }
        assert_eq!(
            history.len(),
            1,
            "3 identical observations collapse to 1 run"
        );
        let run = &history[0];
        assert_eq!(run.observation_count, 3);
        assert_eq!(run.run_started_at(), base);
        assert_eq!(run.timestamp, base + chrono::Duration::minutes(2));

        // A differing field (turn_id here) must start a new run even though
        // primary/secondary are unchanged.
        RateLimitSnapshotPoint::record(
            &mut history,
            base + chrono::Duration::minutes(3),
            Some("t2".into()),
            Some("codex".into()),
            Some(rl_window(10.0)),
            None,
        );
        assert_eq!(history.len(), 2);
        assert_eq!(history[1].observation_count, 1);
        assert_eq!(history[1].run_started_at(), history[1].timestamp);
    }

    #[test]
    fn pre_153_json_without_the_new_fields_deserializes_as_a_run_of_one() {
        // Exact `session_json` shape written before issue #153: no
        // `run_started_at`/`observation_count` keys at all. Old ledger rows
        // must still deserialize correctly without a migration or rescan.
        let json = r#"{
            "timestamp": "2026-01-01T00:00:00Z",
            "turn_id": null,
            "limit_id": null,
            "primary": {"used_percent": 40.0, "window_minutes": 300, "resets_at": null},
            "secondary": null
        }"#;
        let point: RateLimitSnapshotPoint = serde_json::from_str(json).unwrap();
        assert_eq!(point.observation_count, 1);
        assert_eq!(point.run_started_at, None);
        assert_eq!(
            point.run_started_at(),
            point.timestamp,
            "a pre-#153 point is, semantically, a run of exactly one observation"
        );
    }

    #[test]
    fn collapse_matches_incremental_record_over_a_raw_sequence() {
        let base: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let raw: Vec<RateLimitSnapshotPoint> = [10.0, 10.0, 12.0, 12.0, 12.0, 15.0]
            .into_iter()
            .enumerate()
            .map(|(i, used_percent)| RateLimitSnapshotPoint {
                timestamp: base + chrono::Duration::minutes(i as i64),
                turn_id: None,
                limit_id: None,
                primary: Some(rl_window(used_percent)),
                secondary: None,
                run_started_at: None,
                observation_count: 1,
            })
            .collect();

        let collapsed = RateLimitSnapshotPoint::collapse(&raw);
        assert_eq!(collapsed.len(), 3);
        assert_eq!(
            collapsed.iter().map(|p| p.observation_count).sum::<u32>(),
            raw.len() as u32,
            "every raw observation must be accounted for in the collapsed counts"
        );
        assert_eq!(collapsed[0].observation_count, 2);
        assert_eq!(collapsed[1].observation_count, 3);
        assert_eq!(collapsed[2].observation_count, 1);
    }
}
