use crate::model::{
    Harness, Session, TaskCategory, TierBucket, TokenTotals, ToolMetrics, TurnInfo, TurnStatus,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum ToolImpactTargetKind {
    Provider,
    Tool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolImpactTarget {
    pub kind: ToolImpactTargetKind,
    pub key: String,
    pub label: String,
    pub turn_count: u64,
    pub call_count: u64,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ToolImpactCohort {
    pub turn_count: u64,
    pub session_count: u64,
    pub completed_turn_count: u64,
    pub duration_sample_count: u64,
    pub total_duration_ms: u64,
    pub ttft_sample_count: u64,
    pub total_ttft_ms: u64,
    pub tokens: TokenTotals,
    pub buckets: Vec<TierBucket>,
    pub tool_metrics: ToolMetrics,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolImpactResult {
    pub target_kind: ToolImpactTargetKind,
    pub target_key: String,
    pub observed: ToolImpactCohort,
    pub baseline: ToolImpactCohort,
    pub matched_observed: ToolImpactCohort,
    pub matched_baseline: ToolImpactCohort,
    pub matched_pairs: u64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MatchKey {
    harness: Harness,
    model: String,
    category: TaskCategory,
}

#[derive(Clone)]
struct TurnSample {
    session_id: String,
    timestamp: DateTime<Utc>,
    key: MatchKey,
    service_tier: Option<String>,
    completed: bool,
    duration_ms: Option<u64>,
    ttft_ms: Option<u64>,
    tokens: TokenTotals,
    tools: ToolMetrics,
    target_used: bool,
}

#[derive(Default)]
struct CohortAccumulator {
    cohort: ToolImpactCohort,
    session_ids: BTreeSet<String>,
    buckets: BTreeMap<(String, Option<String>), TokenTotals>,
}

impl CohortAccumulator {
    fn push(&mut self, sample: &TurnSample) {
        self.cohort.turn_count += 1;
        self.session_ids.insert(sample.session_id.clone());
        if sample.completed {
            self.cohort.completed_turn_count += 1;
        }
        if let Some(duration) = sample.duration_ms {
            self.cohort.duration_sample_count += 1;
            self.cohort.total_duration_ms += duration;
        }
        if let Some(ttft) = sample.ttft_ms {
            self.cohort.ttft_sample_count += 1;
            self.cohort.total_ttft_ms += ttft;
        }
        add_tokens(&mut self.cohort.tokens, &sample.tokens);
        self.cohort.tool_metrics.add_assign(&sample.tools);
        if sample.tokens.total_tokens > 0 && sample.key.model != "unknown" {
            add_tokens(
                self.buckets
                    .entry((sample.key.model.clone(), sample.service_tier.clone()))
                    .or_default(),
                &sample.tokens,
            );
        }
    }

    fn finish(mut self) -> ToolImpactCohort {
        self.cohort.session_count = self.session_ids.len() as u64;
        self.cohort.buckets = self
            .buckets
            .into_iter()
            .map(|((model, service_tier), tokens)| TierBucket {
                model,
                service_tier,
                tokens,
            })
            .collect();
        self.cohort
    }
}

fn add_tokens(target: &mut TokenTotals, value: &TokenTotals) {
    *target += value;
}

fn turn_overlaps(
    session: &Session,
    turn: &TurnInfo,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> bool {
    let start = turn.started_at.unwrap_or(session.started_at);
    let end = turn
        .completed_at
        .unwrap_or(session.last_event_at)
        .max(start);
    from.is_none_or(|from| end >= from) && to.is_none_or(|to| start <= to)
}

fn turn_has_comparison_data(turn: &TurnInfo) -> bool {
    turn.tokens.total_tokens > 0 || turn.tool_metrics.calls > 0 || turn.duration_ms.is_some()
}

fn observation_matches_target(
    observation: &crate::model::ToolObservation,
    kind: ToolImpactTargetKind,
    key: &str,
) -> bool {
    match kind {
        ToolImpactTargetKind::Provider => {
            observation
                .providers
                .iter()
                .any(|value| value.eq_ignore_ascii_case(key))
                || crate::telemetry::tool_providers(&observation.name, &serde_json::Value::Null)
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(key))
        }
        ToolImpactTargetKind::Tool => {
            let tools = if observation.effective_tools.is_empty() {
                std::slice::from_ref(&observation.name)
            } else {
                observation.effective_tools.as_slice()
            };
            tools.iter().any(|value| value.eq_ignore_ascii_case(key))
        }
    }
}

fn samples_for_session(
    session: &Session,
    kind: ToolImpactTargetKind,
    key: &str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Vec<TurnSample> {
    let storage_id = session.effective_storage_id();
    let observed_turns: BTreeSet<&str> = session
        .tool_observations
        .iter()
        .filter(|observation| observation_matches_target(observation, kind, key))
        .filter_map(|observation| observation.turn_id.as_deref())
        .collect();

    session
        .turns
        .iter()
        .filter(|turn| turn_overlaps(session, turn, from, to))
        .filter(|turn| turn_has_comparison_data(turn))
        .map(|turn| TurnSample {
            session_id: storage_id.clone(),
            timestamp: turn.started_at.unwrap_or(session.started_at),
            key: MatchKey {
                harness: session.harness.clone(),
                model: turn
                    .model
                    .as_deref()
                    .or(session.model.as_deref())
                    .unwrap_or("unknown")
                    .to_owned(),
                category: turn.classification.category,
            },
            service_tier: turn
                .service_tier
                .clone()
                .or_else(|| session.service_tier.clone()),
            completed: turn.status == TurnStatus::Completed,
            duration_ms: turn.duration_ms,
            ttft_ms: turn.time_to_first_token_ms,
            tokens: turn.tokens.clone(),
            tools: turn.tool_metrics.clone(),
            target_used: observed_turns.contains(turn.turn_id.as_str()),
        })
        .collect()
}

fn aggregate<'a>(samples: impl IntoIterator<Item = &'a TurnSample>) -> ToolImpactCohort {
    let mut accumulator = CohortAccumulator::default();
    for sample in samples {
        accumulator.push(sample);
    }
    accumulator.finish()
}

fn take_closest(pool: &mut BTreeMap<i64, Vec<TurnSample>>, target: i64) -> Option<TurnSample> {
    let before = pool.range(..=target).next_back().map(|(key, _)| *key);
    let after = pool.range(target..).next().map(|(key, _)| *key);
    let key = match (before, after) {
        (Some(before), Some(after)) => {
            if target.saturating_sub(before) <= after.saturating_sub(target) {
                before
            } else {
                after
            }
        }
        (Some(before), None) => before,
        (None, Some(after)) => after,
        (None, None) => return None,
    };
    let samples = pool.get_mut(&key)?;
    let sample = samples.pop();
    if samples.is_empty() {
        pool.remove(&key);
    }
    sample
}

pub fn list_targets<S: Borrow<Session>>(
    sessions: &[S],
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Vec<ToolImpactTarget> {
    let mut counts: BTreeMap<(ToolImpactTargetKind, String), TargetCount> = BTreeMap::new();
    for item in sessions {
        let session = item.borrow();
        let storage_id = session.effective_storage_id();
        let eligible_turns: BTreeSet<&str> = session
            .turns
            .iter()
            .filter(|turn| turn_overlaps(session, turn, from, to))
            .filter(|turn| turn_has_comparison_data(turn))
            .map(|turn| turn.turn_id.as_str())
            .collect();

        for observation in &session.tool_observations {
            let Some(turn_id) = observation.turn_id.as_deref() else {
                continue;
            };
            if !eligible_turns.contains(turn_id) {
                continue;
            }
            let turn_identity = (storage_id.clone(), turn_id.to_owned());
            let mut observation_targets = BTreeSet::new();
            for provider in &observation.providers {
                observation_targets.insert((
                    ToolImpactTargetKind::Provider,
                    provider.to_ascii_lowercase(),
                ));
            }
            if observation.providers.is_empty() {
                for provider in
                    crate::telemetry::tool_providers(&observation.name, &serde_json::Value::Null)
                {
                    observation_targets.insert((ToolImpactTargetKind::Provider, provider));
                }
            }
            let tools = if observation.effective_tools.is_empty() {
                std::slice::from_ref(&observation.name)
            } else {
                observation.effective_tools.as_slice()
            };
            for tool in tools {
                observation_targets.insert((ToolImpactTargetKind::Tool, tool.to_ascii_lowercase()));
            }
            for target in observation_targets {
                let count = counts.entry(target).or_default();
                count.turns.insert(turn_identity.clone());
                count.calls += 1;
            }
        }
    }

    let mut targets: Vec<_> = counts
        .into_iter()
        .map(|((kind, key), count)| ToolImpactTarget {
            kind,
            label: key.clone(),
            key,
            turn_count: count.turns.len() as u64,
            call_count: count.calls,
        })
        .collect();
    targets.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| right.turn_count.cmp(&left.turn_count))
            .then_with(|| left.label.cmp(&right.label))
    });
    targets
}

#[derive(Default)]
struct TargetCount {
    turns: BTreeSet<(String, String)>,
    calls: u64,
}

pub fn compare<S: Borrow<Session>>(
    sessions: &[S],
    target_kind: ToolImpactTargetKind,
    target_key: &str,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> ToolImpactResult {
    let target_key = target_key.trim().to_ascii_lowercase();
    let samples: Vec<_> = sessions
        .iter()
        .flat_map(|session| {
            samples_for_session(session.borrow(), target_kind, &target_key, from, to)
        })
        .collect();
    let observed = aggregate(samples.iter().filter(|sample| sample.target_used));
    let baseline = aggregate(samples.iter().filter(|sample| !sample.target_used));

    let mut pools: BTreeMap<MatchKey, BTreeMap<i64, Vec<TurnSample>>> = BTreeMap::new();
    for sample in samples.iter().filter(|sample| !sample.target_used) {
        pools
            .entry(sample.key.clone())
            .or_default()
            .entry(sample.timestamp.timestamp_millis())
            .or_default()
            .push(sample.clone());
    }
    let mut observed_samples: Vec<_> = samples
        .iter()
        .filter(|sample| sample.target_used)
        .cloned()
        .collect();
    observed_samples.sort_by_key(|sample| sample.timestamp);
    let mut matched_observed = Vec::new();
    let mut matched_baseline = Vec::new();
    for sample in observed_samples {
        let Some(pool) = pools.get_mut(&sample.key) else {
            continue;
        };
        if let Some(candidate) = take_closest(pool, sample.timestamp.timestamp_millis()) {
            matched_observed.push(sample);
            matched_baseline.push(candidate);
        }
    }

    let target_label = match target_kind {
        ToolImpactTargetKind::Provider => "provider",
        ToolImpactTargetKind::Tool => "tool",
    };
    let mut warnings = vec![format!(
        "Transcripts prove observed {target_label} use, not whether it was installed or available."
    )];
    if observed.turn_count == 0 {
        warnings.push(format!(
            "No turns using the selected {target_label} were found in this view."
        ));
    }
    if baseline.turn_count == 0 {
        warnings.push(format!(
            "No turns without the selected {target_label} were found for a baseline."
        ));
    }
    if !matched_observed.is_empty() && matched_observed.len() < 3 {
        warnings.push(
            "Fewer than three same-harness, same-model, same-category pairs were available."
                .to_owned(),
        );
    }
    if observed.duration_sample_count < observed.turn_count
        || baseline.duration_sample_count < baseline.turn_count
    {
        warnings.push("Elapsed-time averages exclude turns without duration data.".to_owned());
    }

    ToolImpactResult {
        target_kind,
        target_key,
        observed,
        baseline,
        matched_observed: aggregate(matched_observed.iter()),
        matched_baseline: aggregate(matched_baseline.iter()),
        matched_pairs: matched_observed.len() as u64,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ToolKind, ToolObservation, ToolOutcome, TurnClassification};
    use crate::provider::{claude_code_provider_id, codex_provider_id};
    use std::collections::HashMap;

    fn totals(value: u64) -> TokenTotals {
        TokenTotals {
            input_tokens: value,
            output_tokens: value / 5,
            total_tokens: value + value / 5,
            ..Default::default()
        }
    }

    fn session(id: &str, alpha_provider: bool, tokens: u64, minute: u32) -> Session {
        let timestamp: DateTime<Utc> = format!("2026-07-24T12:{minute:02}:00Z").parse().unwrap();
        let turn_id = format!("turn-{id}");
        let tool_metrics = ToolMetrics {
            calls: 1,
            searches: 1,
            successes: 1,
            duration_ms: 500,
            ..Default::default()
        };
        Session {
            id: id.into(),
            storage_id: format!("codex:thread:{id}"),
            harness: codex_provider_id(),
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: format!("{id}.jsonl"),
            source_availability: Default::default(),
            archived: false,
            started_at: timestamp,
            last_event_at: timestamp + chrono::Duration::seconds(30),
            working_directory: None,
            originator: None,
            source: None,
            subagent_id_is_path_fallback: false,
            history_mode: None,
            memory_mode: None,
            cli_version: None,
            model_provider: None,
            model: Some("gpt-5.5".into()),
            service_tier: None,
            plan_type: None,
            credits_unlimited: None,
            credits_balance: None,
            context_window: None,
            latest_context_tokens: None,
            total_turns: 1,
            first_user_message: None,
            tokens_total: totals(tokens),
            tokens_by_model: HashMap::new(),
            tokens_history: Vec::new(),
            rate_limits_history: Vec::new(),
            turns: vec![TurnInfo {
                turn_id: turn_id.clone(),
                index: 1,
                model: Some("gpt-5.5".into()),
                status: TurnStatus::Completed,
                started_at: Some(timestamp),
                completed_at: Some(timestamp + chrono::Duration::seconds(30)),
                duration_ms: Some(30_000),
                tokens: totals(tokens),
                tool_metrics: tool_metrics.clone(),
                classification: TurnClassification {
                    category: TaskCategory::Exploration,
                    ..Default::default()
                },
                ..Default::default()
            }],
            tool_observations: vec![ToolObservation {
                call_id: format!("call-{id}"),
                turn_id: Some(turn_id),
                harness: codex_provider_id(),
                model: Some("gpt-5.5".into()),
                timestamp,
                kind: ToolKind::Search,
                name: if alpha_provider {
                    "mcp__alpha__code_search".into()
                } else {
                    "search".into()
                },
                providers: if alpha_provider {
                    vec!["alpha".into()]
                } else {
                    Vec::new()
                },
                effective_tools: vec![if alpha_provider {
                    "mcp__alpha__code_search".into()
                } else {
                    "search".into()
                }],
                target: None,
                resource_id: None,
                outcome: ToolOutcome::Success,
                duration_ms: Some(500),
                output_bytes: 10,
            }],
            tool_metrics,
            tool_metrics_by_model: BTreeMap::new(),
            category_totals: BTreeMap::new(),
            optimization_findings: Vec::new(),
        }
    }

    #[test]
    fn compares_and_matches_provider_assisted_turns() {
        let sessions = vec![
            session("alpha", true, 800, 10),
            session("baseline", false, 1_000, 12),
            session("another-baseline", false, 2_000, 14),
        ];
        let result = compare(
            &sessions,
            ToolImpactTargetKind::Provider,
            "alpha",
            None,
            None,
        );
        assert_eq!(result.target_kind, ToolImpactTargetKind::Provider);
        assert_eq!(result.target_key, "alpha");
        assert_eq!(result.observed.turn_count, 1);
        assert_eq!(result.baseline.turn_count, 2);
        assert_eq!(result.matched_pairs, 1);
        assert_eq!(result.matched_observed.tokens.total_tokens, 960);
        assert_eq!(result.matched_baseline.tokens.total_tokens, 1_200);
        assert_eq!(result.matched_observed.buckets[0].model, "gpt-5.5");
    }

    #[test]
    fn discovers_and_compares_individual_tools() {
        let sessions = vec![
            session("alpha", true, 800, 10),
            session("baseline", false, 1_000, 12),
        ];
        let targets = list_targets(&sessions, None, None);
        assert!(targets.iter().any(|target| {
            target.kind == ToolImpactTargetKind::Provider
                && target.key == "alpha"
                && target.turn_count == 1
        }));
        assert!(targets.iter().any(|target| {
            target.kind == ToolImpactTargetKind::Tool
                && target.key == "mcp__alpha__code_search"
                && target.call_count == 1
        }));

        let result = compare(
            &sessions,
            ToolImpactTargetKind::Tool,
            "mcp__alpha__code_search",
            None,
            None,
        );
        assert_eq!(result.observed.turn_count, 1);
        assert_eq!(result.baseline.turn_count, 1);
    }

    #[test]
    fn colliding_provider_ids_remain_distinct_sessions_and_turns() {
        let codex = session("shared", true, 800, 10);
        let mut claude = session("shared", true, 900, 12);
        claude.harness = claude_code_provider_id();
        claude.storage_id = "claude_code:session:shared".into();

        let sessions = vec![codex, claude];
        let targets = list_targets(&sessions, None, None);
        let alpha = targets
            .iter()
            .find(|target| target.kind == ToolImpactTargetKind::Provider && target.key == "alpha")
            .unwrap();
        assert_eq!(alpha.turn_count, 2);

        let result = compare(
            &sessions,
            ToolImpactTargetKind::Provider,
            "alpha",
            None,
            None,
        );
        assert_eq!(result.observed.turn_count, 2);
        assert_eq!(result.observed.session_count, 2);
    }
}
