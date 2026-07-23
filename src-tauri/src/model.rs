use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Which agent harness produced a session's transcript. Serialized as
/// snake_case strings; defaults to Codex so previously-serialized sessions
/// and frontends without the field keep working.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    #[default]
    Codex,
    ClaudeCode,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ToolObservation {
    pub call_id: String,
    pub turn_id: Option<String>,
    pub harness: Harness,
    pub model: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub kind: ToolKind,
    pub name: String,
    /// Stable hashed identity; raw arguments and paths are never retained.
    pub target: Option<String>,
    pub outcome: ToolOutcome,
    pub duration_ms: Option<u64>,
    pub output_bytes: u64,
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
    pub turn_id: Option<String>,
    pub model: Option<String>,
    #[serde(default)]
    pub timestamp: Option<DateTime<Utc>>,
    pub evidence: String,
    pub remediation: String,
}

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
    #[serde(default)]
    pub tool_metrics: ToolMetrics,
    #[serde(default)]
    pub classification: TurnClassification,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    #[serde(default)]
    pub harness: Harness,
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
}

/// Lightweight wire form of a Session for the list view and live update
/// events. Excludes `turns` and `tokens_history`, which dominate payload
/// size (a large session serializes to ~2 MB; its summary to ~1 KB). The
/// full Session is fetched on demand via `get_session_details`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: String,
    pub harness: Harness,
    pub thread_name: Option<String>,
    pub forked_from_id: Option<String>,
    pub parent_thread_id: Option<String>,
    pub agent_path: Option<String>,
    pub agent_nickname: Option<String>,
    pub file_path: String,
    pub archived: bool,
    pub started_at: DateTime<Utc>,
    pub last_event_at: DateTime<Utc>,
    pub working_directory: Option<String>,
    pub originator: Option<String>,
    pub source: Option<String>,
    pub cli_version: Option<String>,
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
}

impl SessionSummary {
    pub fn of(s: &Session) -> Self {
        Self {
            id: s.id.clone(),
            harness: s.harness,
            thread_name: s.thread_name.clone(),
            forked_from_id: s.forked_from_id.clone(),
            parent_thread_id: s.parent_thread_id.clone(),
            agent_path: s.agent_path.clone(),
            agent_nickname: s.agent_nickname.clone(),
            file_path: s.file_path.clone(),
            archived: s.archived,
            started_at: s.started_at,
            last_event_at: s.last_event_at,
            working_directory: s.working_directory.clone(),
            originator: s.originator.clone(),
            source: s.source.clone(),
            cli_version: s.cli_version.clone(),
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
        }
    }
}

fn add_totals(dst: &mut TokenTotals, src: &TokenTotals) {
    dst.input_tokens += src.input_tokens;
    dst.cached_input_tokens += src.cached_input_tokens;
    dst.output_tokens += src.output_tokens;
    dst.reasoning_output_tokens += src.reasoning_output_tokens;
    dst.total_tokens += src.total_tokens;
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
                    self.tokens_history
                        .partition_point(|event| event.timestamp < *from)
                });
                let end = to.as_ref().map_or(self.tokens_history.len(), |to| {
                    self.tokens_history
                        .partition_point(|event| event.timestamp <= *to)
                });
                for event in &self.tokens_history[start.min(end)..end] {
                    add_event(event);
                }
            } else {
                for event in &self.tokens_history {
                    if (from.is_none()
                        || from.as_ref().is_some_and(|start| event.timestamp >= *start))
                        && (to.is_none() || to.as_ref().is_some_and(|end| event.timestamp <= *end))
                    {
                        add_event(event);
                    }
                }
            }

            let (tool_metrics, tool_metrics_by_model) = if observations_sorted {
                let start = from.as_ref().map_or(0, |from| {
                    self.tool_observations
                        .partition_point(|item| item.timestamp < *from)
                });
                let end = to.as_ref().map_or(self.tool_observations.len(), |to| {
                    self.tool_observations
                        .partition_point(|item| item.timestamp <= *to)
                });
                let selected = &self.tool_observations[start.min(end)..end];
                crate::telemetry::metrics_with_models(selected.iter())
            } else {
                crate::telemetry::metrics_with_models(self.tool_observations.iter().filter(
                    |item| {
                        (from.is_none()
                            || from.as_ref().is_some_and(|start| item.timestamp >= *start))
                            && (to.is_none()
                                || to.as_ref().is_some_and(|end| item.timestamp <= *end))
                    },
                ))
            };
            let optimization_findings_count = self
                .optimization_findings
                .iter()
                .filter(|finding| match finding.timestamp {
                    Some(timestamp) => {
                        from.is_none_or(|from| timestamp >= from)
                            && to.is_none_or(|to| timestamp <= to)
                    }
                    None => from.is_none() && to.is_none(),
                })
                .count() as u64;

            buckets.sort_by(|a, b| {
                a.model
                    .cmp(&b.model)
                    .then_with(|| a.service_tier.cmp(&b.service_tier))
            });
            results.push(RangeTotals {
                tokens,
                buckets,
                tool_metrics,
                tool_metrics_by_model,
                optimization_findings_count,
            });
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delta(n: u64) -> TokenTotals {
        TokenTotals {
            input_tokens: n,
            cached_input_tokens: 0,
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
            total_tokens: 0,
            delta: delta(n),
        }
    }

    fn session_with_history(history: Vec<TokenHistoryPoint>) -> Session {
        Session {
            id: "s".into(),
            harness: Harness::Codex,
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            archived: false,
            started_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            last_event_at: "2026-01-01T00:00:00Z".parse().unwrap(),
            working_directory: None,
            originator: None,
            source: None,
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
            turns: Vec::new(),
            tool_observations: Vec::new(),
            tool_metrics: ToolMetrics::default(),
            tool_metrics_by_model: BTreeMap::new(),
            category_totals: BTreeMap::new(),
            optimization_findings: Vec::new(),
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
                harness: Harness::Codex,
                model: Some("m".into()),
                timestamp: format!("2026-01-01T00:00:0{second}Z").parse().unwrap(),
                kind: ToolKind::Read,
                name: "read".into(),
                target: Some("read:synthetic".into()),
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
        assert_eq!(summary.thread_name.as_deref(), Some("t"));
        assert_eq!(summary.buckets.len(), 1);
        assert_eq!(summary.buckets[0].tokens.input_tokens, 100);
    }
}
