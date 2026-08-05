//! Provider-agnostic live quota windows, pace/forecast math, and soft-budget
//! alerting (issue #43).
//!
//! ## Scope and the network seam
//!
//! `AGENTS.md` forbids adding outbound network access, an HTTP client, or a
//! new Tauri capability without an explicit requirement and a security
//! review; that review has not happened. This module is therefore built
//! entirely from data Odometer already has locally: each provider's
//! transcript-reported `rate_limits_history` / `plan_type` / `credits_*`
//! fields (see `crate::model::Session`). `QuotaProvenance::TranscriptDerived`
//! is the only provenance any snapshot carries today.
//!
//! `QuotaProvenance::LiveProvider` exists in the type now so a future,
//! reviewed live-polling source can slot in without changing every
//! consumer's shape: it would construct a `QuotaSnapshot` the same way this
//! module does, just from a network response instead of `Session` history,
//! and set that variant plus the reserved `ProviderOutage` / `AuthExpired` /
//! `RateLimited` / `Offline` unavailable reasons this module never produces
//! itself (see the honesty tests at the bottom of this file, which construct
//! those states synthetically to prove the *type* never collapses them to
//! zero usage — the actual polling implementation is out of scope here).
//!
//! ## Honesty contract
//!
//! A `QuotaWindow` never reports `used`/`remaining` as zero when the real
//! answer is "unknown". Every window that lacks real data sets
//! `unavailable: Some(reason)` and leaves the numeric fields `None`. A window
//! that has data but the data is old sets `stale: true` while still
//! reporting the (unrefreshed) numbers, because "we haven't seen anything
//! newer" is a different, less severe fact than "we know nothing".
//!
//! ## One service, not per-surface math
//!
//! `quota_snapshots_from_sessions` and `evaluate_alerts` are the only places
//! pace, projected exhaustion, reserve/deficit, and budget-crossing logic
//! live. The dashboard, tray, and any future surface consume the resulting
//! `QuotaSnapshot`/`QuotaAlert` values and only format them.

use crate::model::{RateLimitSnapshotPoint, RateLimitWindow, Session};
use crate::provider::ProviderId;
use crate::quota_store::{BudgetUnit, NotificationLogEntry, NotificationSettings, QuotaBudget};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::HashMap;

/// Caps how many rate-limit observations are merged per provider so a
/// pathological corpus (thousands of sessions, each with a long history)
/// cannot make quota aggregation unbounded. Recent observations are kept.
const MAX_POINTS_PER_PROVIDER: usize = 2_000;

/// A within-window used-percent drop at least this large between two
/// consecutive observations is treated as an *observed* window rollover
/// (the provider's counter visibly reset) rather than ordinary usage.
const ROLLOVER_DROP_THRESHOLD_PERCENT: f64 = 15.0;

/// Minimum number of in-window observations before a forecast is produced.
/// Below this, a linear projection is treated as noise rather than signal.
const MIN_EVIDENCE_POINTS: usize = 5;

/// Minimum fraction of the window's total duration that the evidence must
/// span. Five observations captured in the first 90 seconds of a 5-hour
/// window are still `MIN_EVIDENCE_POINTS` worth of *count*, but they carry
/// almost no information about the window's actual burn rate — a burst of
/// calls at session start is not a reliable predictor of the next 5 hours.
/// Both thresholds must hold before a forecast is produced.
const MIN_EVIDENCE_SPAN_FRACTION: f64 = 0.10;

const WEEKLY_MINUTES: i64 = 10_080;
const WEEKLY_TOLERANCE_MINUTES: i64 = 30;
const MONTHLY_MINUTES: i64 = 43_200;
const MONTHLY_TOLERANCE_MINUTES: i64 = 120;
const DAILY_MINUTES: i64 = 1_440;
const DAILY_TOLERANCE_MINUTES: i64 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaWindowKind {
    Burst,
    Daily,
    Weekly,
    Monthly,
    CreditBalance,
    Other,
}

impl QuotaWindowKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Burst => "burst",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::CreditBalance => "credit_balance",
            Self::Other => "other",
        }
    }
}

fn classify_window_kind(window_minutes: Option<u64>) -> QuotaWindowKind {
    let Some(minutes) = window_minutes else {
        return QuotaWindowKind::Other;
    };
    let minutes = minutes as i64;
    if (minutes - WEEKLY_MINUTES).abs() <= WEEKLY_TOLERANCE_MINUTES {
        QuotaWindowKind::Weekly
    } else if (minutes - MONTHLY_MINUTES).abs() <= MONTHLY_TOLERANCE_MINUTES {
        QuotaWindowKind::Monthly
    } else if (minutes - DAILY_MINUTES).abs() <= DAILY_TOLERANCE_MINUTES {
        QuotaWindowKind::Daily
    } else {
        QuotaWindowKind::Burst
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaUnit {
    Percent,
    Credits,
}

/// Where a `QuotaSnapshot`'s numbers came from. Never coerced together: a
/// dashboard/tray surface must always be able to tell a transcript-derived
/// reading from a (currently unimplemented) live one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaProvenance {
    TranscriptDerived,
    /// Reserved for a future, reviewed live-polling source. Nothing in this
    /// build produces this value; see the module doc's network-seam note.
    LiveProvider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaConfidence {
    High,
    Medium,
    Low,
}

fn downgrade(current: QuotaConfidence, cap: QuotaConfidence) -> QuotaConfidence {
    fn rank(c: QuotaConfidence) -> u8 {
        match c {
            QuotaConfidence::High => 2,
            QuotaConfidence::Medium => 1,
            QuotaConfidence::Low => 0,
        }
    }
    if rank(cap) < rank(current) {
        cap
    } else {
        current
    }
}

/// Why a window (or an entire provider) has no usable number right now.
/// `NoQuotaSource` and `NoObservation` are the only reasons the
/// transcript-derived path in this module ever produces; the rest exist so
/// a future live source has somewhere honest to report outages, expired
/// auth, being rate-limited itself, and being offline — and so every
/// consumer (dashboard, tray, alerts) already has to handle "no number"
/// rather than assuming only today's two reasons are possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaUnavailableReason {
    /// This provider has no quota-capable source at all (see
    /// `ProviderCapabilities::quota_source`).
    NoQuotaSource,
    /// The provider is quota-capable, but nothing has been observed yet
    /// (fresh install, offline startup before any transcript activity).
    NoObservation,
    /// The observation's timestamp is in the future relative to the local
    /// clock (system clock skew, or a corrected/replayed timestamp). Elapsed-
    /// time math (staleness, pace, forecast) is not trustworthy here.
    ClockSkew,
    /// Reserved for a future live source: the provider's quota API was
    /// unreachable or returned a server error.
    ProviderOutage,
    /// Reserved for a future live source: stored credentials were rejected
    /// or have expired.
    AuthExpired,
    /// Reserved for a future live source: the quota-polling request was
    /// itself rate-limited.
    RateLimited,
    /// Reserved for a future live source: no network path was available.
    Offline,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaForecast {
    /// Percentage points of the window consumed per hour, from a linear fit
    /// over the in-window evidence.
    pub pace_per_hour: f64,
    /// When usage would reach 100% at the current pace, only if that is
    /// before the window's own reset (a projection past the reset is
    /// meaningless — the window resets for free before exhaustion).
    pub projected_exhaustion_at: Option<DateTime<Utc>>,
    /// `used_percent - even_pace_percent`, where `even_pace_percent` is the
    /// fraction of the window elapsed so far times 100. Positive means
    /// burning faster than a flat, reset-timed pace (a deficit / at risk of
    /// early exhaustion); negative means a reserve/cushion.
    pub reserve_deficit_percent: f64,
    /// How many in-window observations backed this forecast, so a consumer
    /// can show "estimated from N observations" rather than a bare number.
    pub evidence_points: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QuotaWindow {
    pub kind: QuotaWindowKind,
    pub unit: QuotaUnit,
    pub window_minutes: Option<u64>,
    /// 0-100 for `Percent`; credit units for `Credits`. `None` exactly when
    /// `unavailable` is `Some` or (for `Credits`) the plan is unlimited.
    pub used: Option<f64>,
    pub remaining: Option<f64>,
    /// `None` for `Credits` unless the plan declares a hard cap (it never
    /// does today); `Some(100.0)` for `Percent`.
    pub limit: Option<f64>,
    /// True only for a `Credits` window on a plan the provider reports as
    /// unlimited. `used`/`remaining` stay `None` — "unlimited" is a known
    /// state, not a number, and must never render as `0 used`.
    pub unlimited: bool,
    pub resets_at: Option<DateTime<Utc>>,
    pub window_started_at: Option<DateTime<Utc>>,
    /// True when `window_started_at` was inferred by subtracting
    /// `window_minutes` from `resets_at` rather than observed directly as a
    /// used-percent rollover in the data. Per issue #43's carried-over
    /// #92 comment: that subtraction is correct for a fixed window and
    /// wrong for a rolling one, and Odometer cannot tell which kind a given
    /// provider window is from the data alone — so an estimated value is
    /// flagged rather than presented with the same confidence as an
    /// observed one.
    pub window_started_at_estimated: bool,
    /// Timestamp of the observation this window's numbers came from.
    pub observed_at: DateTime<Utc>,
    pub confidence: QuotaConfidence,
    /// True when `observed_at` is older than the configured max cache age.
    /// The numbers are still shown (see module doc): a stale reading is a
    /// different, more honest fact than "no reading at all".
    pub stale: bool,
    pub unavailable: Option<QuotaUnavailableReason>,
    pub forecast: Option<QuotaForecast>,
}

#[derive(Debug, Clone, Serialize)]
pub struct QuotaSnapshot {
    pub provider: ProviderId,
    pub provenance: QuotaProvenance,
    pub windows: Vec<QuotaWindow>,
    /// Set only when `windows` is empty — either the provider has no quota
    /// source at all, or it does but nothing has ever been observed. A
    /// window-specific problem (stale, clock skew) lives on that window
    /// instead, so a provider with one healthy window and one broken one is
    /// never forced into a single top-level verdict.
    pub unavailable: Option<QuotaUnavailableReason>,
}

#[derive(Debug, Clone, Default)]
pub struct QuotaAccountInfo {
    pub credits_unlimited: Option<bool>,
    pub credits_balance: Option<f64>,
    pub observed_at: Option<DateTime<Utc>>,
}

fn detect_observed_rollover(series: &[(DateTime<Utc>, RateLimitWindow)]) -> Option<DateTime<Utc>> {
    for pair in series.windows(2).rev() {
        let (_, prev) = &pair[0];
        let (cur_ts, cur) = &pair[1];
        if prev.used_percent - cur.used_percent >= ROLLOVER_DROP_THRESHOLD_PERCENT {
            return Some(*cur_ts);
        }
    }
    None
}

fn forecast_from_series(
    series: &[(DateTime<Utc>, RateLimitWindow)],
    window_started_at: DateTime<Utc>,
    window_minutes: Option<u64>,
    resets_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Option<QuotaForecast> {
    let minutes = window_minutes? as f64;
    if minutes <= 0.0 {
        return None;
    }
    let in_window: Vec<&(DateTime<Utc>, RateLimitWindow)> = series
        .iter()
        .filter(|(ts, _)| *ts >= window_started_at && *ts <= now)
        .collect();
    if in_window.len() < MIN_EVIDENCE_POINTS {
        return None;
    }
    let first = in_window.first().unwrap();
    let last = in_window.last().unwrap();
    let span_minutes = last.0.signed_duration_since(first.0).num_seconds() as f64 / 60.0;
    if span_minutes / minutes < MIN_EVIDENCE_SPAN_FRACTION {
        return None;
    }
    let elapsed_hours = span_minutes / 60.0;
    if elapsed_hours <= 0.0 {
        return None;
    }
    let used_delta = last.1.used_percent - first.1.used_percent;
    if used_delta < 0.0 {
        // A decrease inside what we believe is one window means our window
        // boundary is wrong (a rollover we didn't detect, or a corrected
        // reading). Extrapolating a negative pace is not meaningful, so
        // suppress rather than project usage trending toward zero forever.
        return None;
    }
    let pace_per_hour = used_delta / elapsed_hours;

    let projected_exhaustion_at = if pace_per_hour > 0.0 {
        let remaining = (100.0 - last.1.used_percent).max(0.0);
        let hours_to_exhaustion = remaining / pace_per_hour;
        let candidate =
            now + Duration::milliseconds((hours_to_exhaustion * 3_600_000.0).round() as i64);
        match resets_at {
            Some(resets_at) if candidate >= resets_at => None,
            _ => Some(candidate),
        }
    } else {
        None
    };

    let elapsed_fraction = (now.signed_duration_since(window_started_at).num_seconds() as f64
        / (minutes * 60.0))
        .clamp(0.0, 1.0);
    let even_pace_used = elapsed_fraction * 100.0;
    let reserve_deficit_percent = last.1.used_percent - even_pace_used;

    Some(QuotaForecast {
        pace_per_hour,
        projected_exhaustion_at,
        reserve_deficit_percent,
        evidence_points: in_window.len(),
    })
}

fn build_percent_window(
    series: &[(DateTime<Utc>, RateLimitWindow)],
    now: DateTime<Utc>,
    max_cache_age: Duration,
) -> Option<QuotaWindow> {
    let (latest_ts, latest) = series.last()?;
    let kind = classify_window_kind(latest.window_minutes);
    let mut window = QuotaWindow {
        kind,
        unit: QuotaUnit::Percent,
        window_minutes: latest.window_minutes,
        used: None,
        remaining: None,
        limit: Some(100.0),
        unlimited: false,
        resets_at: latest.resets_at,
        window_started_at: None,
        window_started_at_estimated: false,
        observed_at: *latest_ts,
        confidence: QuotaConfidence::High,
        stale: false,
        unavailable: None,
        forecast: None,
    };

    if *latest_ts > now {
        window.unavailable = Some(QuotaUnavailableReason::ClockSkew);
        window.confidence = QuotaConfidence::Low;
        return Some(window);
    }

    let age = now.signed_duration_since(*latest_ts);
    window.stale = age > max_cache_age;
    if window.stale {
        window.confidence = downgrade(window.confidence, QuotaConfidence::Low);
    }

    window.used = Some(latest.used_percent.clamp(0.0, 100.0));
    window.remaining = Some((100.0 - latest.used_percent).clamp(0.0, 100.0));

    if let Some(rollover_at) = detect_observed_rollover(series) {
        window.window_started_at = Some(rollover_at);
        window.window_started_at_estimated = false;
    } else if let (Some(resets_at), Some(minutes)) = (latest.resets_at, latest.window_minutes) {
        window.window_started_at = Some(resets_at - Duration::minutes(minutes as i64));
        window.window_started_at_estimated = true;
        window.confidence = downgrade(window.confidence, QuotaConfidence::Medium);
    }

    if !window.stale {
        if let Some(window_started_at) = window.window_started_at {
            window.forecast = forecast_from_series(
                series,
                window_started_at,
                window.window_minutes,
                window.resets_at,
                now,
            );
        }
    }

    Some(window)
}

fn build_credit_window(
    account: &QuotaAccountInfo,
    now: DateTime<Utc>,
    max_cache_age: Duration,
) -> Option<QuotaWindow> {
    let unlimited = account.credits_unlimited.unwrap_or(false);
    if !unlimited && account.credits_balance.is_none() {
        return None;
    }
    let observed_at = account.observed_at.unwrap_or(now);
    let mut window = QuotaWindow {
        kind: QuotaWindowKind::CreditBalance,
        unit: QuotaUnit::Credits,
        window_minutes: None,
        used: None,
        remaining: if unlimited {
            None
        } else {
            account.credits_balance
        },
        limit: None,
        unlimited,
        resets_at: None,
        window_started_at: None,
        window_started_at_estimated: false,
        observed_at,
        confidence: QuotaConfidence::High,
        stale: false,
        unavailable: None,
        forecast: None,
    };

    if observed_at > now {
        window.unavailable = Some(QuotaUnavailableReason::ClockSkew);
        window.confidence = QuotaConfidence::Low;
        return Some(window);
    }

    let age = now.signed_duration_since(observed_at);
    window.stale = age > max_cache_age;
    if window.stale {
        window.confidence = downgrade(window.confidence, QuotaConfidence::Low);
    }
    Some(window)
}

/// Builds one provider's quota snapshot from transcript-derived evidence.
/// Pure and deterministic given `now`, so it is exhaustively unit testable.
///
/// `points` must be sorted ascending by timestamp; callers (see
/// `quota_snapshots_from_sessions`) are responsible for merging and sorting
/// across every session for the harness before calling this.
pub fn build_quota_snapshot(
    provider: ProviderId,
    source_supported: bool,
    points: &[RateLimitSnapshotPoint],
    account: QuotaAccountInfo,
    now: DateTime<Utc>,
    max_cache_age: Duration,
) -> QuotaSnapshot {
    if !source_supported {
        return QuotaSnapshot {
            provider,
            provenance: QuotaProvenance::TranscriptDerived,
            windows: Vec::new(),
            unavailable: Some(QuotaUnavailableReason::NoQuotaSource),
        };
    }

    let primary_series: Vec<(DateTime<Utc>, RateLimitWindow)> = points
        .iter()
        .filter_map(|p| p.primary.clone().map(|w| (p.timestamp, w)))
        .collect();
    let secondary_series: Vec<(DateTime<Utc>, RateLimitWindow)> = points
        .iter()
        .filter_map(|p| p.secondary.clone().map(|w| (p.timestamp, w)))
        .collect();

    let mut windows = Vec::new();
    if let Some(w) = build_percent_window(&primary_series, now, max_cache_age) {
        windows.push(w);
    }
    if let Some(w) = build_percent_window(&secondary_series, now, max_cache_age) {
        windows.push(w);
    }
    if let Some(w) = build_credit_window(&account, now, max_cache_age) {
        windows.push(w);
    }

    let unavailable = if windows.is_empty() {
        Some(QuotaUnavailableReason::NoObservation)
    } else {
        None
    };

    QuotaSnapshot {
        provider,
        provenance: QuotaProvenance::TranscriptDerived,
        windows,
        unavailable,
    }
}

/// Aggregates every registered provider's quota snapshot from the current
/// in-memory session projection. This is the one place account-wide
/// rate-limit history is merged across sessions for quota purposes; nothing
/// else in the app should re-derive pace/forecast from raw sessions.
pub fn quota_snapshots_from_sessions<'a>(
    sessions: impl Iterator<Item = &'a Session>,
    now: DateTime<Utc>,
    max_cache_age: Duration,
) -> Vec<QuotaSnapshot> {
    let registry = crate::provider::ProviderRegistry::builtin();
    let mut points_by_provider: HashMap<ProviderId, Vec<RateLimitSnapshotPoint>> = HashMap::new();
    let mut account_by_provider: HashMap<ProviderId, QuotaAccountInfo> = HashMap::new();

    for session in sessions {
        points_by_provider
            .entry(session.harness.clone())
            .or_default()
            .extend(session.rate_limits_history.iter().cloned());

        if session.credits_unlimited.is_some() || session.credits_balance.is_some() {
            let candidate = QuotaAccountInfo {
                credits_unlimited: session.credits_unlimited,
                credits_balance: session.credits_balance,
                observed_at: Some(session.last_event_at),
            };
            let entry = account_by_provider.entry(session.harness.clone());
            match entry {
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    vacant.insert(candidate);
                }
                std::collections::hash_map::Entry::Occupied(mut occupied) => {
                    let newer = occupied
                        .get()
                        .observed_at
                        .is_none_or(|existing| candidate.observed_at.is_some_and(|c| c > existing));
                    if newer {
                        occupied.insert(candidate);
                    }
                }
            }
        }
    }

    registry
        .descriptors()
        .map(|descriptor| {
            let mut points = points_by_provider
                .remove(&descriptor.id)
                .unwrap_or_default();
            points.sort_by_key(|p| p.timestamp);
            if points.len() > MAX_POINTS_PER_PROVIDER {
                let excess = points.len() - MAX_POINTS_PER_PROVIDER;
                points.drain(0..excess);
            }
            let account = account_by_provider
                .remove(&descriptor.id)
                .unwrap_or_default();
            build_quota_snapshot(
                descriptor.id.clone(),
                descriptor.capabilities.quota_source,
                &points,
                account,
                now,
                max_cache_age,
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Soft-budget alerting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QuotaAlert {
    pub budget_id: String,
    pub provider: ProviderId,
    pub project_key: Option<String>,
    pub message: String,
    pub current_value: f64,
    pub threshold: f64,
    pub fired_at: DateTime<Utc>,
}

/// One budget paired with its freshly computed current value, ready for
/// `evaluate_alerts`. `current_value` is `None` when the budget's window or
/// project has no data at all right now (an absent value never counts as a
/// crossing, and never re-arms one either).
pub struct BudgetEvaluation<'a> {
    pub budget: &'a QuotaBudget,
    pub current_value: Option<f64>,
}

fn in_quiet_hours(range: Option<(u8, u8)>, local_hour: u8) -> bool {
    let Some((start, end)) = range else {
        return false;
    };
    if start == end {
        return false; // degenerate range: treat as "no quiet hours"
    }
    if start < end {
        local_hour >= start && local_hour < end
    } else {
        // Wraps past midnight, e.g. (22, 7).
        local_hour >= start || local_hour < end
    }
}

fn alert_message(budget: &QuotaBudget, value: f64) -> String {
    match budget.unit {
        BudgetUnit::PercentOfWindow => format!(
            "{} is at {:.0}% of its {} soft budget ({:.0}%).",
            budget.provider,
            value,
            budget.window_kind.as_deref().unwrap_or("quota"),
            budget.threshold
        ),
        BudgetUnit::Tokens => format!(
            "{} used {:.0} tokens against a {}-hour soft budget of {:.0}.",
            budget
                .project_key
                .as_deref()
                .unwrap_or(budget.provider.as_str()),
            value,
            budget.period_hours.unwrap_or(24),
            budget.threshold
        ),
    }
}

/// Edge-triggered, dedup, reset-aware soft-budget alerting. Deterministic
/// given its inputs, so it is the one implementation every surface (today:
/// the `check_quota_alerts` command; tomorrow: a CLI/statusline) shares.
///
/// A budget "arms" (logged, alert surfaced once — if notifications are on
/// and it's not quiet hours) the first time its value reaches the
/// threshold, and "re-arms" (log entry removed, so the next crossing can
/// fire again) the moment its value drops back under the threshold. This
/// is what makes the design reset-aware without needing to know each
/// budget's reset time: a provider window resetting drives `used` back
/// down, which re-arms it naturally; a rolling token budget re-arms when
/// usage falls back under the cap.
///
/// Crossings are always recorded in the returned log (even when
/// notifications are globally disabled or it is quiet hours), so toggling
/// notifications on afterward — or sleep/resume, or calling this many times
/// in a row for the same still-crossed budget — never produces a backlog
/// storm of alerts for a crossing that already happened.
pub fn evaluate_alerts(
    evaluations: &[BudgetEvaluation<'_>],
    settings: &NotificationSettings,
    log: &[NotificationLogEntry],
    now: DateTime<Utc>,
    local_hour: u8,
) -> (Vec<QuotaAlert>, Vec<NotificationLogEntry>) {
    let mut log_by_key: std::collections::BTreeMap<String, NotificationLogEntry> = log
        .iter()
        .cloned()
        .map(|entry| (entry.dedup_key.clone(), entry))
        .collect();
    let mut alerts = Vec::new();
    let quiet = in_quiet_hours(settings.quiet_hours, local_hour);

    for evaluation in evaluations {
        if !evaluation.budget.enabled {
            continue;
        }
        let Some(value) = evaluation.current_value else {
            continue;
        };
        let key = evaluation.budget.id.clone();
        let armed = log_by_key.contains_key(&key);

        if value < evaluation.budget.threshold {
            log_by_key.remove(&key);
            continue;
        }
        if armed {
            continue;
        }
        log_by_key.insert(
            key.clone(),
            NotificationLogEntry {
                dedup_key: key,
                fired_at: now,
            },
        );
        if settings.enabled && !quiet {
            alerts.push(QuotaAlert {
                budget_id: evaluation.budget.id.clone(),
                provider: evaluation.budget.provider.clone(),
                project_key: evaluation.budget.project_key.clone(),
                message: alert_message(evaluation.budget, value),
                current_value: value,
                threshold: evaluation.budget.threshold,
                fired_at: now,
            });
        }
    }

    (alerts, log_by_key.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::codex_provider_id;
    use crate::quota_store::QuotaBudget;

    fn ts(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn point(
        timestamp: &str,
        used_percent: f64,
        window_minutes: u64,
        resets_at: &str,
    ) -> RateLimitSnapshotPoint {
        RateLimitSnapshotPoint {
            timestamp: ts(timestamp),
            turn_id: None,
            limit_id: None,
            primary: Some(RateLimitWindow {
                used_percent,
                window_minutes: Some(window_minutes),
                resets_at: Some(ts(resets_at)),
            }),
            secondary: None,
        }
    }

    const MAX_AGE: fn() -> Duration = || Duration::hours(6);

    // -- Honesty: provider without quota-source capability -----------------

    #[test]
    fn provider_without_quota_capability_is_honestly_unavailable_not_zero() {
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            false,
            &[],
            QuotaAccountInfo::default(),
            Utc::now(),
            MAX_AGE(),
        );
        assert!(snapshot.windows.is_empty());
        assert_eq!(
            snapshot.unavailable,
            Some(QuotaUnavailableReason::NoQuotaSource)
        );
    }

    // -- Honesty: offline startup / nothing observed yet --------------------

    #[test]
    fn no_observation_yet_is_honestly_unavailable_not_zero() {
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &[],
            QuotaAccountInfo::default(),
            Utc::now(),
            MAX_AGE(),
        );
        assert!(snapshot.windows.is_empty());
        assert_eq!(
            snapshot.unavailable,
            Some(QuotaUnavailableReason::NoObservation)
        );
    }

    // -- Honesty: stale cache still shows numbers, flagged ------------------

    #[test]
    fn stale_cache_keeps_last_known_numbers_but_is_flagged() {
        let now = ts("2026-01-02T00:00:00Z");
        let points = [point(
            "2026-01-01T00:00:00Z",
            40.0,
            300,
            "2026-01-01T05:00:00Z",
        )];
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            Duration::hours(6),
        );
        let window = &snapshot.windows[0];
        assert!(window.stale);
        assert_eq!(window.used, Some(40.0));
        assert_eq!(
            window.unavailable, None,
            "stale numbers are not the same as no numbers"
        );
        assert_eq!(window.confidence, QuotaConfidence::Low);
    }

    #[test]
    fn fresh_observation_within_max_age_is_not_stale() {
        let now = ts("2026-01-01T01:00:00Z");
        let points = [point(
            "2026-01-01T00:30:00Z",
            10.0,
            300,
            "2026-01-01T05:00:00Z",
        )];
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            Duration::hours(6),
        );
        assert!(!snapshot.windows[0].stale);
    }

    // -- Honesty: sleep/resume (a large wall-clock jump) is just staleness --

    #[test]
    fn sleep_resume_reevaluation_reports_stale_not_a_fabricated_fresh_reading() {
        let points = [point(
            "2026-01-01T00:00:00Z",
            30.0,
            300,
            "2026-01-01T05:00:00Z",
        )];
        // The machine slept for 3 days; "now" jumps far ahead without any
        // new observation arriving in between.
        let resumed_now = ts("2026-01-01T00:00:00Z") + Duration::days(3);
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            resumed_now,
            Duration::hours(6),
        );
        let window = &snapshot.windows[0];
        assert!(window.stale);
        assert_eq!(
            window.used,
            Some(30.0),
            "still shows the last known reading"
        );
        assert!(
            window.forecast.is_none(),
            "never forecasts from a stale reading"
        );
    }

    // -- Honesty: clock skew --------------------------------------------------

    #[test]
    fn future_timestamped_observation_is_clock_skew_not_a_negative_age() {
        let now = ts("2026-01-01T00:00:00Z");
        let points = [point(
            "2026-01-01T01:00:00Z", // one hour in the future relative to `now`
            50.0,
            300,
            "2026-01-01T06:00:00Z",
        )];
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            MAX_AGE(),
        );
        let window = &snapshot.windows[0];
        assert_eq!(window.unavailable, Some(QuotaUnavailableReason::ClockSkew));
        assert_eq!(window.used, None, "never reports a number under clock skew");
    }

    // -- Honesty: reserved live-source reasons never collapse to zero -------

    #[test]
    fn reserved_live_source_reasons_never_render_as_zero_usage() {
        // These reasons are not produced by this build, but the type must
        // hold them without any consumer assuming `used`/`remaining` are
        // populated whenever `unavailable` is absent-from-today's-set.
        for reason in [
            QuotaUnavailableReason::ProviderOutage,
            QuotaUnavailableReason::AuthExpired,
            QuotaUnavailableReason::RateLimited,
            QuotaUnavailableReason::Offline,
        ] {
            let window = QuotaWindow {
                kind: QuotaWindowKind::Burst,
                unit: QuotaUnit::Percent,
                window_minutes: Some(300),
                used: None,
                remaining: None,
                limit: Some(100.0),
                unlimited: false,
                resets_at: None,
                window_started_at: None,
                window_started_at_estimated: false,
                observed_at: Utc::now(),
                confidence: QuotaConfidence::Low,
                stale: false,
                unavailable: Some(reason),
                forecast: None,
            };
            assert_eq!(window.used, None);
            assert_eq!(window.remaining, None);
            assert!(window.unavailable.is_some());
        }
    }

    // -- Multiple simultaneous windows, unlike units never coerced ----------

    #[test]
    fn credit_balance_and_percent_window_coexist_with_distinct_units() {
        let now = ts("2026-01-01T01:00:00Z");
        let points = [point(
            "2026-01-01T00:30:00Z",
            25.0,
            300,
            "2026-01-01T05:00:00Z",
        )];
        let account = QuotaAccountInfo {
            credits_unlimited: Some(false),
            credits_balance: Some(1234.5),
            observed_at: Some(now),
        };
        let snapshot =
            build_quota_snapshot(codex_provider_id(), true, &points, account, now, MAX_AGE());
        assert_eq!(snapshot.windows.len(), 2);
        let percent = snapshot
            .windows
            .iter()
            .find(|w| w.unit == QuotaUnit::Percent)
            .unwrap();
        let credits = snapshot
            .windows
            .iter()
            .find(|w| w.unit == QuotaUnit::Credits)
            .unwrap();
        assert_eq!(percent.used, Some(25.0));
        assert_eq!(credits.remaining, Some(1234.5));
        // Never summed or converted into one figure.
        assert_ne!(percent.used, credits.remaining);
    }

    #[test]
    fn unlimited_credits_never_reports_a_fabricated_zero_or_number() {
        let now = Utc::now();
        let account = QuotaAccountInfo {
            credits_unlimited: Some(true),
            credits_balance: None,
            observed_at: Some(now),
        };
        let snapshot =
            build_quota_snapshot(codex_provider_id(), true, &[], account, now, MAX_AGE());
        let window = snapshot
            .windows
            .iter()
            .find(|w| w.unit == QuotaUnit::Credits)
            .unwrap();
        assert!(window.unlimited);
        assert_eq!(window.used, None);
        assert_eq!(window.remaining, None);
        assert_eq!(
            window.unavailable, None,
            "unlimited is a known state, not an error"
        );
    }

    // -- Minimum-evidence rule: both sides --------------------------------

    fn evenly_spaced_points(
        start: &str,
        step_minutes: i64,
        count: usize,
        used_start: f64,
        used_step: f64,
        window_minutes: u64,
        resets_at: &str,
    ) -> Vec<RateLimitSnapshotPoint> {
        let start = ts(start);
        (0..count)
            .map(|i| RateLimitSnapshotPoint {
                timestamp: start + Duration::minutes(step_minutes * i as i64),
                turn_id: None,
                limit_id: None,
                primary: Some(RateLimitWindow {
                    used_percent: used_start + used_step * i as f64,
                    window_minutes: Some(window_minutes),
                    resets_at: Some(ts(resets_at)),
                }),
                secondary: None,
            })
            .collect()
    }

    #[test]
    fn forecast_is_suppressed_below_minimum_evidence_points() {
        // Only 4 points (< MIN_EVIDENCE_POINTS = 5), well within the span
        // requirement otherwise.
        let points = evenly_spaced_points(
            "2026-01-01T00:00:00Z",
            30,
            4,
            10.0,
            5.0,
            300,
            "2026-01-01T05:00:00Z",
        );
        let now = points.last().unwrap().timestamp;
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            MAX_AGE(),
        );
        assert!(
            snapshot.windows[0].forecast.is_none(),
            "fewer than the minimum evidence points must suppress a forecast"
        );
    }

    #[test]
    fn forecast_is_suppressed_when_evidence_span_is_too_narrow() {
        // 5 points (meets the count), but all within 4 minutes of a 300-
        // minute window — a burst at session start, not a pace signal.
        let points = evenly_spaced_points(
            "2026-01-01T00:00:00Z",
            1,
            5,
            10.0,
            0.2,
            300,
            "2026-01-01T05:00:00Z",
        );
        let now = points.last().unwrap().timestamp;
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            MAX_AGE(),
        );
        assert!(
            snapshot.windows[0].forecast.is_none(),
            "a too-narrow evidence span must suppress a forecast even with enough points"
        );
    }

    #[test]
    fn forecast_is_produced_once_both_evidence_thresholds_are_met() {
        // 6 points spanning 60 minutes of a 300-minute window (20% span,
        // above the 10% minimum), rising 5%/observation.
        let points = evenly_spaced_points(
            "2026-01-01T00:00:00Z",
            12,
            6,
            10.0,
            5.0,
            300,
            "2026-01-01T05:00:00Z",
        );
        let now = points.last().unwrap().timestamp;
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            MAX_AGE(),
        );
        let forecast = snapshot.windows[0]
            .forecast
            .as_ref()
            .expect("evidence thresholds are met; a forecast must be produced");
        assert_eq!(forecast.evidence_points, 6);
        assert!(forecast.pace_per_hour > 0.0);
    }

    #[test]
    fn projected_exhaustion_is_never_reported_past_the_windows_own_reset() {
        // Pace is slow enough that exhaustion would land after the window
        // resets; the window resets "for free" first, so no projection.
        let points = evenly_spaced_points(
            "2026-01-01T00:00:00Z",
            12,
            6,
            10.0,
            0.5,
            300,
            "2026-01-01T00:30:00Z", // resets very soon
        );
        let now = points.last().unwrap().timestamp;
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            MAX_AGE(),
        );
        let forecast = snapshot.windows[0].forecast.as_ref().unwrap();
        assert!(forecast.projected_exhaustion_at.is_none());
    }

    #[test]
    fn reserve_deficit_sign_reflects_pace_versus_even_burn() {
        // Burning ~5%/observation over 60 minutes inside a 300-minute
        // window that has only been open ~1h — much faster than an even
        // pace to the reset, so this should read as a deficit (positive).
        let points = evenly_spaced_points(
            "2026-01-01T00:00:00Z",
            12,
            6,
            10.0,
            5.0,
            300,
            "2026-01-01T05:00:00Z",
        );
        let now = points.last().unwrap().timestamp;
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            MAX_AGE(),
        );
        let forecast = snapshot.windows[0].forecast.as_ref().unwrap();
        assert!(
            forecast.reserve_deficit_percent > 0.0,
            "burning far faster than an even pace to reset should read as a deficit"
        );
    }

    // -- Observed vs. estimated window start ---------------------------------

    #[test]
    fn an_observed_rollover_is_high_confidence_not_estimated() {
        let points = [
            point("2026-01-01T00:00:00Z", 95.0, 300, "2026-01-01T00:05:00Z"),
            // Rolled over: used_percent dropped sharply.
            point("2026-01-01T00:10:00Z", 5.0, 300, "2026-01-01T05:10:00Z"),
        ];
        let now = ts("2026-01-01T00:10:00Z");
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            MAX_AGE(),
        );
        let window = &snapshot.windows[0];
        assert!(!window.window_started_at_estimated);
        assert_eq!(window.window_started_at, Some(ts("2026-01-01T00:10:00Z")));
    }

    #[test]
    fn without_an_observed_rollover_window_start_is_estimated_and_capped_at_medium() {
        let points = [point(
            "2026-01-01T00:00:00Z",
            40.0,
            300,
            "2026-01-01T05:00:00Z",
        )];
        let now = ts("2026-01-01T00:00:00Z");
        let snapshot = build_quota_snapshot(
            codex_provider_id(),
            true,
            &points,
            QuotaAccountInfo::default(),
            now,
            MAX_AGE(),
        );
        let window = &snapshot.windows[0];
        assert!(window.window_started_at_estimated);
        assert_eq!(window.confidence, QuotaConfidence::Medium);
    }

    // -- Multi-provider aggregation -----------------------------------------

    #[test]
    fn every_registered_provider_gets_a_snapshot_even_with_no_data() {
        let sessions: Vec<Session> = Vec::new();
        let snapshots =
            quota_snapshots_from_sessions(sessions.iter(), Utc::now(), Duration::hours(6));
        assert!(snapshots.iter().any(|s| s.provider == codex_provider_id()));
        assert!(snapshots
            .iter()
            .any(|s| s.provider == crate::provider::claude_code_provider_id()));
        // Claude Code has no quota_source capability today.
        let claude = snapshots
            .iter()
            .find(|s| s.provider == crate::provider::claude_code_provider_id())
            .unwrap();
        assert_eq!(
            claude.unavailable,
            Some(QuotaUnavailableReason::NoQuotaSource)
        );
    }

    // -- Alert engine: opt-in, dedup, reset-aware, quiet hours ---------------

    fn budget(id: &str, threshold: f64) -> QuotaBudget {
        QuotaBudget {
            id: id.to_string(),
            provider: codex_provider_id(),
            project_key: None,
            unit: BudgetUnit::PercentOfWindow,
            window_kind: Some("burst".to_string()),
            period_hours: None,
            threshold,
            enabled: true,
        }
    }

    fn enabled_settings() -> NotificationSettings {
        NotificationSettings {
            enabled: true,
            quiet_hours: None,
        }
    }

    #[test]
    fn alerts_never_fire_when_notifications_are_disabled() {
        let b = budget("b1", 80.0);
        let evaluations = [BudgetEvaluation {
            budget: &b,
            current_value: Some(90.0),
        }];
        let settings = NotificationSettings {
            enabled: false,
            quiet_hours: None,
        };
        let (alerts, log) = evaluate_alerts(&evaluations, &settings, &[], Utc::now(), 12);
        assert!(alerts.is_empty());
        // Still tracked so re-enabling later doesn't cause a backlog storm.
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn a_crossing_fires_exactly_once_until_it_resets() {
        let b = budget("b1", 80.0);
        let settings = enabled_settings();
        let evaluations = [BudgetEvaluation {
            budget: &b,
            current_value: Some(85.0),
        }];
        let (first_alerts, log_after_first) =
            evaluate_alerts(&evaluations, &settings, &[], Utc::now(), 12);
        assert_eq!(first_alerts.len(), 1);

        // Re-evaluating repeatedly at the same (still-crossed) value must
        // not fire again — this is the "no storm on resume from sleep"
        // case: many rapid re-checks after a long gap must not each fire.
        let (second_alerts, log_after_second) =
            evaluate_alerts(&evaluations, &settings, &log_after_first, Utc::now(), 12);
        assert!(second_alerts.is_empty());
        assert_eq!(log_after_second.len(), 1);

        // Dropping back under threshold (e.g. the provider window reset)
        // re-arms it.
        let reset_evaluations = [BudgetEvaluation {
            budget: &b,
            current_value: Some(5.0),
        }];
        let (reset_alerts, log_after_reset) = evaluate_alerts(
            &reset_evaluations,
            &settings,
            &log_after_second,
            Utc::now(),
            12,
        );
        assert!(reset_alerts.is_empty());
        assert!(log_after_reset.is_empty());

        // Crossing again after the reset fires again.
        let (third_alerts, _) =
            evaluate_alerts(&evaluations, &settings, &log_after_reset, Utc::now(), 12);
        assert_eq!(third_alerts.len(), 1);
    }

    #[test]
    fn quiet_hours_suppress_the_alert_but_still_record_the_crossing() {
        let b = budget("b1", 80.0);
        let settings = NotificationSettings {
            enabled: true,
            quiet_hours: Some((22, 7)), // wraps past midnight
        };
        let evaluations = [BudgetEvaluation {
            budget: &b,
            current_value: Some(90.0),
        }];
        let (alerts, log) = evaluate_alerts(&evaluations, &settings, &[], Utc::now(), 23);
        assert!(
            alerts.is_empty(),
            "23:00 is inside the (22, 7) quiet window"
        );
        assert_eq!(log.len(), 1, "the crossing is still recorded");

        // Outside quiet hours the same still-armed crossing does not
        // re-fire (it already "happened"), matching the storm-avoidance
        // contract above.
        let (later_alerts, _) = evaluate_alerts(&evaluations, &settings, &log, Utc::now(), 9);
        assert!(later_alerts.is_empty());
    }

    #[test]
    fn disabled_budget_never_arms_or_fires() {
        let mut b = budget("b1", 80.0);
        b.enabled = false;
        let settings = enabled_settings();
        let evaluations = [BudgetEvaluation {
            budget: &b,
            current_value: Some(95.0),
        }];
        let (alerts, log) = evaluate_alerts(&evaluations, &settings, &[], Utc::now(), 12);
        assert!(alerts.is_empty());
        assert!(log.is_empty());
    }
}
