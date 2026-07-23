use crate::model::{Harness, RangeTotals, Session, TierBucket, TokenTotals, ToolMetrics};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExternalEvent {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    /// None is global; otherwise a canonical path or redacted project identity.
    pub scope: Option<String>,
    pub source: String,
    pub kind: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CorrelationQuery {
    pub events: Vec<ExternalEvent>,
    #[serde(default = "default_window_days")]
    pub before_days: i64,
    #[serde(default = "default_window_days")]
    pub after_days: i64,
    #[serde(default)]
    pub exclude_confounded: bool,
    #[serde(default = "default_true")]
    pub include_subagents: bool,
}

fn default_window_days() -> i64 {
    7
}
fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CorrelationObservation {
    pub session_count: u64,
    pub turn_count: u64,
    /// Sum of each included session's overlap with the observation window.
    pub session_duration_ms: u64,
    pub tokens: TokenTotals,
    pub buckets_by_harness: BTreeMap<Harness, Vec<TierBucket>>,
    pub tool_metrics: ToolMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventCorrelation {
    pub event: ExternalEvent,
    pub before: CorrelationObservation,
    pub after: CorrelationObservation,
    pub token_delta: i64,
    pub session_delta: i64,
    pub confounding_event_ids: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CorrelationResult {
    pub results: Vec<EventCorrelation>,
}

fn is_subagent(session: &Session) -> bool {
    session.parent_thread_id.is_some()
        || session.agent_path.is_some()
        || session.source.as_deref() == Some("subagent")
}

fn normalized_scope(value: &str) -> String {
    value
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

const REDACTED_PROJECT_SCOPE_PREFIX: &str = "project:";

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub(crate) fn project_scope_identity(value: &str) -> String {
    format!(
        "{REDACTED_PROJECT_SCOPE_PREFIX}{}",
        stable_hash(normalized_scope(value).as_bytes())
    )
}

fn redacted_scope_matches(cwd: &str, event_scope: &str) -> bool {
    let mut candidate = normalized_scope(cwd);
    loop {
        if project_scope_identity(&candidate) == event_scope {
            return true;
        }
        let Some(separator) = candidate.rfind('/') else {
            return false;
        };
        candidate.truncate(separator);
    }
}

fn scope_matches(session: &Session, event: &ExternalEvent) -> bool {
    // Harness is optional source metadata, not a source-specific branch. Any
    // event producer can constrain its observations to one harness while the
    // core remains agnostic to config/git event kinds.
    if let Some(harness) = event.metadata.get("harness") {
        let session_harness = match session.harness {
            Harness::Codex => "codex",
            Harness::ClaudeCode => "claude_code",
        };
        if harness != session_harness {
            return false;
        }
    }
    let Some(event_scope) = event.scope.as_deref() else {
        return true;
    };
    let Some(cwd) = session.working_directory.as_deref() else {
        return false;
    };
    if event_scope.starts_with(REDACTED_PROJECT_SCOPE_PREFIX) {
        return redacted_scope_matches(cwd, event_scope);
    }
    let event_scope = normalized_scope(event_scope);
    let cwd = normalized_scope(cwd);
    cwd == event_scope
        || cwd.starts_with(&(event_scope.clone() + "/"))
        || event_scope.starts_with(&(cwd + "/"))
}

fn add_tokens(target: &mut TokenTotals, value: &TokenTotals) {
    target.input_tokens += value.input_tokens;
    target.cached_input_tokens += value.cached_input_tokens;
    target.output_tokens += value.output_tokens;
    target.reasoning_output_tokens += value.reasoning_output_tokens;
    target.total_tokens += value.total_tokens;
}

fn add_bucket(target: &mut Vec<TierBucket>, value: &TierBucket) {
    if let Some(bucket) = target
        .iter_mut()
        .find(|bucket| bucket.model == value.model && bucket.service_tier == value.service_tier)
    {
        add_tokens(&mut bucket.tokens, &value.tokens);
    } else {
        target.push(value.clone());
    }
}

fn interval_overlaps(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> bool {
    from.is_none_or(|from| end >= from) && to.is_none_or(|to| start <= to)
}

fn overlap_duration_ms(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> u64 {
    let start = from.map_or(start, |from| start.max(from));
    let end = to.map_or(end, |to| end.min(to));
    end.signed_duration_since(start).num_milliseconds().max(0) as u64
}

fn add_range(
    out: &mut CorrelationObservation,
    session: &Session,
    window: (Option<DateTime<Utc>>, Option<DateTime<Utc>>),
    range: RangeTotals,
) {
    if range.tokens.total_tokens == 0 && range.tool_metrics.calls == 0 {
        return;
    }
    out.session_count += 1;
    out.turn_count += session
        .turns
        .iter()
        .filter(|turn| {
            let start = turn.started_at.unwrap_or(session.started_at);
            let end = turn.completed_at.unwrap_or(session.last_event_at);
            interval_overlaps(start, end, window.0, window.1)
        })
        .count() as u64;
    out.session_duration_ms += overlap_duration_ms(
        session.started_at,
        session.last_event_at,
        window.0,
        window.1,
    );
    add_tokens(&mut out.tokens, &range.tokens);
    let harness_buckets = out.buckets_by_harness.entry(session.harness).or_default();
    for bucket in &range.buckets {
        add_bucket(harness_buckets, bucket);
    }
    out.tool_metrics.add_assign(&range.tool_metrics);
}

pub fn correlate<S: Borrow<Session>>(sessions: &[S], query: CorrelationQuery) -> CorrelationResult {
    let windows: Vec<_> = query
        .events
        .iter()
        .map(|event| {
            (
                (
                    Some(event.timestamp - Duration::days(query.before_days.max(0))),
                    Some(event.timestamp - Duration::milliseconds(1)),
                ),
                (
                    Some(event.timestamp),
                    Some(event.timestamp + Duration::days(query.after_days.max(0))),
                ),
            )
        })
        .collect();
    let confounds: Vec<Vec<String>> = query
        .events
        .iter()
        .enumerate()
        .map(|(index, event)| {
            let from = windows[index].0 .0.expect("bounded window");
            let to = windows[index].1 .1.expect("bounded window");
            query
                .events
                .iter()
                .filter(|other| {
                    other.id != event.id
                        && other.timestamp >= from
                        && other.timestamp <= to
                        && (event.scope.is_none()
                            || other.scope.is_none()
                            || other.scope == event.scope)
                })
                .map(|other| other.id.clone())
                .collect()
        })
        .collect();
    let excluded: Vec<bool> = confounds
        .iter()
        .map(|items| query.exclude_confounded && !items.is_empty())
        .collect();
    let mut observations: Vec<(CorrelationObservation, CorrelationObservation)> =
        vec![
            (
                CorrelationObservation::default(),
                CorrelationObservation::default()
            );
            query.events.len()
        ];

    // One history traversal per session for every relevant before/after window.
    for session in sessions {
        let session = session.borrow();
        if !query.include_subagents && is_subagent(session) {
            continue;
        }
        let matched: Vec<usize> = query
            .events
            .iter()
            .enumerate()
            .filter(|(index, event)| !excluded[*index] && scope_matches(session, event))
            .map(|(index, _)| index)
            .collect();
        let requested: Vec<_> = matched
            .iter()
            .flat_map(|index| [windows[*index].0, windows[*index].1])
            .collect();
        for (position, pair) in session
            .range_totals_multi(&requested)
            .chunks_exact(2)
            .enumerate()
        {
            let index = matched[position];
            add_range(
                &mut observations[index].0,
                session,
                windows[index].0,
                pair[0].clone(),
            );
            add_range(
                &mut observations[index].1,
                session,
                windows[index].1,
                pair[1].clone(),
            );
        }
    }

    let mut results = Vec::new();
    for (index, event) in query.events.into_iter().enumerate() {
        let (mut before, mut after) = std::mem::take(&mut observations[index]);
        for observation in [&mut before, &mut after] {
            for buckets in observation.buckets_by_harness.values_mut() {
                buckets.sort_by(|a, b| {
                    a.model
                        .cmp(&b.model)
                        .then_with(|| a.service_tier.cmp(&b.service_tier))
                });
            }
        }
        let confounding_event_ids = confounds[index].clone();
        let mut warnings = Vec::new();
        if excluded[index] {
            warnings.push("sample excluded because another event overlaps the window".into());
        }
        if before.session_count < 3 || after.session_count < 3 {
            warnings.push("low sample size; do not interpret the delta as causal".into());
        }
        if !confounding_event_ids.is_empty() && !query.exclude_confounded {
            warnings.push("overlapping events may confound this comparison".into());
        }
        results.push(EventCorrelation {
            event,
            token_delta: after.tokens.total_tokens as i64 - before.tokens.total_tokens as i64,
            session_delta: after.session_count as i64 - before.session_count as i64,
            before,
            after,
            confounding_event_ids,
            warnings,
        });
    }
    CorrelationResult { results }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Harness, TokenHistoryPoint};
    use std::collections::{BTreeMap, HashMap};

    fn session(id: &str, cwd: Option<&str>, points: &[(&str, u64)], subagent: bool) -> Session {
        let history: Vec<_> = points
            .iter()
            .map(|(timestamp, count)| TokenHistoryPoint {
                timestamp: timestamp.parse().unwrap(),
                model: Some("m".into()),
                service_tier: None,
                total_tokens: *count,
                delta: TokenTotals {
                    input_tokens: *count,
                    total_tokens: *count,
                    ..Default::default()
                },
            })
            .collect();
        Session {
            id: id.into(),
            harness: Harness::Codex,
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: subagent.then(|| "parent".into()),
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            archived: false,
            started_at: points.first().unwrap().0.parse().unwrap(),
            last_event_at: points.last().unwrap().0.parse().unwrap(),
            working_directory: cwd.map(str::to_owned),
            originator: None,
            source: None,
            history_mode: None,
            memory_mode: None,
            cli_version: None,
            model_provider: None,
            model: Some("m".into()),
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

    fn event(id: &str, timestamp: &str, scope: Option<&str>) -> ExternalEvent {
        ExternalEvent {
            id: id.into(),
            timestamp: timestamp.parse().unwrap(),
            scope: scope.map(str::to_owned),
            source: "test".into(),
            kind: "change".into(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn uses_inclusive_after_bounds_and_redacted_project_scope() {
        let sessions = vec![
            session(
                "a",
                Some("C:/repo/app"),
                &[("2026-01-01T23:59:59.999Z", 2), ("2026-01-02T00:00:00Z", 4)],
                false,
            ),
            session(
                "b",
                Some("C:/other"),
                &[("2026-01-02T00:00:00Z", 100)],
                false,
            ),
        ];
        let result = correlate(
            &sessions,
            CorrelationQuery {
                events: vec![event(
                    "e",
                    "2026-01-02T00:00:00Z",
                    Some(&project_scope_identity("C:/repo")),
                )],
                before_days: 1,
                after_days: 1,
                exclude_confounded: false,
                include_subagents: true,
            },
        );
        assert_eq!(result.results[0].before.tokens.total_tokens, 2);
        assert_eq!(result.results[0].after.tokens.total_tokens, 4);
    }

    #[test]
    fn project_scope_identities_are_path_normalized_and_do_not_expose_paths() {
        let scope = project_scope_identity("C:\\Private\\Client\\");
        assert_eq!(scope, project_scope_identity("c:/private/client"));
        assert!(scope.starts_with("project:"));
        assert!(!scope.contains("private"));
        assert!(!scope.contains("client"));
    }

    #[test]
    fn flags_and_can_exclude_confounded_low_samples() {
        let events = vec![
            event("a", "2026-01-02T00:00:00Z", None),
            event("b", "2026-01-03T00:00:00Z", None),
        ];
        let result = correlate(
            &[] as &[Session],
            CorrelationQuery {
                events,
                before_days: 2,
                after_days: 2,
                exclude_confounded: true,
                include_subagents: false,
            },
        );
        assert!(!result.results[0].confounding_event_ids.is_empty());
        assert!(result.results[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("excluded")));
    }

    #[test]
    fn config_events_only_match_their_harness() {
        let mut config_event = event("e", "2026-01-02T00:00:00Z", None);
        config_event.source = "config".into();
        config_event
            .metadata
            .insert("harness".into(), "claude_code".into());
        let sessions = vec![
            session("codex", None, &[("2026-01-02T00:00:00Z", 10)], false),
            {
                let mut claude = session("claude", None, &[("2026-01-02T00:00:00Z", 20)], false);
                claude.harness = Harness::ClaudeCode;
                claude
            },
        ];
        let result = correlate(
            &sessions,
            CorrelationQuery {
                events: vec![config_event],
                before_days: 0,
                after_days: 0,
                exclude_confounded: false,
                include_subagents: true,
            },
        );
        assert_eq!(result.results[0].after.tokens.total_tokens, 20);
        assert_eq!(result.results[0].after.session_count, 1);
    }

    #[test]
    fn before_after_windows_are_symmetric_and_subagents_are_optional() {
        let sessions = vec![
            session(
                "before",
                None,
                &[("2026-01-01T00:00:00Z", 4), ("2026-01-01T12:00:00Z", 6)],
                false,
            ),
            session(
                "after",
                None,
                &[("2026-01-02T12:00:00Z", 5), ("2026-01-03T00:00:00Z", 5)],
                false,
            ),
            session("subagent", None, &[("2026-01-02T18:00:00Z", 3)], true),
        ];
        let query = |include_subagents| CorrelationQuery {
            events: vec![event("e", "2026-01-02T00:00:00Z", None)],
            before_days: 1,
            after_days: 1,
            exclude_confounded: false,
            include_subagents,
        };
        let without = correlate(&sessions, query(false));
        assert_eq!(without.results[0].before.tokens.total_tokens, 10);
        assert_eq!(without.results[0].after.tokens.total_tokens, 10);
        assert_eq!(
            without.results[0].before.session_duration_ms,
            12 * 60 * 60 * 1_000
        );
        assert_eq!(
            without.results[0].after.session_duration_ms,
            12 * 60 * 60 * 1_000
        );

        let with = correlate(&sessions, query(true));
        assert_eq!(with.results[0].after.tokens.total_tokens, 13);
        assert_eq!(with.results[0].after.session_count, 2);
    }

    #[test]
    fn missing_scope_and_empty_samples_return_zero_observations() {
        let sessions = vec![session(
            "elsewhere",
            Some("C:/other"),
            &[("2026-01-02T00:00:00Z", 10)],
            false,
        )];
        let result = correlate(
            &sessions,
            CorrelationQuery {
                events: vec![event("e", "2026-01-02T00:00:00Z", Some("C:/missing"))],
                before_days: 1,
                after_days: 1,
                exclude_confounded: false,
                include_subagents: true,
            },
        );
        assert_eq!(result.results[0].before, CorrelationObservation::default());
        assert_eq!(result.results[0].after, CorrelationObservation::default());
        assert!(result.results[0]
            .warnings
            .iter()
            .any(|warning| warning.contains("low sample")));
    }
}
