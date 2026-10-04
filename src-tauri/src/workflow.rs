//! Workflow measurements are observations, never accepted-quality or causal claims.
use anyhow::{bail, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

pub const COMPARISON_VERSION: u32 = 1;
pub const MINIMUM_COMPARISON_SESSIONS: u64 = 3;

#[derive(Debug, Deserialize)]
pub(crate) struct WorkflowSnapshot {
    #[serde(default)]
    pub tool_observations: Vec<crate::model::ToolObservation>,
    #[serde(default)]
    pub tool_metrics: crate::model::ToolMetrics,
    #[serde(default)]
    pub turns: Vec<WorkflowTurn>,
    pub parent_thread_id: Option<String>,
    pub agent_path: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct WorkflowTurn {
    pub turn_id: String,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub classification: Option<crate::model::TurnClassification>,
}

pub(crate) enum WorkflowSource {
    Available(WorkflowSnapshot),
    Unavailable(&'static str),
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowRequest {
    pub session_ids: Vec<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct WorkflowMeasure {
    pub id: String,
    pub value: Option<f64>,
    pub unit: &'static str,
    pub numerator: f64,
    pub denominator: f64,
    pub denominator_is: &'static str,
    pub coverage_is: &'static str,
    pub covered_samples: u64,
    pub eligible_samples: u64,
    pub missing_data: Option<&'static str>,
}

impl WorkflowMeasure {
    fn ratio(
        id: &str,
        numerator: u64,
        denominator: u64,
        denominator_is: &'static str,
        coverage_is: &'static str,
        covered_samples: u64,
        eligible_samples: u64,
    ) -> Self {
        Self {
            id: id.into(),
            value: (denominator > 0).then(|| numerator as f64 / denominator as f64),
            unit: "ratio",
            numerator: numerator as f64,
            denominator: denominator as f64,
            denominator_is,
            coverage_is,
            covered_samples,
            eligible_samples,
            missing_data: (denominator == 0).then_some("no_recorded_denominator"),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct WorkflowWindow {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub ledger_metrics: crate::query::WorkflowMetrics,
    pub additional_metrics: Vec<WorkflowMeasure>,
    pub analyzed_sessions: u64,
    pub unavailable_sessions: u64,
}

#[derive(Debug, Serialize)]
pub struct WorkflowEvidence {
    pub session_id: String,
    pub turn_id: Option<String>,
    pub timestamp: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct WorkflowFinding {
    pub id: String,
    pub provider: String,
    pub project_id: Option<String>,
    pub rule_id: String,
    pub before: FindingObservation,
    pub after: FindingObservation,
    pub comparison: FindingComparison,
    pub evidence: Vec<WorkflowEvidence>,
    pub evidence_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct WorkflowReport {
    pub version: u32,
    pub generated_at: DateTime<Utc>,
    pub analyzer_version: u32,
    pub selected_sessions: usize,
    pub coverage_complete: bool,
    pub before: WorkflowWindow,
    pub after: WorkflowWindow,
    pub findings: Vec<WorkflowFinding>,
    pub limitations: Vec<String>,
}

#[derive(Default)]
struct WindowSignals {
    sessions: u64,
    unavailable: u64,
    tool_calls: u64,
    turn_tool_calls: u64,
    tool_turns: BTreeSet<(String, String)>,
    classified_turns: u64,
    planning_turns: u64,
    timestamped_turns: u64,
    first_edit_ms: Vec<u64>,
    delegated_sessions: u64,
}

fn metric_signals(signals: &mut WindowSignals) -> Vec<WorkflowMeasure> {
    let mut metrics = vec![
        WorkflowMeasure::ratio("tools_per_tool_active_turn", signals.turn_tool_calls,
            signals.tool_turns.len() as u64, "distinct session/turn pairs with recorded tool calls",
            "only tool calls with a recorded turn identity; tool-free turns are outside this metric",
            signals.turn_tool_calls, signals.tool_calls),
        WorkflowMeasure::ratio("planning_turn_share", signals.planning_turns, signals.classified_turns,
            "versioned classified turns whose recorded start is inside the window",
            "existing deterministic classifier; unclassified or timestamp-free turns are excluded",
            signals.classified_turns, signals.timestamped_turns),
        WorkflowMeasure::ratio("observed_subagent_session_share", signals.delegated_sessions, signals.sessions,
            "analyzed sessions with tool observations inside the window",
            "explicit parent_thread_id or agent_path metadata; this does not measure all delegation",
            signals.sessions, signals.sessions + signals.unavailable),
    ];
    signals.first_edit_ms.sort_unstable();
    let len = signals.first_edit_ms.len();
    let median = if len == 0 {
        None
    } else if len % 2 == 1 {
        Some(signals.first_edit_ms[len / 2] as f64)
    } else {
        Some(
            signals.first_edit_ms[len / 2 - 1] as f64 / 2.0
                + signals.first_edit_ms[len / 2] as f64 / 2.0,
        )
    };
    metrics.push(WorkflowMeasure {
        id: "median_time_to_first_edit".into(), value: median, unit: "milliseconds",
        numerator: len as f64, denominator: signals.timestamped_turns as f64,
        denominator_is: "turns with a recorded start in the window",
        coverage_is: "samples have a linked mutation timestamp at or after that start; tool wait time is not accepted delivery time",
        covered_samples: len as u64, eligible_samples: signals.timestamped_turns,
        missing_data: median.is_none().then_some("no_linked_timed_mutations"),
    });
    for id in [
        "user_correction_rate",
        "accepted_delivery_time",
        "realized_causal_savings",
    ] {
        metrics.push(WorkflowMeasure {
            id: id.into(), value: None, unit: "unavailable", numerator: 0.0, denominator: 0.0,
            denominator_is: "explicit human acceptance/correction labels with a comparable baseline",
            coverage_is: "tool failures, mutation retries and completion timestamps are not human acceptance labels",
            covered_samples: 0, eligible_samples: signals.timestamped_turns,
            missing_data: Some("human_outcome_labels_not_recorded"),
        });
    }
    metrics
}

#[derive(Default)]
struct ScopeSignals {
    before_sessions: u64,
    after_sessions: u64,
    before_calls: u64,
    after_calls: u64,
    complete: bool,
    rules: BTreeMap<String, RuleSignals>,
}

#[derive(Default)]
struct RuleSignals {
    before_findings: u64,
    after_findings: u64,
    before_avoidable: u64,
    after_avoidable: u64,
    evidence: Vec<WorkflowEvidence>,
    evidence_truncated: bool,
}

fn in_window(timestamp: DateTime<Utc>, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    from <= timestamp && timestamp <= to
}

/// Local, explicit read. Both periods use the same analyzer on existing normalized
/// observations. Original finding bodies and prompt/output text are never projected.
pub fn report(
    store: &crate::history_store::HistoryStore,
    rates: &crate::rates::RateCard,
    request: WorkflowRequest,
    events: &[crate::correlation::ExternalEvent],
    now: DateTime<Utc>,
) -> Result<WorkflowReport> {
    if request.session_ids.len() > 10_000 {
        bail!("workflow analysis is limited to 10,000 selected sessions");
    }
    let after_to = request.to.unwrap_or(now);
    let after_from = request.from.unwrap_or(after_to - Duration::days(7));
    let duration = after_to.signed_duration_since(after_from) + Duration::milliseconds(1);
    if duration < Duration::hours(1) || duration > Duration::days(90) || after_to > now {
        bail!("workflow windows must contain 1 hour to 90 days of recorded history and end no later than now");
    }
    let before_to = after_from - Duration::milliseconds(1);
    let before_from = before_to - duration + Duration::milliseconds(1);
    let known: HashSet<_> = store.session_keys()?.into_iter().collect();
    let requested: HashSet<_> = request.session_ids.into_iter().collect();
    let mut keys: Vec<_> = requested.intersection(&known).cloned().collect();
    keys.sort();
    let selected: HashSet<_> = keys.iter().cloned().collect();
    let coverage_complete = store.has_complete_coverage()? && selected.len() == requested.len();
    let before_metrics = crate::query::workflow_metrics_for_keys(
        store,
        rates,
        |key| {
            key.split_once(':')
                .map_or("unknown", |(provider, _)| provider)
                .into()
        },
        &keys,
        Some(before_from),
        Some(before_to),
        now,
    )?;
    let after_metrics = crate::query::workflow_metrics_for_keys(
        store,
        rates,
        |key| {
            key.split_once(':')
                .map_or("unknown", |(provider, _)| provider)
                .into()
        },
        &keys,
        Some(after_from),
        Some(after_to),
        now,
    )?;
    let ranges = store.range_totals_multi(
        &keys,
        &[
            (Some(before_from), Some(before_to)),
            (Some(after_from), Some(after_to)),
        ],
    )?;
    let mut before = WindowSignals::default();
    let mut after = WindowSignals::default();
    let mut scopes: BTreeMap<(String, Option<String>), ScopeSignals> = BTreeMap::new();
    let mut unavailable_reasons = BTreeSet::new();
    store.stream_workflow_snapshots(&selected, |key, project, source| {
        let provider = key
            .split_once(':')
            .map_or("unknown", |(provider, _)| provider)
            .to_owned();
        let scope = scopes
            .entry((provider, project.map(str::to_owned)))
            .or_insert_with(|| ScopeSignals {
                complete: true,
                ..Default::default()
            });
        let window_ranges = [ranges[0].get(key), ranges[1].get(key)];
        let contributes_usage = window_ranges.iter().any(|range| {
            range.is_some_and(|range| range.tool_metrics.calls > 0 || range.tokens.total_tokens > 0)
        });
        for (index, range) in window_ranges.iter().enumerate() {
            if let Some(range) = range {
                let signals = if index == 0 { &mut before } else { &mut after };
                signals.tool_calls += range.tool_metrics.calls;
                if range.tool_metrics.calls > 0 || range.tokens.total_tokens > 0 {
                    if index == 0 {
                        scope.before_sessions += 1;
                        scope.before_calls += range.tool_metrics.calls;
                    } else {
                        scope.after_sessions += 1;
                        scope.after_calls += range.tool_metrics.calls;
                    }
                }
            }
        }
        let snapshot = match source {
            WorkflowSource::Unavailable(reason) => {
                if contributes_usage {
                    scope.complete = false;
                    unavailable_reasons.insert(reason.to_owned());
                }
                for (index, range) in window_ranges.iter().enumerate() {
                    if range.is_some_and(|range| {
                        range.tool_metrics.calls > 0 || range.tokens.total_tokens > 0
                    }) {
                        if index == 0 {
                            before.unavailable += 1;
                        } else {
                            after.unavailable += 1;
                        }
                    }
                }
                return Ok(());
            }
            WorkflowSource::Available(snapshot) => snapshot,
        };
        if contributes_usage
            && snapshot.tool_metrics.calls != snapshot.tool_observations.len() as u64
        {
            scope.complete = false;
            unavailable_reasons.insert("normalized_observation_coverage_mismatch".into());
        }
        for (index, (from, to)) in [(before_from, before_to), (after_from, after_to)]
            .into_iter()
            .enumerate()
        {
            let signals = if index == 0 { &mut before } else { &mut after };
            let observations: Vec<_> = snapshot
                .tool_observations
                .iter()
                .filter(|item| in_window(item.timestamp, from, to))
                .collect();
            let ledger_calls = window_ranges[index].map_or(0, |range| range.tool_metrics.calls);
            if ledger_calls != observations.len() as u64 {
                scope.complete = false;
                unavailable_reasons.insert("window_observation_coverage_mismatch".into());
            }
            if ledger_calls > 0
                || window_ranges[index].is_some_and(|range| range.tokens.total_tokens > 0)
            {
                signals.sessions += 1;
                signals.delegated_sessions +=
                    u64::from(snapshot.parent_thread_id.is_some() || snapshot.agent_path.is_some());
            }
            for item in &observations {
                if let Some(turn_id) = &item.turn_id {
                    signals.turn_tool_calls += 1;
                    signals.tool_turns.insert((key.into(), turn_id.clone()));
                }
            }
            for turn in &snapshot.turns {
                let Some(start) = turn.started_at.filter(|start| in_window(*start, from, to))
                else {
                    continue;
                };
                signals.timestamped_turns += 1;
                if let Some(classification) = &turn.classification {
                    signals.classified_turns += 1;
                    signals.planning_turns +=
                        u64::from(classification.category == crate::model::TaskCategory::Planning);
                }
                let first_edit = observations
                    .iter()
                    .filter(|item| {
                        item.turn_id.as_deref() == Some(turn.turn_id.as_str())
                            && item.kind == crate::model::ToolKind::Mutation
                            && item.timestamp >= start
                            && turn.completed_at.is_none_or(|end| item.timestamp <= end)
                    })
                    .map(|item| item.timestamp)
                    .min();
                if let Some(edit) = first_edit {
                    signals
                        .first_edit_ms
                        .push(edit.signed_duration_since(start).num_milliseconds() as u64);
                }
            }
            for finding in crate::telemetry::findings(observations) {
                let rule = scope.rules.entry(finding.rule_id).or_default();
                if index == 0 {
                    rule.before_findings += 1;
                    rule.before_avoidable += finding.avoidable_calls;
                } else {
                    rule.after_findings += 1;
                    rule.after_avoidable += finding.avoidable_calls;
                }
                if rule.evidence.len() < 20 {
                    rule.evidence.push(WorkflowEvidence {
                        session_id: key.into(),
                        turn_id: finding.turn_id,
                        timestamp: finding.timestamp,
                    });
                } else {
                    rule.evidence_truncated = true;
                }
            }
        }
        Ok(())
    })?;
    let confounded = events
        .iter()
        .any(|event| in_window(event.timestamp, before_from, after_to));
    let mut findings = Vec::new();
    for ((provider, project), scope) in scopes {
        for (rule_id, rule) in scope.rules {
            let observation = |sessions, calls, count, avoidable| FindingObservation {
                sessions,
                tool_calls: calls,
                findings: count,
                likely_avoidable_calls: avoidable,
                analyzer_version: scope.complete.then_some(crate::telemetry::ANALYZER_VERSION),
                coverage_complete: coverage_complete && scope.complete,
                window_duration_ms: duration.num_milliseconds() as u64,
            };
            let before_observation = observation(
                scope.before_sessions,
                scope.before_calls,
                rule.before_findings,
                rule.before_avoidable,
            );
            let after_observation = observation(
                scope.after_sessions,
                scope.after_calls,
                rule.after_findings,
                rule.after_avoidable,
            );
            findings.push(WorkflowFinding {
                id: finding_identity(
                    &provider,
                    project.as_deref().unwrap_or("unassigned"),
                    &rule_id,
                ),
                provider: provider.clone(),
                project_id: project.clone(),
                rule_id,
                comparison: compare_finding(
                    &before_observation,
                    &after_observation,
                    false,
                    confounded,
                ),
                before: before_observation,
                after: after_observation,
                evidence: rule.evidence,
                evidence_truncated: rule.evidence_truncated,
            });
        }
    }
    findings.sort_by(|left, right| left.id.cmp(&right.id));
    let mut limitations: Vec<_> = unavailable_reasons.into_iter().collect();
    if !coverage_complete {
        limitations.push("ledger_coverage_incomplete".into());
    }
    limitations.push("timing_and_tool_success_do_not_establish_accepted_quality".into());
    if confounded {
        limitations.push("recorded_changes_overlap_comparison_windows".into());
    }
    Ok(WorkflowReport {
        version: COMPARISON_VERSION,
        generated_at: now,
        analyzer_version: crate::telemetry::ANALYZER_VERSION,
        selected_sessions: keys.len(),
        coverage_complete,
        before: WorkflowWindow {
            from: before_from,
            to: before_to,
            ledger_metrics: before_metrics,
            additional_metrics: metric_signals(&mut before),
            analyzed_sessions: before.sessions,
            unavailable_sessions: before.unavailable,
        },
        after: WorkflowWindow {
            from: after_from,
            to: after_to,
            ledger_metrics: after_metrics,
            additional_metrics: metric_signals(&mut after),
            analyzed_sessions: after.sessions,
            unavailable_sessions: after.unavailable,
        },
        findings,
        limitations,
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FindingState {
    New,
    Persistent,
    Improving,
    Resolved,
    Suppressed,
    NotApplicable,
}

/// Rule identity is separate from analyzer version and from presentation text.
/// The project scope supplied here is the existing opaque canonical project key.
pub fn finding_identity(provider: &str, project: &str, rule: &str) -> String {
    let mut identity = Vec::new();
    for value in ["workflow-finding-v1", provider, project, rule] {
        identity.extend_from_slice(&(value.len() as u64).to_le_bytes());
        identity.extend_from_slice(value.as_bytes());
    }
    format!("finding:{:016x}", crate::stable_hash::fnv1a64(&identity))
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FindingObservation {
    pub sessions: u64,
    pub tool_calls: u64,
    pub findings: u64,
    pub likely_avoidable_calls: u64,
    /// Proof of which analyzer ran, including sessions with no findings.
    pub analyzer_version: Option<u32>,
    pub coverage_complete: bool,
    pub window_duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FindingComparison {
    pub version: u32,
    pub state: FindingState,
    pub comparable: bool,
    pub before_calls_per_100: Option<f64>,
    pub after_calls_per_100: Option<f64>,
    /// Observed rate difference, not realized token/cost savings.
    pub observed_change_per_100_calls: Option<f64>,
    pub limitations: Vec<String>,
}

pub fn compare_finding(
    before: &FindingObservation,
    after: &FindingObservation,
    suppressed: bool,
    confounded: bool,
) -> FindingComparison {
    let rate = |observation: &FindingObservation| {
        (observation.tool_calls > 0).then(|| {
            observation
                .likely_avoidable_calls
                .min(observation.tool_calls) as f64
                * 100.0
                / observation.tool_calls as f64
        })
    };
    let mut limitations = Vec::new();
    if !before.coverage_complete || !after.coverage_complete {
        limitations.push("incomplete_coverage".into());
    }
    if before.analyzer_version.is_none() || before.analyzer_version != after.analyzer_version {
        limitations.push("analyzer_definitions_not_comparable".into());
    }
    if before.window_duration_ms == 0 || before.window_duration_ms != after.window_duration_ms {
        limitations.push("unequal_or_empty_windows".into());
    }
    if before.sessions < MINIMUM_COMPARISON_SESSIONS || after.sessions < MINIMUM_COMPARISON_SESSIONS
    {
        limitations.push("low_sample_size".into());
    }
    let before_rate = rate(before);
    let after_rate = rate(after);
    if before_rate.is_none() || after_rate.is_none() {
        limitations.push("missing_tool_call_denominator".into());
    }
    if confounded {
        limitations.push("overlapping_changes".into());
    }
    let comparable = limitations.is_empty();
    let state = if suppressed {
        FindingState::Suppressed
    } else if !comparable {
        FindingState::NotApplicable
    } else if before.findings == 0 && after.findings > 0 {
        FindingState::New
    } else if before.findings > 0 && after.findings == 0 {
        FindingState::Resolved
    } else if after.findings == 0 {
        FindingState::NotApplicable
    } else if after_rate < before_rate {
        FindingState::Improving
    } else {
        FindingState::Persistent
    };
    let observed_change_per_100_calls = comparable
        .then(|| {
            after_rate
                .zip(before_rate)
                .map(|(after, before)| after - before)
        })
        .flatten();
    limitations.push("observational_comparison_not_causal_savings".into());
    FindingComparison {
        version: COMPARISON_VERSION,
        state,
        comparable,
        before_calls_per_100: before_rate,
        after_calls_per_100: after_rate,
        observed_change_per_100_calls,
        limitations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measured_ledger() -> (
        tempfile::TempDir,
        crate::history_store::HistoryStore,
        Vec<String>,
        DateTime<Utc>,
    ) {
        use crate::telemetry::{observe_call, ToolCallInput};
        let directory = tempfile::tempdir().unwrap();
        let store =
            crate::history_store::HistoryStore::open(&directory.path().join("history.sqlite3"))
                .unwrap();
        let generation = store.begin_scan().unwrap();
        let now: DateTime<Utc> = "2026-10-04T00:00:00Z".parse().unwrap();
        let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sample-session.jsonl");
        for day in [8, 9, 10, 1, 2, 3] {
            let start = now - Duration::days(day);
            let mut session = crate::parser::parse_file(&fixture, false).unwrap().unwrap();
            session.id = format!("workflow-{day}");
            session.storage_id.clear();
            session.started_at = start;
            session.last_event_at = start + Duration::minutes(3);
            session.tokens_history.clear();
            session.tokens_total = crate::model::TokenTotals::default();
            session.tokens_by_model.clear();
            session.tool_observations.clear();
            session.turns.truncate(1);
            let turn = &mut session.turns[0];
            turn.turn_id = "synthetic-turn".into();
            turn.started_at = Some(start);
            turn.completed_at = Some(start + Duration::minutes(3));
            turn.user_message = Some("PRIVATE PROMPT secret-filename.txt secret-command".into());
            turn.last_agent_message = Some("PRIVATE RESPONSE".into());
            for index in 0..3 {
                observe_call(
                    &mut session.tool_observations,
                    ToolCallInput {
                        call_id: format!("call-{index}"),
                        turn_id: Some("synthetic-turn".into()),
                        harness: crate::provider::codex_provider_id(),
                        model: Some("gpt-5.4".into()),
                        timestamp: start + Duration::minutes(index),
                        name: "read_file".into(),
                        arguments: &serde_json::json!({"path": if day > 7 { "PRIVATE-FILENAME".into() } else { format!("PRIVATE-FILENAME-{index}") }}),
                    },
                );
                session.tool_observations.last_mut().unwrap().outcome =
                    crate::model::ToolOutcome::Success;
            }
            crate::telemetry::refresh_session(&mut session);
            store
                .observe(
                    &directory.path().join(format!("source-{day}.jsonl")),
                    &session,
                    generation,
                )
                .unwrap();
        }
        let keys = store.session_keys().unwrap();
        (directory, store, keys, now)
    }

    #[test]
    fn durable_comparison_reconciles_tool_only_sessions_and_omits_private_bodies() {
        let (_directory, store, keys, now) = measured_ledger();
        let reader = store.workflow_reader().unwrap();
        let report = report(
            &reader,
            &crate::rates::RateCard::default(),
            WorkflowRequest {
                session_ids: keys,
                from: None,
                to: None,
            },
            &[],
            now,
        )
        .unwrap();
        assert_eq!(report.before.ledger_metrics.sessions, 3);
        assert_eq!(report.after.ledger_metrics.sessions, 3);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].comparison.state, FindingState::Resolved);
        assert_eq!(report.findings[0].before.tool_calls, 9);
        assert_eq!(report.findings[0].before.likely_avoidable_calls, 6);
        let tools = report
            .after
            .additional_metrics
            .iter()
            .find(|metric| metric.id == "tools_per_tool_active_turn")
            .unwrap();
        assert_eq!(tools.numerator, 9.0);
        assert_eq!(tools.denominator, 3.0);
        assert_eq!(tools.value, Some(3.0));
        let serialized = serde_json::to_string(&report).unwrap();
        for private in [
            "PRIVATE",
            "secret-command",
            "secret-filename",
            "user_message",
            "last_agent_message",
            "remediation",
        ] {
            assert!(
                !serialized.contains(private),
                "private field/body leaked: {private}"
            );
        }
    }

    #[test]
    fn missing_requested_identity_and_confounds_do_not_become_resolved_wins() {
        let (_directory, store, mut keys, now) = measured_ledger();
        keys.push("codex:unavailable-identity".into());
        let report = report(
            &store.workflow_reader().unwrap(),
            &crate::rates::RateCard::default(),
            WorkflowRequest {
                session_ids: keys,
                from: None,
                to: None,
            },
            &[],
            now,
        )
        .unwrap();
        assert!(!report.coverage_complete);
        assert_eq!(
            report.findings[0].comparison.state,
            FindingState::NotApplicable
        );
    }

    fn observed(findings: u64, avoidable: u64, calls: u64) -> FindingObservation {
        FindingObservation {
            sessions: 3,
            tool_calls: calls,
            findings,
            likely_avoidable_calls: avoidable,
            analyzer_version: Some(3),
            coverage_complete: true,
            window_duration_ms: 7 * 24 * 60 * 60 * 1000,
        }
    }

    #[test]
    fn identities_are_scope_specific_and_do_not_use_versions_or_evidence() {
        let original = finding_identity("codex", "opaque-project", "repeated-read");
        assert_eq!(
            original,
            finding_identity("codex", "opaque-project", "repeated-read")
        );
        assert_ne!(
            original,
            finding_identity("claude_code", "opaque-project", "repeated-read")
        );
        assert_ne!(
            finding_identity("a", "bc", "d"),
            finding_identity("ab", "c", "d")
        );
        assert!(!original.contains("opaque-project"));
    }

    #[test]
    fn improving_uses_rates_instead_of_smaller_workload_counts() {
        let before = observed(4, 10, 100);
        let smaller_workload = observed(2, 8, 50);
        assert_eq!(
            compare_finding(&before, &smaller_workload, false, false).state,
            FindingState::Persistent
        );
        let larger_workload = observed(6, 12, 200);
        let comparison = compare_finding(&before, &larger_workload, false, false);
        assert_eq!(comparison.state, FindingState::Improving);
        assert_eq!(comparison.observed_change_per_100_calls, Some(-4.0));
    }

    #[test]
    fn missing_evidence_and_version_changes_never_become_resolved_wins() {
        let before = observed(4, 10, 100);
        let clean = observed(0, 0, 100);
        assert_eq!(
            compare_finding(&before, &clean, false, false).state,
            FindingState::Resolved
        );
        for incomplete in [
            FindingObservation {
                analyzer_version: Some(4),
                ..clean.clone()
            },
            FindingObservation {
                analyzer_version: None,
                ..clean.clone()
            },
            FindingObservation {
                coverage_complete: false,
                ..clean.clone()
            },
            FindingObservation {
                sessions: 1,
                ..clean.clone()
            },
            FindingObservation {
                tool_calls: 0,
                ..clean.clone()
            },
            FindingObservation {
                window_duration_ms: 1,
                ..clean.clone()
            },
        ] {
            let comparison = compare_finding(&before, &incomplete, false, false);
            assert_eq!(comparison.state, FindingState::NotApplicable);
            assert_eq!(comparison.observed_change_per_100_calls, None);
        }
        assert!(!compare_finding(&before, &clean, false, true).comparable);
    }

    #[test]
    fn suppression_remains_explicit_even_without_comparable_observations() {
        assert_eq!(
            compare_finding(
                &FindingObservation::default(),
                &FindingObservation::default(),
                true,
                true
            )
            .state,
            FindingState::Suppressed
        );
        assert_eq!(
            compare_finding(&observed(0, 0, 100), &observed(1, 2, 100), false, false).state,
            FindingState::New
        );
    }
}
