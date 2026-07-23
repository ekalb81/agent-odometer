use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

/// Which agent harness produced a session's transcript. Serialized as
/// snake_case strings; defaults to Codex so previously-serialized sessions
/// and frontends without the field keep working.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    #[default]
    Codex,
    ClaudeCode,
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
    let mut map: BTreeMap<(String, Option<String>), TokenTotals> = BTreeMap::new();
    for ev in events {
        let Some(model) = &ev.model else { continue };
        let entry = map
            .entry((model.clone(), ev.service_tier.clone()))
            .or_default();
        add_totals(entry, &ev.delta);
    }
    map.into_iter()
        .map(|((model, service_tier), tokens)| TierBucket {
            model,
            service_tier,
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

    /// `range_totals` for several windows at once, walking the (potentially
    /// large) history a single time instead of once per window. Buckets
    /// accumulate through a linear probe of a short Vec — sessions touch only
    /// a few (model, tier) pairs, and this avoids the per-event key clones a
    /// map entry API would cost.
    pub fn range_totals_multi(&self, ranges: &[RangeWindow]) -> Vec<RangeTotals> {
        let mut tokens = vec![TokenTotals::default(); ranges.len()];
        let mut buckets: Vec<Vec<TierBucket>> = vec![Vec::new(); ranges.len()];
        for ev in &self.tokens_history {
            for (i, (from, to)) in ranges.iter().enumerate() {
                // map_or rather than is_none_or: the crate's MSRV (1.77) predates it.
                let in_range = from.map_or(true, |f| ev.timestamp >= f)
                    && to.map_or(true, |t| ev.timestamp <= t);
                if !in_range {
                    continue;
                }
                add_totals(&mut tokens[i], &ev.delta);
                let Some(model) = &ev.model else { continue };
                match buckets[i]
                    .iter_mut()
                    .find(|b| &b.model == model && b.service_tier == ev.service_tier)
                {
                    Some(b) => add_totals(&mut b.tokens, &ev.delta),
                    None => buckets[i].push(TierBucket {
                        model: model.clone(),
                        service_tier: ev.service_tier.clone(),
                        tokens: ev.delta.clone(),
                    }),
                }
            }
        }
        tokens
            .into_iter()
            .zip(buckets)
            .map(|(tokens, mut buckets)| {
                // Same (model, tier) ordering bucket_history's BTreeMap produced.
                buckets.sort_by(|a, b| {
                    a.model
                        .cmp(&b.model)
                        .then_with(|| a.service_tier.cmp(&b.service_tier))
                });
                RangeTotals { tokens, buckets }
            })
            .collect()
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
