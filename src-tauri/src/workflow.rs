//! Workflow measurements are observations, never accepted-quality or causal claims.
use anyhow::{bail, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};

#[path = "workflow_lifecycle.rs"]
pub(crate) mod lifecycle;
pub use lifecycle::{FindingLifecycle, FindingSuppressionEdit};

pub const COMPARISON_VERSION: u32 = 1;
pub const MINIMUM_COMPARISON_SESSIONS: u64 = 3;

pub(crate) fn fits_output_budget(value: &impl Serialize, limit: usize) -> bool {
    struct Counter {
        remaining: usize,
    }
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.remaining {
                return Err(std::io::Error::other("workflow output limit"));
            }
            self.remaining -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Counter { remaining: limit }, value).is_ok()
}

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
    /// Descriptive counts only; category is the existing versioned classifier.
    pub drilldowns: Vec<WorkflowDrilldown>,
}

#[derive(Debug, Default, Serialize)]
pub struct WorkflowDrilldown {
    pub dimension: &'static str,
    pub value: String,
    pub sessions: u64,
    pub tool_calls: u64,
    pub classified_turns: u64,
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
    pub lifecycle: Option<FindingLifecycle>,
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
    pub historical_findings: Vec<HistoricalWorkflowFinding>,
    pub setup_health: Option<WorkflowSetupHealth>,
    pub limitations: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct HistoricalWorkflowFinding {
    pub id: String,
    pub provider: String,
    pub project_id: Option<String>,
    pub rule_id: String,
    pub lifecycle: FindingLifecycle,
}

#[derive(Debug, Serialize)]
pub struct WorkflowProviderHealth {
    pub provider: String,
    pub state: crate::diagnostics::ProviderHealthState,
    pub configured_roots: usize,
    pub available_roots: usize,
    pub parsed_files: u64,
    pub parse_failures: u64,
    pub durable_sessions: u64,
    pub fallback_pricing_used: bool,
    pub reasons: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WorkflowSetupHealth {
    pub source_configuration_valid: bool,
    pub generated_at: DateTime<Utc>,
    pub last_scan_at: Option<DateTime<Utc>>,
    pub providers: Vec<WorkflowProviderHealth>,
}

impl WorkflowSetupHealth {
    pub(crate) fn from_diagnostics(report: crate::diagnostics::DiagnosticsReport) -> Self {
        Self {
            source_configuration_valid: report.source_configuration_valid,
            generated_at: report.generated_at,
            last_scan_at: report.last_scan_at,
            providers: report
                .providers
                .into_iter()
                .map(|provider| WorkflowProviderHealth {
                    provider: provider.id.to_string(),
                    state: provider.state,
                    configured_roots: provider.roots.len(),
                    available_roots: provider.roots.iter().filter(|root| root.exists).count(),
                    parsed_files: provider.discovery.parsed_files,
                    parse_failures: provider.discovery.parse_failures,
                    durable_sessions: provider.ledger.durable_sessions,
                    fallback_pricing_used: provider.pricing.fallback_used,
                    reasons: provider
                        .reasons
                        .into_iter()
                        .map(|reason| reason.code)
                        .collect(),
                })
                .collect(),
        }
    }
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
    drilldowns: BTreeMap<(&'static str, String), WorkflowDrilldown>,
}

impl WindowSignals {
    fn drilldown(&mut self, dimension: &'static str, value: String) -> &mut WorkflowDrilldown {
        self.drilldowns
            .entry((dimension, value.clone()))
            .or_insert_with(|| WorkflowDrilldown {
                dimension,
                value,
                ..Default::default()
            })
    }
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
    from.timestamp_millis() <= timestamp.timestamp_millis()
        && timestamp.timestamp_millis() <= to.timestamp_millis()
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
    if request.session_ids.iter().any(|id| id.len() > 1024)
        || request.session_ids.iter().map(String::len).sum::<usize>() > 1024 * 1024
    {
        bail!("workflow identity input exceeds its metadata limit");
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
                let project_name = project.unwrap_or("unattributed").to_owned();
                let project_row = signals.drilldown("project", project_name);
                project_row.tool_calls += range.tool_metrics.calls;
                if range.tool_metrics.calls > 0 || range.tokens.total_tokens > 0 {
                    project_row.sessions += 1;
                }
                for (model, totals) in &range.tool_metrics_by_model {
                    signals.drilldown("model", model.clone()).tool_calls += totals.calls;
                }
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
                    let category = serde_json::to_value(classification.category)?
                        .as_str()
                        .unwrap_or("other")
                        .to_owned();
                    signals.drilldown("category", category).classified_turns += 1;
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
                id: finding_identity(&provider, project.as_deref().unwrap_or(""), &rule_id),
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
                lifecycle: None,
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
            drilldowns: before.drilldowns.into_values().collect(),
        },
        after: WorkflowWindow {
            from: after_from,
            to: after_to,
            ledger_metrics: after_metrics,
            additional_metrics: metric_signals(&mut after),
            analyzed_sessions: after.sessions,
            unavailable_sessions: after.unavailable,
            drilldowns: after.drilldowns.into_values().collect(),
        },
        findings,
        historical_findings: Vec::new(),
        setup_health: None,
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

    #[test]
    fn explicit_measurement_persists_but_read_only_reports_and_new_project_scopes_do_not_write() {
        let (_directory, store, keys, now) = measured_ledger();
        let measure = |at| {
            report(
                &store.workflow_reader().unwrap(),
                &crate::rates::RateCard::default(),
                WorkflowRequest {
                    session_ids: keys.clone(),
                    from: None,
                    to: None,
                },
                &[],
                at,
            )
            .unwrap()
        };
        let mut first = measure(now);
        store.load_workflow_lifecycle(&mut first).unwrap();
        assert!(first
            .findings
            .iter()
            .all(|finding| finding.lifecycle.is_none()));
        let mut second = measure(now);
        store.load_workflow_lifecycle(&mut second).unwrap();
        assert!(second.historical_findings.is_empty());
        assert!(second
            .findings
            .iter()
            .all(|finding| finding.lifecycle.is_none()));
        store.record_workflow_measurement(&mut first).unwrap();
        let original = &first.findings[0];
        store
            .suppress_workflow_finding(&FindingSuppressionEdit {
                provider: original.provider.clone(),
                project_id: original.project_id.clone(),
                rule_id: original.rule_id.clone(),
                expected_revision: original.lifecycle.as_ref().unwrap().revision,
                suppressed: true,
            })
            .unwrap();
        let mut reopened = measure(now + Duration::milliseconds(1));
        store.load_workflow_lifecycle(&mut reopened).unwrap();
        assert_eq!(
            reopened.findings[0].comparison.state,
            FindingState::Suppressed
        );
        assert_eq!(
            reopened.findings[0]
                .lifecycle
                .as_ref()
                .unwrap()
                .first_observed_at,
            now
        );
        for key in &keys {
            store
                .reassign_session_project(key, Some("manual:new-scope"))
                .unwrap();
        }
        let mut moved = measure(now + Duration::milliseconds(2));
        store.load_workflow_lifecycle(&mut moved).unwrap();
        assert!(moved.findings[0].lifecycle.is_none());
        assert_ne!(moved.findings[0].id, first.findings[0].id);
        assert_ne!(moved.findings[0].comparison.state, FindingState::Suppressed);
        assert_eq!(moved.historical_findings.len(), 1);
        assert_eq!(moved.historical_findings[0].id, first.findings[0].id);
        assert!(moved.historical_findings[0].lifecycle.suppressed);
    }

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
            turn.tokens = crate::model::TokenTotals::default();
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
        for dimension in ["project", "model"] {
            assert_eq!(
                report
                    .after
                    .drilldowns
                    .iter()
                    .filter(|row| row.dimension == dimension)
                    .map(|row| row.tool_calls)
                    .sum::<u64>(),
                9
            );
        }
        assert_eq!(
            report
                .after
                .drilldowns
                .iter()
                .filter(|row| row.dimension == "project")
                .map(|row| row.sessions)
                .sum::<u64>(),
            report.after.analyzed_sessions
        );
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

    #[test]
    fn finding_scope_follows_durable_reassignment_and_canonical_merge() {
        let (_directory, store, keys, now) = measured_ledger();
        for key in &keys {
            store
                .reassign_session_project(key, Some("manual:source"))
                .unwrap();
        }
        store
            .merge_project("manual:source", "manual:canonical")
            .unwrap();
        let query = || WorkflowRequest {
            session_ids: keys.clone(),
            from: None,
            to: None,
        };
        let merged = report(
            &store.workflow_reader().unwrap(),
            &crate::rates::RateCard::default(),
            query(),
            &[],
            now,
        )
        .unwrap();
        assert_eq!(merged.findings.len(), 1);
        assert_eq!(
            merged.findings[0].project_id.as_deref(),
            Some("manual:canonical")
        );
        assert_eq!(
            merged.findings[0].id,
            finding_identity("codex", "manual:canonical", "repeated-read")
        );
        let before_key = merged.findings[0].evidence[0].session_id.clone();
        store
            .reassign_session_project(&before_key, Some("manual:split"))
            .unwrap();
        let split = report(
            &store.workflow_reader().unwrap(),
            &crate::rates::RateCard::default(),
            query(),
            &[],
            now,
        )
        .unwrap();
        assert_eq!(split.findings.len(), 2);
        let moved = split
            .findings
            .iter()
            .find(|finding| finding.project_id.as_deref() == Some("manual:split"))
            .unwrap();
        assert!(moved
            .evidence
            .iter()
            .all(|evidence| evidence.session_id == before_key));
        assert_eq!(moved.comparison.state, FindingState::NotApplicable);
        assert!(split
            .findings
            .iter()
            .all(|finding| finding.project_id.as_deref() != Some("manual:source")));
    }

    #[test]
    fn serialized_output_budget_counts_json_escaping_without_allocating_output() {
        let value = "\n\"\\😀".repeat(10);
        let actual = serde_json::to_vec(&value).unwrap().len();
        assert!(fits_output_budget(&value, actual));
        assert!(!fits_output_budget(&value, actual - 1));
    }

    #[test]
    fn unicode_text_snapshot_is_limited_by_bytes_and_does_not_hide_ledger_usage() {
        let (directory, store, keys, now) = measured_ledger();
        let key = keys.iter().find(|key| key.ends_with("workflow-8")).unwrap();
        let connection =
            rusqlite::Connection::open(directory.path().join("history.sqlite3")).unwrap();
        let multibyte = "😀".repeat(4 * 1024 * 1024);
        connection.execute("UPDATE session_snapshots SET session_json = CAST(json_set(CAST(session_json AS TEXT),'$.first_user_message',?2) AS TEXT) WHERE session_key=?1", rusqlite::params![key, multibyte]).unwrap();
        let (characters, bytes): (i64, i64) = connection.query_row("SELECT length(session_json),length(CAST(session_json AS BLOB)) FROM session_snapshots WHERE session_key=?1", [key], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert!(characters < 16 * 1024 * 1024);
        assert!(bytes > 16 * 1024 * 1024);
        drop(connection);
        let measured = report(
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
        assert_eq!(measured.before.unavailable_sessions, 1);
        assert_eq!(measured.before.ledger_metrics.metrics[0].denominator, 9.0);
        assert!(measured
            .limitations
            .iter()
            .any(|reason| reason == "snapshot_analysis_limit"));
        assert!(measured
            .findings
            .iter()
            .all(|finding| finding.comparison.state == FindingState::NotApplicable));
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
