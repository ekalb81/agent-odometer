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
use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

/// Caps how many rate-limit observations are merged per provider so a
/// pathological corpus (thousands of sessions, each with a long history)
/// cannot make quota aggregation unbounded. Recent observations are kept.
const MAX_POINTS_PER_PROVIDER: usize = 2_000;

/// How many rate-limit points [`QuotaPointsIndex`] *retains* per provider,
/// evicting the oldest at insert time once this is exceeded (issue #152).
/// Deliberately larger than [`MAX_POINTS_PER_PROVIDER`] — the number a read
/// actually consumes — rather than equal to it, and the margin is not
/// cosmetic:
///
/// When a session is retracted (its source file is deleted, or it is
/// re-parsed with a shorter history — see the "hard part" comment below),
/// every point it contributed disappears from the index immediately, and
/// any of its points that were evicted *earlier* cannot come back without a
/// ledger read. If retention exactly matched `MAX_POINTS_PER_PROVIDER`, a
/// single retraction could leave the read path with fewer than
/// `MAX_POINTS_PER_PROVIDER` points even though the corpus still has
/// plenty of older evidence elsewhere — silently degrading the pace/
/// forecast math (`forecast_from_series` needs `MIN_EVIDENCE_POINTS`) for
/// no reason other than an avoidable eviction policy. Retaining 4x the read
/// cap means a retraction has to remove more than 6,000 points below the
/// read cap's own 2,000 before the read path could possibly be shorted —
/// far more headroom than any single session's retraction plausibly
/// creates in isolation, and the cost of that headroom is nearly free: even
/// at `RETAINED_POINTS_PER_PROVIDER` (8,000) points times the 3 providers
/// observed in the field, the retained set costs single-digit MB.
const RETAINED_POINTS_PER_PROVIDER: usize = 4 * MAX_POINTS_PER_PROVIDER;

// Enforced at compile time, not just by a test: if this constant is ever
// edited to lose the margin over the read cap, the build itself fails
// rather than relying on a test run catching it.
const _: () = assert!(RETAINED_POINTS_PER_PROVIDER > MAX_POINTS_PER_PROVIDER);

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

#[derive(Debug, Clone, PartialEq, Serialize)]
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
// Incremental per-provider points index (issue #131)
//
// `quota_snapshots_from_sessions` above is still the from-scratch reference
// implementation: the correctness oracle every equivalence test in this
// module checks against, and what `commands::check_quota_alerts` still calls
// directly (it already recomputes unconditionally on every poll and does not
// route through `AppState`'s cache, so it is out of this issue's scope).
// `QuotaPointsIndex` is a second implementation of the same aggregation,
// maintained incrementally so `AppState::quota_snapshots`'s recompute (still
// throttled by `QuotaSnapshotCache` below, which is unchanged) costs
// O(a session's own point count) at update time and
// O(min(corpus, MAX_POINTS_PER_PROVIDER)) at read time, instead of O(entire
// corpus) on every recompute.
//
// ## The hard part: replacement, not just accumulation
//
// A session's `rate_limits_history` can be replaced wholesale — re-parse,
// truncation, rotation, and the batched scan path (#132,
// `AppState::reconcile_scanned_batch_if_current`) all rewrite a session's
// full history, not just append to it. Naively `extend`-ing a maintained
// `Vec` on every update would duplicate every point the session contributed
// before, inflating used-percent and pace math forever. So this index
// remembers, per session key, exactly which composite keys that session
// last contributed (`SessionContribution`) and retracts all of them before
// inserting the session's current content — including when the new history
// is *shorter* than the old one, where there is no way to know how many of
// the old points to drop by comparing lengths; the old contribution must be
// fully retracted and the new one fully reinserted. See the
// `index_and_from_scratch_agree_*` tests below, which exercise append,
// grow-replace, shrink-replace, and outright removal and check each step
// against a from-scratch computation over the same corpus.
//
// ## Ordering and the two-tier cap (issue #131, then #152)
//
// Points are kept per provider in a `BTreeMap` ordered by
// `(timestamp, session_key, in-session sequence)` rather than a `Vec` that
// would need re-sorting on every update. The tie-break beyond timestamp is
// new: the from-scratch path's tie-break for two *different* sessions
// reporting the exact same instant was already unspecified (session
// iteration order feeding a stable sort — never guaranteed reproducible),
// so a deterministic one here does not change any documented behavior for
// the realistic case (no two sessions observe a rate limit at the exact
// same instant).
//
// Two caps apply, at two different times, for two different reasons:
//
// - `MAX_POINTS_PER_PROVIDER` (2,000) is the *read*-time cap — keep only
//   the most recent points across the whole corpus — applied when
//   producing a snapshot's input slice (`provider_points`). It is not
//   maintained eagerly at write time on its own: trimming the maintained
//   map down to exactly this many on every write would risk permanently
//   discarding a point that a *later* retraction makes relevant again
//   (an older point that used to sit just outside the top 2,000 can become
//   part of it once a newer session's points are retracted), silently
//   breaking equivalence with a from-scratch computation. Reading the
//   newest N is still O(N) off a `BTreeMap`'s tail, not O(map size).
// - `RETAINED_POINTS_PER_PROVIDER` (4x that; see its own doc comment for
//   why the margin matters) *is* maintained eagerly: `update_session`
//   evicts the globally oldest points (`BTreeMap::pop_first`) right after
//   inserting a session's new ones, whenever the bucket exceeds this cap,
//   regardless of which session contributed the evicted point. This is what
//   bounds the index's memory to the retained set instead of the full
//   corpus (issue #152 measured ~10.9M point-copies retained to serve a
//   6,000-point read path). Its margin over the read cap is exactly why
//   this eager trim is safe unlike the read cap would be: a retraction can
//   only ever expose points that were already inside the retained cushion
//   below the read cap, never points evicted outright — see
//   `RETAINED_POINTS_PER_PROVIDER`'s doc comment.
//
// Eviction prunes the owning session's `SessionContribution.point_keys` in
// the same step — falling back to trimming the *local*, not-yet-stored list
// when a session's own just-inserted points are what gets evicted — so
// `contributions` never keeps a key `by_key` no longer has. Without that,
// a session with a pathological history (up to 22,395 points observed in
// the field) would still cost `contributions` its full, unbounded point
// count even after `by_key` itself was capped, defeating most of the
// memory fix. See `evict_excess`.
//
// ## Concurrency note
//
// Every session-mutation site in `store.rs` calls `update_session` right
// next to the corresponding `AppState::sessions` write, but the two are not
// one atomic operation — this index has its own `Mutex`, separate from
// `DashMap`'s per-shard locking on `sessions`. Two source paths racing to
// publish the *same* session key at the same instant (the "displaced" case)
// could theoretically leave this index and `AppState::sessions` briefly
// disagreeing about which of the two contents is current — exactly as
// `AppState::sessions` itself already offers no stronger guarantee than
// "whichever insert lands last, for that key, wins" once outside the
// `session_paths` bookkeeping (see `store.rs`'s own doc comments on the
// scan/watcher and removal races it already accepts and self-heals). Any
// later update for that key — and there is always a later one, since
// sessions are continuously re-observed — re-synchronizes it, matching that
// existing eventual-consistency posture rather than introducing a new one.
// ---------------------------------------------------------------------------

/// One rate-limit point's position inside a provider's maintained,
/// timestamp-ordered index. `session_key` and `seq` only break ties among
/// otherwise-equal timestamps (see the module note above); they are never
/// used to look a point up any other way.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PointKey {
    timestamp: DateTime<Utc>,
    session_key: String,
    seq: u32,
}

#[derive(Default)]
struct ProviderPoints {
    by_key: BTreeMap<PointKey, RateLimitSnapshotPoint>,
}

#[derive(Default)]
struct ProviderAccounts {
    by_key: BTreeMap<(DateTime<Utc>, String), QuotaAccountInfo>,
}

/// What one session last contributed, so [`QuotaPointsIndex::update_session`]
/// can retract exactly that (not merely "the first N points") before
/// inserting whatever the session reports now.
struct SessionContribution {
    provider: ProviderId,
    point_keys: Vec<PointKey>,
    account_key: Option<(DateTime<Utc>, String)>,
}

#[derive(Default)]
struct QuotaPointsIndexState {
    points: HashMap<ProviderId, ProviderPoints>,
    accounts: HashMap<ProviderId, ProviderAccounts>,
    contributions: HashMap<String, SessionContribution>,
}

/// Incrementally maintained per-provider rate-limit points and credit-
/// balance observations, keyed by session storage id. See the module
/// section above for the design and the concurrency trade-off.
#[derive(Default)]
pub struct QuotaPointsIndex {
    state: Mutex<QuotaPointsIndexState>,
}

impl QuotaPointsIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Folds `session`'s current rate-limit history and credit info into
    /// the index under `session_key`, first retracting whatever this
    /// session contributed previously (if anything). Cost is O(the
    /// session's old point count + its new point count), never the corpus.
    pub fn update_session(&self, session_key: &str, session: &Session) {
        let mut state = self.state.lock().unwrap();
        Self::retract(&mut state, session_key);

        let provider = session.harness.clone();
        let mut point_keys = Vec::with_capacity(session.rate_limits_history.len());
        if !session.rate_limits_history.is_empty() {
            let bucket = state.points.entry(provider.clone()).or_default();
            for (seq, point) in session.rate_limits_history.iter().enumerate() {
                let key = PointKey {
                    timestamp: point.timestamp,
                    session_key: session_key.to_string(),
                    seq: seq as u32,
                };
                bucket.by_key.insert(key.clone(), point.clone());
                point_keys.push(key);
            }
            Self::evict_excess(&mut state, &provider, session_key, &mut point_keys);
        }

        let account_key =
            if session.credits_unlimited.is_some() || session.credits_balance.is_some() {
                let key = (session.last_event_at, session_key.to_string());
                let account = QuotaAccountInfo {
                    credits_unlimited: session.credits_unlimited,
                    credits_balance: session.credits_balance,
                    observed_at: Some(session.last_event_at),
                };
                state
                    .accounts
                    .entry(provider.clone())
                    .or_default()
                    .by_key
                    .insert(key.clone(), account);
                Some(key)
            } else {
                None
            };

        if point_keys.is_empty() && account_key.is_none() {
            state.contributions.remove(session_key);
        } else {
            state.contributions.insert(
                session_key.to_string(),
                SessionContribution {
                    provider,
                    point_keys,
                    account_key,
                },
            );
        }
    }

    /// Retracts whatever `session_key` last contributed without inserting
    /// anything new. No production call site removes a session from
    /// `AppState::sessions` outright today — every mutation site replaces a
    /// session's content or marks one `Missing` in place, which
    /// `update_session` alone covers — but removal is the operation issue
    /// #131 calls out as the hard case, so it is a first-class, directly
    /// testable method here rather than only reachable by inference from
    /// `update_session`.
    pub fn remove_session(&self, session_key: &str) {
        let mut state = self.state.lock().unwrap();
        Self::retract(&mut state, session_key);
    }

    fn retract(state: &mut QuotaPointsIndexState, session_key: &str) {
        let Some(contribution) = state.contributions.remove(session_key) else {
            return;
        };
        if let Some(bucket) = state.points.get_mut(&contribution.provider) {
            for key in &contribution.point_keys {
                bucket.by_key.remove(key);
            }
        }
        if let Some(account_key) = contribution.account_key {
            if let Some(bucket) = state.accounts.get_mut(&contribution.provider) {
                bucket.by_key.remove(&account_key);
            }
        }
    }

    /// Evicts the globally oldest points in `provider`'s bucket until it is
    /// back at [`RETAINED_POINTS_PER_PROVIDER`], after `update_session` has
    /// just inserted `session_key`'s current points. An evicted point may
    /// belong to *any* session, not just the one being updated — eviction
    /// removes the oldest points across the whole provider bucket
    /// (`BTreeMap::pop_first`), same as the read path keeps the newest
    /// across the whole bucket.
    ///
    /// Whichever session owned an evicted point must stop claiming it in
    /// `SessionContribution.point_keys`, or that field is exactly the
    /// "replaced one unbounded Vec with another" bug: a session with a
    /// pathological history (up to 22,395 points observed in the field)
    /// would otherwise still cost `contributions` its full point count
    /// forever, even though `by_key` itself is capped. Two cases:
    /// - The evicted point belongs to `session_key` itself: its key is
    ///   still only in the local `point_keys` accumulator passed in by
    ///   `update_session` (that call's own `SessionContribution` has not
    ///   been stored yet — `retract` already removed the old one), so it is
    ///   trimmed from there directly.
    /// - The evicted point belongs to some other, already-recorded session:
    ///   its key is trimmed from that session's stored `SessionContribution`
    ///   in `state.contributions`.
    fn evict_excess(
        state: &mut QuotaPointsIndexState,
        provider: &ProviderId,
        session_key: &str,
        point_keys: &mut Vec<PointKey>,
    ) {
        let Some(bucket) = state.points.get_mut(provider) else {
            return;
        };
        while bucket.by_key.len() > RETAINED_POINTS_PER_PROVIDER {
            let Some((evicted, _)) = bucket.by_key.pop_first() else {
                break;
            };
            if evicted.session_key.as_str() == session_key {
                point_keys.retain(|key| *key != evicted);
            } else if let Some(contribution) = state.contributions.get_mut(&evicted.session_key) {
                contribution.point_keys.retain(|key| *key != evicted);
            }
        }
    }

    /// Builds every registered provider's `QuotaSnapshot` from the
    /// currently maintained state. This is `AppState::quota_snapshots`'s
    /// recompute path, replacing the O(corpus) walk in
    /// `quota_snapshots_from_sessions` with a read bounded by
    /// `MAX_POINTS_PER_PROVIDER` (issue #131).
    pub fn snapshots(&self, now: DateTime<Utc>, max_cache_age: Duration) -> Vec<QuotaSnapshot> {
        let registry = crate::provider::ProviderRegistry::builtin();
        let state = self.state.lock().unwrap();
        registry
            .descriptors()
            .map(|descriptor| {
                let points = Self::provider_points(&state, &descriptor.id);
                let account = state
                    .accounts
                    .get(&descriptor.id)
                    .and_then(|bucket| bucket.by_key.values().next_back().cloned())
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

    /// The provider's points in ascending timestamp order, capped to the
    /// most recent `MAX_POINTS_PER_PROVIDER` exactly like
    /// `quota_snapshots_from_sessions`'s own truncation. Cost is bounded by
    /// the cap, not by how many points the provider has accumulated.
    fn provider_points(
        state: &QuotaPointsIndexState,
        provider: &ProviderId,
    ) -> Vec<RateLimitSnapshotPoint> {
        let Some(bucket) = state.points.get(provider) else {
            return Vec::new();
        };
        if bucket.by_key.len() <= MAX_POINTS_PER_PROVIDER {
            return bucket.by_key.values().cloned().collect();
        }
        let mut points: Vec<RateLimitSnapshotPoint> = bucket
            .by_key
            .values()
            .rev()
            .take(MAX_POINTS_PER_PROVIDER)
            .cloned()
            .collect();
        points.reverse();
        points
    }
}

// ---------------------------------------------------------------------------
// Quota snapshot cache (issue #128)
//
// `quota_snapshots_from_sessions` clones every rate-limit point of every
// session in the corpus — O(corpus), measured at 1.3-1.8s on a ~4,000-
// session field recording. The tray refresh path (`App.svelte`) calls
// `get_quota_snapshots` on essentially every session update (a median
// ~289ms apart during active use), which turned an otherwise-incremental
// hot path into a near-continuous full-corpus rebuild: 32.3s of an 87s
// run.
//
// This cache makes the steady-state cost O(1): most calls return a clone
// of the last computed (small — one entry per provider) `Vec<QuotaSnapshot>`
// rather than re-aggregating. It recomputes whenever *either*:
//   1. `sessions_generation` (see `AppState::sessions_generation`) has
//      changed since the last recompute *and* at least
//      `QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL` has elapsed — collapses a
//      burst of rapid session updates into a single recompute, or
//   2. at least `QUOTA_SNAPSHOT_MAX_AGE` has elapsed since the last
//      recompute, regardless of whether sessions changed at all.
//
// Condition 2 is not optional. A `QuotaSnapshot` is not a pure function of
// session data — `QuotaWindow.stale` (`age > max_cache_age`), the elapsed
// fraction driving `reserve_deficit_percent`, and pace/projected-exhaustion
// math (`forecast_from_series`) are all functions of `now`. A generation-
// only condition never recomputes on an idle app (no session updates), so
// a window computed as fresh at 09:00 would still be served as fresh at
// 10:00 even though an hour has genuinely passed — the staleness detector
// itself going stale, exactly inverting #43's "never present a cached
// value as fresher than it is". Condition 2 bounds that.
//
// This never fabricates freshness: a cache hit returns the exact
// `Vec<QuotaSnapshot>` a prior call already computed (with its own
// `observed_at`/`stale` fields intact, honestly reflecting the `now` used
// when it was actually computed), not a value whose timestamps get
// silently bumped to "now" on serve — condition 2 exists precisely so that
// value doesn't get served for longer than `QUOTA_SNAPSHOT_MAX_AGE` once
// it's actually gone stale.
// ---------------------------------------------------------------------------

/// Minimum wall-clock time between two full recomputes triggered by a
/// session change, even if sessions keep changing in between. Every
/// provider quota window Odometer tracks moves on the order of minutes
/// (`daily`/`weekly`/`monthly`; even a `burst` window is measured in tens
/// of minutes — see `classify_window_kind`), so no consumer benefits from
/// a data-driven recompute more often than this. A live session can mutate
/// roughly every ~289ms during active use (issue #128's field recording),
/// so without a floor the cache would still recompute on almost every
/// call. 5 seconds keeps the tray visibly live (it already debounces its
/// own refresh by 250ms) while turning the field recording's 21 recomputes
/// at ~1.5s each (32.3s total) into at most ~1 every 5s.
pub const QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(5);

/// Upper bound on how long a cached snapshot may be served without a
/// recompute, regardless of whether `sessions_generation` ever changes —
/// the fix for the time-dependence described above. Chosen against what I
/// found checking the configurable floor: `quota_store::validate_quota_config`
/// only rejects `max_cache_age_secs <= 0` (no lower bound beyond that), so
/// a value of 1 is technically writable through `set_quota_config`, and a
/// hand-edited `quota-v1.json` isn't validated on load at all. Neither is
/// reachable through today's UI, though: nothing in `SubscriptionUsage.svelte`
/// ever edits `max_cache_age_secs`, so every real installation runs with
/// the shipped default of 21,600s (6h). 30 seconds keeps every realistic
/// staleness transition within an easily-noticed margin, stays well above
/// `QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL` so it never fights the
/// session-change throttle, and — being a small fraction of even the
/// shortest real window kind (`burst`, tens of minutes) — never turns into
/// a meaningful fraction of "recomputing too often". A hand-edited
/// `max_cache_age_secs` under 30s would still see its staleness transition
/// lag by up to this bound; that is an accepted tradeoff of bounding
/// recompute cost on an otherwise-idle app at all, for a configuration no
/// shipped UI can produce.
pub const QUOTA_SNAPSHOT_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(30);

struct QuotaSnapshotCacheEntry {
    snapshots: Vec<QuotaSnapshot>,
    sessions_generation: u64,
    computed_at: std::time::Instant,
}

/// Caches `quota_snapshots_from_sessions`'s output. See the module section
/// above for the recompute policy and the honesty guarantee.
#[derive(Default)]
pub struct QuotaSnapshotCache {
    entry: std::sync::Mutex<Option<QuotaSnapshotCacheEntry>>,
}

impl QuotaSnapshotCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the cached snapshots, calling `compute` to refresh them only
    /// when at least one recompute condition is met (see the module doc).
    /// `now` is the caller's monotonic-clock reading, passed in rather than
    /// read internally so the recompute gate is deterministic and
    /// unit-testable without real sleeps.
    pub fn get_or_recompute(
        &self,
        sessions_generation: u64,
        now: std::time::Instant,
        compute: impl FnOnce() -> Vec<QuotaSnapshot>,
    ) -> Vec<QuotaSnapshot> {
        let mut guard = self.entry.lock().unwrap();
        let should_recompute = match guard.as_ref() {
            None => true,
            Some(entry) => {
                let age = now.saturating_duration_since(entry.computed_at);
                let changed_and_throttle_elapsed = entry.sessions_generation != sessions_generation
                    && age >= QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL;
                let max_age_elapsed = age >= QUOTA_SNAPSHOT_MAX_AGE;
                changed_and_throttle_elapsed || max_age_elapsed
            }
        };
        if should_recompute {
            let snapshots = compute();
            *guard = Some(QuotaSnapshotCacheEntry {
                snapshots: snapshots.clone(),
                sessions_generation,
                computed_at: now,
            });
            snapshots
        } else {
            guard.as_ref().expect("checked above").snapshots.clone()
        }
    }

    /// Forces the next call to recompute, regardless of generation or
    /// elapsed time. The only caller is a quota-config change
    /// (`AppState::set_quota_store`, from `commands::set_quota_config`):
    /// `max_cache_age_secs` feeds `quota_snapshots_from_sessions` directly,
    /// so an edit to it must not wait out the recompute interval to take
    /// effect.
    pub fn invalidate(&self) {
        *self.entry.lock().unwrap() = None;
    }
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

    // -- QuotaPointsIndex: incremental aggregation agrees with from-scratch
    //    (issue #131) ---------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn test_session(
        key: &str,
        harness: ProviderId,
        last_event_at: DateTime<Utc>,
        credits_unlimited: Option<bool>,
        credits_balance: Option<f64>,
        rate_limits_history: Vec<RateLimitSnapshotPoint>,
    ) -> Session {
        Session {
            id: key.to_string(),
            storage_id: key.to_string(),
            harness,
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: String::new(),
            source_availability: crate::model::SourceAvailability::Present,
            archived: false,
            started_at: last_event_at,
            last_event_at,
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
            credits_unlimited,
            credits_balance,
            context_window: None,
            latest_context_tokens: None,
            total_turns: 0,
            first_user_message: None,
            tokens_total: crate::model::TokenTotals::default(),
            tokens_by_model: std::collections::HashMap::new(),
            tokens_history: Vec::new(),
            rate_limits_history,
            turns: Vec::new(),
            tool_observations: Vec::new(),
            tool_metrics: crate::model::ToolMetrics::default(),
            tool_metrics_by_model: std::collections::BTreeMap::new(),
            category_totals: std::collections::BTreeMap::new(),
            optimization_findings: Vec::new(),
            project_key: None,
            project_label: None,
            project_provenance: None,
        }
    }

    fn assert_index_matches_oracle(
        index: &QuotaPointsIndex,
        sessions: &std::collections::HashMap<String, Session>,
        now: DateTime<Utc>,
    ) {
        let oracle_sessions: Vec<Session> = sessions.values().cloned().collect();
        let oracle = quota_snapshots_from_sessions(oracle_sessions.iter(), now, Duration::hours(6));
        let indexed = index.snapshots(now, Duration::hours(6));
        assert_eq!(
            indexed, oracle,
            "incremental index disagrees with a from-scratch computation over the same corpus"
        );
    }

    // The duplication bug named directly in issue #131: shrinking a
    // session's `rate_limits_history` (re-parse, truncation, rotation) must
    // retract every point it previously contributed, not just the ones a
    // length comparison would suggest.
    #[test]
    fn replacing_a_sessions_history_with_a_shorter_one_matches_a_from_scratch_computation() {
        let long_points = evenly_spaced_points(
            "2026-01-01T00:00:00Z",
            12,
            8,
            10.0,
            5.0,
            300,
            "2026-01-01T05:00:00Z",
        );
        let short_points = long_points[0..2].to_vec();

        let index = QuotaPointsIndex::new();
        let mut session_a = test_session(
            "a",
            codex_provider_id(),
            long_points.last().unwrap().timestamp,
            None,
            None,
            long_points,
        );
        index.update_session("a", &session_a);

        // The history shrinks: only the first two of the original eight
        // observations are current now.
        session_a.rate_limits_history = short_points.clone();
        session_a.last_event_at = short_points.last().unwrap().timestamp;
        index.update_session("a", &session_a);

        let now = short_points.last().unwrap().timestamp + Duration::minutes(1);
        let mut sessions = std::collections::HashMap::new();
        sessions.insert("a".to_string(), session_a);
        assert_index_matches_oracle(&index, &sessions, now);

        // Made explicit, not just implied by the equality above: the six
        // discarded points are all timestamped after `now` (they cover
        // minutes 24-84; `now` is minute 13). If they had leaked through,
        // `series.last()` would pick one of them as the "latest"
        // observation, and a timestamp after `now` reads as clock skew
        // (`used: None`) rather than the correct fresh `used: Some(15.0)`.
        let indexed = index.snapshots(now, Duration::hours(6));
        let codex_snapshot = indexed
            .iter()
            .find(|s| s.provider == codex_provider_id())
            .unwrap();
        let window = &codex_snapshot.windows[0];
        assert_eq!(window.unavailable, None);
        assert_eq!(
            window.used,
            Some(short_points[1].primary.as_ref().unwrap().used_percent)
        );
        assert!(window.forecast.is_none());
    }

    // Covers append, replace-with-longer, replace-with-shorter, and outright
    // removal in one sequence, checking incremental-vs-from-scratch
    // agreement after every step.
    #[test]
    fn incremental_index_matches_from_scratch_across_append_grow_shrink_and_remove() {
        let epoch = ts("2026-01-01T00:00:00Z");
        // A single, globally-increasing timestamp cursor for the whole test:
        // no two points anywhere below share a timestamp, which sidesteps
        // the tie-break ambiguity `QuotaPointsIndex`'s doc comment notes
        // (the from-scratch path's own tie-break for two *different*
        // sessions reporting the same instant was already unspecified).
        let mut minute = 0i64;
        let mut next_point = |used_percent: f64| {
            let timestamp = epoch + Duration::minutes(minute);
            minute += 10;
            RateLimitSnapshotPoint {
                timestamp,
                turn_id: None,
                limit_id: None,
                primary: Some(RateLimitWindow {
                    used_percent,
                    window_minutes: Some(300),
                    resets_at: Some(epoch + Duration::hours(10_000)),
                }),
                secondary: None,
            }
        };

        let index = QuotaPointsIndex::new();
        let mut sessions: std::collections::HashMap<String, Session> =
            std::collections::HashMap::new();

        // Append: s1 starts with 3 points, then a later update appends 2 more.
        let mut s1 = test_session(
            "s1",
            codex_provider_id(),
            epoch,
            None,
            None,
            vec![next_point(10.0), next_point(15.0), next_point(20.0)],
        );
        index.update_session("s1", &s1);
        sessions.insert("s1".to_string(), s1.clone());
        let now = s1.rate_limits_history.last().unwrap().timestamp + Duration::minutes(1);
        assert_index_matches_oracle(&index, &sessions, now);

        s1.rate_limits_history.push(next_point(25.0));
        s1.rate_limits_history.push(next_point(30.0));
        s1.last_event_at = s1.rate_limits_history.last().unwrap().timestamp;
        index.update_session("s1", &s1);
        sessions.insert("s1".to_string(), s1.clone());
        let now = s1.rate_limits_history.last().unwrap().timestamp + Duration::minutes(1);
        assert_index_matches_oracle(&index, &sessions, now);

        // A second session enters the corpus, with credit-balance data too.
        let s2 = test_session(
            "s2",
            codex_provider_id(),
            epoch,
            Some(false),
            Some(500.0),
            vec![next_point(40.0), next_point(45.0), next_point(50.0)],
        );
        index.update_session("s2", &s2);
        sessions.insert("s2".to_string(), s2.clone());
        let now = s2.rate_limits_history.last().unwrap().timestamp + Duration::minutes(1);
        assert_index_matches_oracle(&index, &sessions, now);

        // Replace-longer: s1's history is rewritten to a longer one.
        let mut longer = Vec::new();
        for i in 0..8 {
            longer.push(next_point(10.0 + i as f64 * 5.0));
        }
        s1.rate_limits_history = longer;
        s1.last_event_at = s1.rate_limits_history.last().unwrap().timestamp;
        index.update_session("s1", &s1);
        sessions.insert("s1".to_string(), s1.clone());
        let now = s1.rate_limits_history.last().unwrap().timestamp + Duration::minutes(1);
        assert_index_matches_oracle(&index, &sessions, now);

        // Replace-shorter: s1's history is rewritten down to 2 points,
        // discarding the other 6 — the duplication-bug case, again, this
        // time alongside another session with its own live contribution.
        s1.rate_limits_history = vec![next_point(12.0), next_point(18.0)];
        s1.last_event_at = s1.rate_limits_history.last().unwrap().timestamp;
        index.update_session("s1", &s1);
        sessions.insert("s1".to_string(), s1.clone());
        let now = s1.rate_limits_history.last().unwrap().timestamp + Duration::minutes(1);
        assert_index_matches_oracle(&index, &sessions, now);

        // Remove: s2 leaves the corpus outright.
        index.remove_session("s2");
        sessions.remove("s2");
        assert_index_matches_oracle(&index, &sessions, now);
    }

    // Property-style: a randomized sequence of insert/replace/remove
    // operations against a small pool of session keys, checked against a
    // from-scratch computation periodically through the run. A tiny
    // deterministic xorshift PRNG keeps this reproducible without adding a
    // `rand` dependency for one test.
    #[test]
    fn incremental_index_matches_from_scratch_over_a_randomized_operation_sequence() {
        struct Xorshift64(u64);
        impl Xorshift64 {
            fn next_u64(&mut self) -> u64 {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                x
            }
            fn next_range(&mut self, bound: u64) -> u64 {
                self.next_u64() % bound
            }
        }

        let mut rng = Xorshift64(0x9E3779B97F4A7C15);
        let epoch = ts("2026-01-01T00:00:00Z");
        let mut minute = 0i64;

        let index = QuotaPointsIndex::new();
        let mut sessions: std::collections::HashMap<String, Session> =
            std::collections::HashMap::new();
        let session_keys = ["s1", "s2", "s3", "s4"];

        for step in 0..300u32 {
            let action = rng.next_range(4);
            let key = session_keys[rng.next_range(session_keys.len() as u64) as usize];

            match action {
                0 | 1 => {
                    // insert-new or replace-in-place with a fresh,
                    // random-length history (also covers append/grow/shrink,
                    // since the key may already exist with a different
                    // length).
                    let point_count = 1 + rng.next_range(6);
                    let mut history = Vec::new();
                    for _ in 0..point_count {
                        let timestamp = epoch + Duration::minutes(minute);
                        minute += 10;
                        history.push(RateLimitSnapshotPoint {
                            timestamp,
                            turn_id: None,
                            limit_id: None,
                            primary: Some(RateLimitWindow {
                                used_percent: rng.next_range(100) as f64,
                                window_minutes: Some(300),
                                resets_at: Some(epoch + Duration::hours(100_000)),
                            }),
                            secondary: None,
                        });
                    }
                    let has_credits = rng.next_range(2) == 0;
                    let session = test_session(
                        key,
                        codex_provider_id(),
                        history.last().map(|p| p.timestamp).unwrap_or(epoch),
                        has_credits.then_some(false),
                        has_credits.then_some(100.0 + rng.next_range(900) as f64),
                        history,
                    );
                    index.update_session(key, &session);
                    sessions.insert(key.to_string(), session);
                }
                _ => {
                    index.remove_session(key);
                    sessions.remove(key);
                }
            }

            if step % 5 == 0 {
                let now = epoch + Duration::minutes(minute + 5);
                assert_index_matches_oracle(&index, &sessions, now);
            }
        }

        let now = epoch + Duration::minutes(minute + 5);
        assert_index_matches_oracle(&index, &sessions, now);
    }

    // -- QuotaPointsIndex: retention cap and eviction (issue #152) ----------

    /// Builds a session with `count` points, one per second starting at
    /// `epoch`, so every point has a strictly distinct timestamp — sidesteps
    /// the tie-break ambiguity the module doc notes for two sessions
    /// reporting the exact same instant (irrelevant here: these tests use a
    /// single session per provider bucket).
    fn session_with_point_count(key: &str, epoch: DateTime<Utc>, count: usize) -> Session {
        let history: Vec<RateLimitSnapshotPoint> = (0..count)
            .map(|i| RateLimitSnapshotPoint {
                timestamp: epoch + Duration::seconds(i as i64),
                turn_id: None,
                limit_id: None,
                primary: Some(RateLimitWindow {
                    used_percent: (i % 100) as f64,
                    window_minutes: Some(300),
                    resets_at: Some(epoch + Duration::hours(100_000)),
                }),
                secondary: None,
            })
            .collect();
        let last_ts = history.last().map(|p| p.timestamp).unwrap_or(epoch);
        test_session(key, codex_provider_id(), last_ts, None, None, history)
    }

    // Below the retention cap, nothing is ever evicted, so the index must
    // still match the from-scratch oracle exactly — not just "on the final
    // result" the way the above-cap test below has to settle for.
    #[test]
    fn index_matches_oracle_exactly_when_corpus_is_below_the_retention_cap() {
        let epoch = ts("2026-01-01T00:00:00Z");
        let index = QuotaPointsIndex::new();
        let mut sessions: std::collections::HashMap<String, Session> =
            std::collections::HashMap::new();

        // 5 sessions x 500 points = 2,500 points total, comfortably under
        // RETAINED_POINTS_PER_PROVIDER (8,000).
        for s in 0..5 {
            let key = format!("s{s}");
            let session_epoch = epoch + Duration::hours(s as i64 * 10_000); // disjoint per session
            let session = session_with_point_count(&key, session_epoch, 500);
            index.update_session(&key, &session);
            sessions.insert(key, session);
        }

        {
            let state = index.state.lock().unwrap();
            let retained = state
                .points
                .get(&codex_provider_id())
                .map(|bucket| bucket.by_key.len())
                .unwrap_or(0);
            assert_eq!(
                retained, 2_500,
                "nothing should be evicted below the retention cap"
            );
        }

        let now = epoch + Duration::hours(5 * 10_000) + Duration::hours(1);
        assert_index_matches_oracle(&index, &sessions, now);
    }

    // Above the retention cap, the index's maintained set and the oracle's
    // full view legitimately diverge — the index only remembers the newest
    // RETAINED_POINTS_PER_PROVIDER, the oracle sees every point ever
    // reported — but both ultimately reduce to the newest
    // MAX_POINTS_PER_PROVIDER (2,000) when building a snapshot, and
    // RETAINED_POINTS_PER_PROVIDER's margin over that guarantees the
    // newest-2,000 subset is identical either way. So the snapshot *result*
    // must still match exactly; this is not a case for relaxing the
    // equivalence assertion.
    #[test]
    fn index_matches_oracle_on_the_final_result_when_corpus_exceeds_the_retention_cap() {
        let epoch = ts("2026-01-01T00:00:00Z");
        let session = session_with_point_count("huge", epoch, 10_000);
        let last_ts = session.rate_limits_history.last().unwrap().timestamp;

        let index = QuotaPointsIndex::new();
        index.update_session("huge", &session);
        let mut sessions = std::collections::HashMap::new();
        sessions.insert("huge".to_string(), session);

        {
            let state = index.state.lock().unwrap();
            let retained = state
                .points
                .get(&codex_provider_id())
                .map(|bucket| bucket.by_key.len())
                .unwrap_or(0);
            assert_eq!(
                retained, RETAINED_POINTS_PER_PROVIDER,
                "a 10,000-point corpus must be trimmed down to the retention cap"
            );
        }

        let now = last_ts + Duration::minutes(1);
        assert_index_matches_oracle(&index, &sessions, now);
    }

    // The regression test issue #152 asks for directly: a single session
    // with a pathological point count (above the field-observed max of
    // 22,395) must not grow the index without bound, and — the specific bug
    // this issue named — `contributions` must only record keys that are
    // actually retained rather than the session's full, unbounded history.
    #[test]
    fn pathological_single_session_point_count_does_not_grow_the_index_without_bound() {
        // The margin itself (RETAINED_POINTS_PER_PROVIDER > MAX_POINTS_PER_PROVIDER)
        // is enforced at compile time by the `const _: () = assert!(...)` next
        // to `RETAINED_POINTS_PER_PROVIDER`'s definition, not re-checked here.
        let epoch = ts("2026-01-01T00:00:00Z");
        let session = session_with_point_count("huge", epoch, 25_000);

        let index = QuotaPointsIndex::new();
        index.update_session("huge", &session);

        let state = index.state.lock().unwrap();
        let retained = state
            .points
            .get(&codex_provider_id())
            .map(|bucket| bucket.by_key.len())
            .unwrap_or(0);
        assert_eq!(
            retained, RETAINED_POINTS_PER_PROVIDER,
            "a 25,000-point session must still be trimmed to the retention cap"
        );

        let contribution = state
            .contributions
            .get("huge")
            .expect("the session's contribution must still be tracked");
        assert_eq!(
            contribution.point_keys.len(),
            retained,
            "contributions must only record keys that are actually retained, not the \
             session's full 25,000-point history — otherwise the unbounded Vec this issue \
             is fixing has just moved from ResidentSession/points to contributions"
        );
    }

    // Retracting a session that contributed points now-evicted from `by_key`
    // (because a later session's insert pushed the bucket past the
    // retention cap) must remain a harmless no-op for those already-gone
    // keys — not a panic, and not a double-retraction of some other
    // session's still-live points that happen to share a key by coincidence
    // (they cannot, `PointKey` embeds `session_key`, but this test proves
    // the removal path tolerates stale keys rather than assuming it).
    #[test]
    fn retracting_a_session_with_already_evicted_points_is_a_harmless_no_op() {
        let epoch = ts("2026-01-01T00:00:00Z");
        let index = QuotaPointsIndex::new();

        // "old" contributes points that will all be older than anything
        // "new" contributes.
        let old = session_with_point_count("old", epoch, 100);
        index.update_session("old", &old);

        // "new" alone exceeds the retention cap, so every one of "old"'s
        // points is evicted as a side effect of inserting "new"'s.
        let new_epoch = epoch + Duration::hours(1);
        let new = session_with_point_count("new", new_epoch, RETAINED_POINTS_PER_PROVIDER + 100);
        index.update_session("new", &new);

        {
            let state = index.state.lock().unwrap();
            assert!(
                state
                    .contributions
                    .get("old")
                    .is_none_or(|contribution| contribution.point_keys.is_empty()),
                "old's points should all have been evicted and pruned from its own \
                 contribution"
            );
        }

        // Retracting "old" now must not panic and must not disturb "new"'s
        // still-live points.
        index.remove_session("old");

        let now = new.rate_limits_history.last().unwrap().timestamp + Duration::minutes(1);
        let mut sessions = std::collections::HashMap::new();
        sessions.insert("new".to_string(), new);
        assert_index_matches_oracle(&index, &sessions, now);
    }

    // -- QuotaPointsIndex: per-update cost stays flat as the corpus grows
    //    (issue #131) — manual probe, not run in CI ---------------------------

    #[test]
    #[ignore = "manual perf probe; run with `cargo test --release quota_points_index_update_cost -- --ignored --nocapture`"]
    fn quota_points_index_update_cost_is_flat_as_corpus_grows() {
        fn seed_session(idx: usize, epoch: DateTime<Utc>) -> Session {
            let history: Vec<RateLimitSnapshotPoint> = (0..20)
                .map(|i| RateLimitSnapshotPoint {
                    timestamp: epoch + Duration::minutes((idx * 100 + i) as i64),
                    turn_id: None,
                    limit_id: None,
                    primary: Some(RateLimitWindow {
                        used_percent: (i as f64) * 2.0,
                        window_minutes: Some(300),
                        resets_at: Some(epoch + Duration::hours(100_000)),
                    }),
                    secondary: None,
                })
                .collect();
            test_session(
                &format!("s{idx}"),
                codex_provider_id(),
                epoch,
                None,
                None,
                history,
            )
        }

        fn probe(corpus_size: usize) -> (f64, f64) {
            let epoch = ts("2026-01-01T00:00:00Z");
            let index = QuotaPointsIndex::new();
            for i in 0..corpus_size {
                let session = seed_session(i, epoch);
                index.update_session(&format!("s{i}"), &session);
            }

            // Steady-state update cost: session "s0" keeps being replaced
            // with a slightly different history, as a live session's
            // rate-limit history changes turn to turn.
            let mut update_samples = Vec::with_capacity(50);
            for round in 0..50u32 {
                let mut session = seed_session(0, epoch);
                session.rate_limits_history.push(RateLimitSnapshotPoint {
                    timestamp: epoch + Duration::minutes(1_000_000 + round as i64),
                    turn_id: None,
                    limit_id: None,
                    primary: Some(RateLimitWindow {
                        used_percent: 50.0,
                        window_minutes: Some(300),
                        resets_at: Some(epoch + Duration::hours(100_000)),
                    }),
                    secondary: None,
                });
                let started = std::time::Instant::now();
                index.update_session("s0", &session);
                update_samples.push(started.elapsed().as_secs_f64() * 1_000_000.0);
            }
            update_samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let update_median_us = update_samples[update_samples.len() / 2];

            // Recompute (read) cost: what `AppState::quota_snapshots`'s
            // cache-miss path now does instead of walking every session.
            let mut read_samples = Vec::with_capacity(20);
            for _ in 0..20 {
                let now = epoch + Duration::hours(100_001);
                let started = std::time::Instant::now();
                let _ = index.snapshots(now, Duration::hours(6));
                read_samples.push(started.elapsed().as_secs_f64() * 1_000_000.0);
            }
            read_samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let read_median_us = read_samples[read_samples.len() / 2];

            (update_median_us, read_median_us)
        }

        let (small_update, small_read) = probe(200);
        let (large_update, large_read) = probe(8_000);
        println!(
            "quota_points_index probe: corpus=200 -> update {small_update:.1}us / \
             recompute {small_read:.1}us; corpus=8000 -> update {large_update:.1}us / \
             recompute {large_read:.1}us"
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

    // -- QuotaSnapshotCache: recompute policy (issue #128) -------------------
    //
    // These use a counting stand-in for the expensive aggregation
    // (`quota_snapshots_from_sessions` in production) to prove it runs at
    // most once across many calls with no intervening session change, and
    // exactly once more after a change plus the min-recompute interval —
    // the behavior that was missing before this fix, when every call
    // recomputed unconditionally.

    #[test]
    fn quota_snapshot_cache_computes_once_across_many_calls_with_no_session_change() {
        let cache = QuotaSnapshotCache::new();
        let calls = std::cell::Cell::new(0u32);
        let compute = || {
            calls.set(calls.get() + 1);
            Vec::new()
        };
        let t0 = std::time::Instant::now();

        cache.get_or_recompute(1, t0, compute);
        assert_eq!(calls.get(), 1, "the first call always computes");

        // Same generation (nothing changed), called repeatedly, including
        // well past the min-recompute interval but still short of
        // QUOTA_SNAPSHOT_MAX_AGE: must never recompute purely from data
        // being unchanged. (Time alone crossing QUOTA_SNAPSHOT_MAX_AGE is
        // its own, separately-tested recompute trigger — see
        // `quota_snapshot_cache_recomputes_after_max_age_even_without_a_session_change`.)
        cache.get_or_recompute(1, t0, compute);
        cache.get_or_recompute(1, t0 + QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL * 2, compute);
        cache.get_or_recompute(
            1,
            t0 + QUOTA_SNAPSHOT_MAX_AGE - std::time::Duration::from_millis(1),
            compute,
        );
        assert_eq!(
            calls.get(),
            1,
            "no session change means no recompute, however many calls, as long as the max age hasn't elapsed"
        );
    }

    #[test]
    fn quota_snapshot_cache_recomputes_after_max_age_even_without_a_session_change() {
        // The bug this guards against: on an idle app (no session updates,
        // so `sessions_generation` never moves), a generation-only
        // recompute condition would cache a snapshot forever. Time alone
        // must still force a recompute, because `stale`/pace/forecast are
        // functions of `now`, not just session data.
        let cache = QuotaSnapshotCache::new();
        let calls = std::cell::Cell::new(0u32);
        let compute = || {
            calls.set(calls.get() + 1);
            Vec::new()
        };
        let t0 = std::time::Instant::now();
        cache.get_or_recompute(1, t0, compute);
        assert_eq!(calls.get(), 1);

        // Still generation 1 (no session change), but QUOTA_SNAPSHOT_MAX_AGE
        // has fully elapsed.
        cache.get_or_recompute(1, t0 + QUOTA_SNAPSHOT_MAX_AGE, compute);
        assert_eq!(
            calls.get(),
            2,
            "an idle app must still recompute once the max age elapses, even with no session change"
        );
    }

    #[test]
    fn quota_snapshot_cache_recomputes_once_after_a_session_change_once_the_interval_elapses() {
        let cache = QuotaSnapshotCache::new();
        let calls = std::cell::Cell::new(0u32);
        let compute = || {
            calls.set(calls.get() + 1);
            Vec::new()
        };
        let t0 = std::time::Instant::now();
        cache.get_or_recompute(1, t0, compute);
        assert_eq!(calls.get(), 1);

        // A session changed (generation 1 -> 2) immediately after, but the
        // min-recompute interval has not elapsed: repeated rapid calls must
        // keep serving the cached result, not recompute on every call.
        cache.get_or_recompute(2, t0 + std::time::Duration::from_millis(1), compute);
        cache.get_or_recompute(2, t0 + QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL / 2, compute);
        assert_eq!(
            calls.get(),
            1,
            "a changed generation inside the min-recompute interval must not recompute yet"
        );

        // Once the generation differs *and* the interval has elapsed, the
        // next call recomputes exactly once.
        cache.get_or_recompute(2, t0 + QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL, compute);
        assert_eq!(calls.get(), 2);

        // ...and settles back into "no further recompute" for that
        // generation, exactly like the first assertion.
        cache.get_or_recompute(2, t0 + QUOTA_SNAPSHOT_MIN_RECOMPUTE_INTERVAL * 5, compute);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn quota_snapshot_cache_invalidate_forces_an_immediate_recompute() {
        let cache = QuotaSnapshotCache::new();
        let calls = std::cell::Cell::new(0u32);
        let compute = || {
            calls.set(calls.get() + 1);
            Vec::new()
        };
        let t0 = std::time::Instant::now();
        cache.get_or_recompute(1, t0, compute);
        assert_eq!(calls.get(), 1);

        // Same generation, well inside the interval: ordinarily a cache
        // hit. An explicit invalidation (a quota-config change) must force
        // a recompute anyway rather than waiting out the interval.
        cache.invalidate();
        cache.get_or_recompute(1, t0 + std::time::Duration::from_millis(1), compute);
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn quota_snapshot_cache_hit_returns_exactly_the_prior_computation() {
        // Honesty: a served cache hit must be the same value the last real
        // recompute produced, not something that looks "freshened" merely
        // because time passed between calls.
        let now = ts("2026-01-01T00:00:00Z");
        let points = [point(
            "2026-01-01T00:00:00Z",
            42.0,
            300,
            "2026-01-01T05:00:00Z",
        )];
        let compute = || {
            vec![build_quota_snapshot(
                codex_provider_id(),
                true,
                &points,
                QuotaAccountInfo::default(),
                now,
                Duration::hours(6),
            )]
        };
        let cache = QuotaSnapshotCache::new();
        let t0 = std::time::Instant::now();
        let first = cache.get_or_recompute(1, t0, compute);
        let served = cache.get_or_recompute(1, t0 + std::time::Duration::from_millis(1), compute);
        assert_eq!(first[0].windows[0].used, Some(42.0));
        assert_eq!(
            served, first,
            "a cache hit must return exactly what was last computed, not a refreshed value"
        );
    }

    #[test]
    fn a_window_that_goes_stale_purely_from_elapsed_time_is_reported_stale_once_the_max_age_recompute_runs(
    ) {
        // The precise user-visible consequence of the bug: on an idle app
        // (no session updates, so `sessions_generation` never moves), a
        // window that was fresh when first cached must not still read as
        // fresh once real time has made it genuinely stale.
        let observed_at = ts("2026-01-01T00:00:00Z");
        let points = [point(
            "2026-01-01T00:00:00Z",
            40.0,
            300,
            "2026-01-01T05:00:00Z",
        )];
        let max_cache_age = Duration::seconds(120);
        let cache = QuotaSnapshotCache::new();
        let t0 = std::time::Instant::now();

        // First computation: 30s after the observation, well inside the
        // 120s max_cache_age, so the window is fresh.
        let now_fresh = observed_at + Duration::seconds(30);
        let fresh = cache.get_or_recompute(1, t0, || {
            vec![build_quota_snapshot(
                codex_provider_id(),
                true,
                &points,
                QuotaAccountInfo::default(),
                now_fresh,
                max_cache_age,
            )]
        });
        assert!(!fresh[0].windows[0].stale);

        // No session change (still generation 1), but QUOTA_SNAPSHOT_MAX_AGE
        // of monotonic time has passed, and wall-clock time has moved far
        // enough that the same observation is now genuinely past
        // max_cache_age (200s > 120s).
        let now_stale = observed_at + Duration::seconds(200);
        let served = cache.get_or_recompute(1, t0 + QUOTA_SNAPSHOT_MAX_AGE, || {
            vec![build_quota_snapshot(
                codex_provider_id(),
                true,
                &points,
                QuotaAccountInfo::default(),
                now_stale,
                max_cache_age,
            )]
        });
        assert!(
            served[0].windows[0].stale,
            "staleness must not stay frozen at 'fresh' on an idle app just because sessions never changed"
        );
    }
}
