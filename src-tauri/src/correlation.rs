use crate::model::{
    Harness, RangeTotals, Session, SessionSummary, TierBucket, TokenTotals, ToolMetrics,
};
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
    pub after_window_end: DateTime<Utc>,
    pub after_window_complete: bool,
    pub minimum_session_count: u64,
    pub sample_ready: bool,
    pub token_delta: i64,
    pub session_delta: i64,
    pub confounding_event_ids: Vec<String>,
    pub warnings: Vec<String>,
}

const MINIMUM_SESSION_COUNT: u64 = 3;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct CorrelationResult {
    pub results: Vec<EventCorrelation>,
}

/// Fields `is_subagent`/`scope_matches` need, present identically on both
/// full `Session` and its resident `SessionSummary` (issue #139 follow-up).
/// One implementation shared by both means the summary-level pre-filter in
/// [`candidate_session_keys`] can never drift from what [`correlate_at`]
/// itself decides once it has full content — the pre-filter can only ever
/// be a superset of what the final pass keeps, never a stricter, possibly
/// wrong, second opinion.
trait SessionScope {
    fn harness(&self) -> &Harness;
    fn working_directory(&self) -> Option<&str>;
    fn parent_thread_id(&self) -> Option<&str>;
    fn agent_path(&self) -> Option<&str>;
    fn source(&self) -> Option<&str>;
}

impl SessionScope for Session {
    fn harness(&self) -> &Harness {
        &self.harness
    }
    fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }
    fn parent_thread_id(&self) -> Option<&str> {
        self.parent_thread_id.as_deref()
    }
    fn agent_path(&self) -> Option<&str> {
        self.agent_path.as_deref()
    }
    fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
}

impl SessionScope for SessionSummary {
    fn harness(&self) -> &Harness {
        &self.harness
    }
    fn working_directory(&self) -> Option<&str> {
        self.working_directory.as_deref()
    }
    fn parent_thread_id(&self) -> Option<&str> {
        self.parent_thread_id.as_deref()
    }
    fn agent_path(&self) -> Option<&str> {
        self.agent_path.as_deref()
    }
    fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }
}

fn is_subagent<T: SessionScope>(session: &T) -> bool {
    session.parent_thread_id().is_some()
        || session.agent_path().is_some()
        || session.source() == Some("subagent")
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

fn scope_matches<T: SessionScope>(session: &T, event: &ExternalEvent) -> bool {
    // Harness is optional source metadata, not a source-specific branch. Any
    // event producer can constrain its observations to one harness while the
    // core remains agnostic to config/git event kinds. ProviderId's own
    // string form is the harness label, so no per-provider branching is
    // needed here (and unknown providers compare correctly by construction).
    if let Some(harness) = event.metadata.get("harness") {
        if harness != session.harness().as_str() {
            return false;
        }
    }
    let Some(event_scope) = event.scope.as_deref() else {
        return true;
    };
    let Some(cwd) = session.working_directory() else {
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
    *target += value;
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
    let harness_buckets = out
        .buckets_by_harness
        .entry(session.harness.clone())
        .or_default();
    for bucket in &range.buckets {
        add_bucket(harness_buckets, bucket);
    }
    out.tool_metrics.add_assign(&range.tool_metrics);
}

/// One event's (before, after) window, each an inclusive `[from, to]` bound.
type EventWindow = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Computes each event's (before, after) window pair from the query's
/// `before_days`/`after_days`. Shared by [`correlate_at`] and
/// [`candidate_session_keys`] so both agree on exactly the same windows —
/// the pre-filter would be unsound if it computed these independently and
/// drifted even slightly from what the final pass uses.
fn event_windows(query: &CorrelationQuery) -> Vec<(EventWindow, EventWindow)> {
    query
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
        .collect()
}

/// Computes each event's confounding-event ids and whether it is excluded,
/// from the query's events and `exclude_confounded`. Shared for the same
/// reason as [`event_windows`].
fn event_exclusions(
    query: &CorrelationQuery,
    windows: &[(EventWindow, EventWindow)],
) -> (Vec<Vec<String>>, Vec<bool>) {
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
    (confounds, excluded)
}

/// Durable storage keys of resident summaries that could possibly
/// contribute to `query`'s result (issue #139 follow-up): `correlate_events`
/// has no session-id scoping of its own, so without this, every call would
/// load full content (turns, `tokens_history`) for the entire corpus from
/// the ledger, on a hot-ish endpoint (`ConfigTimeline.svelte` re-runs it on
/// every live session-store flush while its tab is open).
///
/// A session cannot contribute to any window it isn't [`scope_matches`]-ed
/// for or that is [excluded](event_exclusions) — that much `correlate_at`
/// already establishes. Beyond that: every token/tool event a session ever
/// records happens inside `[started_at, last_event_at]` (`last_event_at`
/// only ever advances forward to an event's own timestamp — see
/// `parser.rs`/`claude_parser.rs`'s `if timestamp > s.last_event_at`
/// updates; `started_at` is fixed once at session creation and never
/// revised on a later `session_meta`/resume). So a session whose
/// `[started_at, last_event_at]` span doesn't overlap a matched window
/// cannot have any event inside that window either — `range_totals_multi`
/// for it is provably all zero, and [`add_range`] would already skip it for
/// exactly that reason once loaded. Filtering it out here first, using only
/// the resident summary, reaches the identical answer without the ledger
/// read: this is a pre-filter that can only ever return a superset of the
/// sessions that end up contributing, never exclude one that would have.
///
/// `summaries` is `(key, &SessionSummary)` pairs, the natural shape of
/// iterating `AppState.sessions`.
pub fn candidate_session_keys<'a>(
    summaries: impl Iterator<Item = (&'a str, &'a SessionSummary)>,
    query: &CorrelationQuery,
) -> Vec<String> {
    let windows = event_windows(query);
    let (_, excluded) = event_exclusions(query, &windows);
    summaries
        .filter(|(_, summary)| query.include_subagents || !is_subagent(*summary))
        .filter(|(_, summary)| {
            query.events.iter().enumerate().any(|(index, event)| {
                if excluded[index] || !scope_matches(*summary, event) {
                    return false;
                }
                let (before, after) = windows[index];
                interval_overlaps(
                    summary.started_at,
                    summary.last_event_at,
                    before.0,
                    before.1,
                ) || interval_overlaps(summary.started_at, summary.last_event_at, after.0, after.1)
            })
        })
        .map(|(key, _)| key.to_owned())
        .collect()
}

pub fn correlate<S: Borrow<Session>>(sessions: &[S], query: CorrelationQuery) -> CorrelationResult {
    correlate_at(sessions, query, Utc::now())
}

fn correlate_at<S: Borrow<Session>>(
    sessions: &[S],
    query: CorrelationQuery,
    now: DateTime<Utc>,
) -> CorrelationResult {
    let windows = event_windows(&query);
    let (confounds, excluded) = event_exclusions(&query, &windows);
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
        // `as_chunks::<2>()` rather than `chunks_exact(2)`: the pair count
        // is a compile-time constant here (each matched event contributes
        // exactly its before and after window), so this yields `&[_; 2]` and
        // the indexing below cannot panic. Required by clippy 1.98's
        // `chunks_exact_to_as_chunks`.
        let totals = session.range_totals_multi(&requested);
        for (position, pair) in totals.as_chunks::<2>().0.iter().enumerate() {
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
        let after_window_end = windows[index].1 .1.expect("bounded window");
        let after_window_complete = now >= after_window_end;
        let sample_ready = before.session_count >= MINIMUM_SESSION_COUNT
            && after.session_count >= MINIMUM_SESSION_COUNT;
        if !sample_ready {
            warnings.push("low sample size; do not interpret the delta as causal".into());
        }
        if !confounding_event_ids.is_empty() && !query.exclude_confounded {
            warnings.push("overlapping events may confound this comparison".into());
        }
        results.push(EventCorrelation {
            event,
            after_window_end,
            after_window_complete,
            minimum_session_count: MINIMUM_SESSION_COUNT,
            sample_ready,
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
    use crate::model::TokenHistoryPoint;
    use crate::provider::{claude_code_provider_id, codex_provider_id};
    use std::collections::{BTreeMap, HashMap};

    fn session(id: &str, cwd: Option<&str>, points: &[(&str, u64)], subagent: bool) -> Session {
        let history: Vec<_> = points
            .iter()
            .map(|(timestamp, count)| TokenHistoryPoint {
                timestamp: timestamp.parse().unwrap(),
                model: Some("m".into()),
                service_tier: None,
                request_input_tokens: Some(*count),
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
            storage_id: format!("codex:thread:{id}"),
            harness: codex_provider_id(),
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: subagent.then(|| "parent".into()),
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            source_availability: Default::default(),
            archived: false,
            started_at: points.first().unwrap().0.parse().unwrap(),
            last_event_at: points.last().unwrap().0.parse().unwrap(),
            working_directory: cwd.map(str::to_owned),
            originator: None,
            source: None,
            subagent_id_is_path_fallback: false,
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
        assert!(!result.results[0].sample_ready);
        assert_eq!(result.results[0].minimum_session_count, 3);
    }

    #[test]
    fn reports_when_the_after_window_is_still_collecting_data() {
        let event_time = "2026-01-02T00:00:00Z";
        let result = correlate_at(
            &[] as &[Session],
            CorrelationQuery {
                events: vec![event("e", event_time, None)],
                before_days: 7,
                after_days: 7,
                exclude_confounded: false,
                include_subagents: true,
            },
            "2026-01-03T00:00:00Z".parse().unwrap(),
        );
        let comparison = &result.results[0];
        assert_eq!(
            comparison.after_window_end,
            "2026-01-09T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert!(!comparison.after_window_complete);
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
                claude.harness = claude_code_provider_id();
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

    // -- candidate_session_keys pre-filter (issue #139 follow-up) ---------

    fn candidate_ids(sessions: &[Session], query: &CorrelationQuery) -> Vec<String> {
        let summaries: Vec<(String, SessionSummary)> = sessions
            .iter()
            .map(|s| (s.effective_storage_id(), SessionSummary::of(s)))
            .collect();
        let mut ids = candidate_session_keys(
            summaries
                .iter()
                .map(|(key, summary)| (key.as_str(), summary)),
            query,
        );
        ids.sort();
        ids
    }

    /// Runs the exact two-stage pipeline `commands::correlate_events` uses:
    /// compute candidates from summaries alone, then `correlate` only over
    /// the sessions that made the cut.
    fn correlate_via_prefilter(sessions: &[Session], query: CorrelationQuery) -> CorrelationResult {
        let ids = candidate_ids(sessions, &query);
        let filtered: Vec<&Session> = sessions
            .iter()
            .filter(|s| ids.contains(&s.effective_storage_id()))
            .collect();
        correlate(&filtered, query)
    }

    #[test]
    fn candidate_session_keys_excludes_sessions_whose_span_never_overlaps_any_window() {
        let overlapping = session(
            "overlapping",
            None,
            &[("2026-01-01T10:00:00Z", 5), ("2026-01-01T12:00:00Z", 5)],
            false,
        );
        let far_future = session(
            "far-future",
            None,
            &[("2026-02-01T00:00:00Z", 9), ("2026-02-02T00:00:00Z", 9)],
            false,
        );
        let far_past = session(
            "far-past",
            None,
            &[("2025-11-01T00:00:00Z", 9), ("2025-11-02T00:00:00Z", 9)],
            false,
        );
        let query = CorrelationQuery {
            events: vec![event("e", "2026-01-02T00:00:00Z", None)],
            before_days: 1,
            after_days: 1,
            exclude_confounded: false,
            include_subagents: true,
        };
        let sessions = vec![overlapping, far_future, far_past];
        assert_eq!(
            candidate_ids(&sessions, &query),
            vec!["codex:thread:overlapping"]
        );
    }

    #[test]
    fn candidate_session_keys_includes_sessions_overlapping_either_half_of_the_window() {
        let overlaps_before_only =
            session("before-only", None, &[("2026-01-01T10:00:00Z", 5)], false);
        let overlaps_after_only =
            session("after-only", None, &[("2026-01-02T18:00:00Z", 5)], false);
        let query = CorrelationQuery {
            events: vec![event("e", "2026-01-02T00:00:00Z", None)],
            before_days: 1,
            after_days: 1,
            exclude_confounded: false,
            include_subagents: true,
        };
        let sessions = vec![overlaps_before_only, overlaps_after_only];
        assert_eq!(
            candidate_ids(&sessions, &query),
            vec!["codex:thread:after-only", "codex:thread:before-only"]
        );
    }

    #[test]
    fn candidate_session_keys_still_applies_scope_and_subagent_filters() {
        let wrong_scope = session(
            "wrong-scope",
            Some("C:/other"),
            &[("2026-01-02T06:00:00Z", 5)],
            false,
        );
        let matching_scope = session(
            "matching-scope",
            Some("C:/repo"),
            &[("2026-01-02T06:00:00Z", 5)],
            false,
        );
        let excluded_subagent = session("subagent", None, &[("2026-01-02T06:00:00Z", 5)], true);
        let query = CorrelationQuery {
            events: vec![event("e", "2026-01-02T00:00:00Z", Some("C:/repo"))],
            before_days: 1,
            after_days: 1,
            exclude_confounded: false,
            include_subagents: false,
        };
        let sessions = vec![wrong_scope, matching_scope, excluded_subagent];
        assert_eq!(
            candidate_ids(&sessions, &query),
            vec!["codex:thread:matching-scope"]
        );
    }

    /// The soundness proof the pre-filter's doc comment claims: running
    /// `correlate` through the two-stage (summary pre-filter, then load and
    /// correlate only the candidates) pipeline must produce a byte-for-byte
    /// identical `CorrelationResult` to running `correlate` directly over
    /// every session, for a corpus that exercises every way a session can
    /// be excluded from candidacy — out of time range entirely, in range but
    /// wrong scope, in range but a subagent — alongside sessions that
    /// genuinely contribute to two separate, non-confounding events (proving
    /// the pre-filter's per-event `.any()` unions candidates correctly
    /// rather than only working for a single event).
    #[test]
    fn prefiltering_by_time_overlap_does_not_change_the_correlation_result() {
        let sessions = vec![
            session(
                "contributes-before-e1",
                Some("C:/repo"),
                &[("2026-01-01T08:00:00Z", 7), ("2026-01-01T20:00:00Z", 3)],
                false,
            ),
            session(
                "contributes-after-e1",
                Some("C:/repo"),
                &[("2026-01-02T02:00:00Z", 11)],
                false,
            ),
            // Overlaps only e2's window, not e1's — proves candidacy is a
            // union across events, not just whichever event is checked first.
            session(
                "contributes-e2-only",
                None,
                &[("2026-01-10T06:00:00Z", 17)],
                false,
            ),
            session(
                "out-of-range",
                Some("C:/repo"),
                &[("2026-03-01T00:00:00Z", 40)],
                false,
            ),
            session(
                "wrong-scope-in-range",
                Some("C:/unrelated"),
                &[("2026-01-02T04:00:00Z", 25)],
                false,
            ),
            session(
                "subagent-in-range",
                Some("C:/repo"),
                &[("2026-01-02T05:00:00Z", 6)],
                true,
            ),
        ];
        let query = CorrelationQuery {
            events: vec![
                event("e1", "2026-01-02T00:00:00Z", Some("C:/repo")),
                // Far enough from e1 that neither confounds the other —
                // this test is about candidacy, not confound exclusion
                // (covered separately by `flags_and_can_exclude_confounded_low_samples`).
                event("e2", "2026-01-10T00:00:00Z", None),
            ],
            before_days: 1,
            after_days: 1,
            exclude_confounded: true,
            include_subagents: false,
        };

        let direct = correlate(&sessions, query.clone());
        let via_prefilter = correlate_via_prefilter(&sessions, query.clone());
        assert_eq!(direct, via_prefilter);
        // Not a vacuous comparison: confirm the prefilter actually narrowed
        // the candidate set below the full corpus, and that real data
        // survived for both events.
        assert!(candidate_ids(&sessions, &query).len() < sessions.len());
        assert_ne!(direct.results[0].before, CorrelationObservation::default());
        assert_ne!(direct.results[1].after, CorrelationObservation::default());
    }
}
