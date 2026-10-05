//! Shared, local delivery policy. Quota-v2 is the only persisted notification
//! authority; candidates contain fixed codes and bounded evidence routes only.
use crate::quota_store::{NotificationLogEntry, NotificationSettings};
use chrono::{DateTime, Duration, Timelike, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

pub const PREFIX: &str = "@ambient/";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Categories {
    pub attention: bool,
    pub provider_incidents: bool,
    pub stale_quota: bool,
    pub retention_risk: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Budgets,
    Attention,
    ProviderStatus,
    Retention,
    Quota,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Notice {
    pub id: String,
    pub route: Route,
    /// A fixed label/code, never a source body, account label, or project path.
    pub provider: String,
    pub code: String,
    pub observed_at: DateTime<Utc>,
    pub delivered_at: DateTime<Utc>,
}

pub struct Candidate {
    pub key: String,
    pub route: Route,
    pub provider: String,
    pub code: String,
    pub observed_at: DateTime<Utc>,
    pub enabled: bool,
    pub fresh: bool,
}

/// Runtime failure suppression prevents replay after a failed disk write. It
/// is not a second history: only bounded hashes survive until process exit.
#[derive(Default)]
pub struct Runtime {
    pub initialized: bool,
    pub budget_checked_at: Option<DateTime<Utc>>,
    pub retention: Option<(DateTime<Utc>, bool, bool)>,
    pub policy_baseline: bool,
    pub last_check: Option<DateTime<Utc>>,
    pub was_quiet: bool,
    pub failed: BTreeSet<String>,
}

pub fn provider_label(provider: &str) -> String {
    match provider {
        "codex" => "Codex",
        "claude_code" => "Claude Code",
        "gemini_cli" => "Gemini CLI",
        _ => "Other provider",
    }
    .into()
}

pub fn key(value: &str) -> String {
    format!(
        "{PREFIX}{:016x}",
        crate::stable_hash::fnv1a64(value.as_bytes())
    )
}

/// Active conditions are rearmed only by an observed resolution. Unknown
/// inputs retain their keys; callers explicitly supply resolved condition keys.
/// Suppressed/stale candidates are consumed too, including the startup baseline.
pub fn evaluate(
    candidates: &[Candidate],
    resolved: &[String],
    settings: &NotificationSettings,
    log: &[NotificationLogEntry],
    runtime: &Runtime,
    now: DateTime<Utc>,
    hour: u8,
) -> (Vec<Notice>, Vec<NotificationLogEntry>) {
    let mut next = log.to_vec();
    next.retain(|entry| !resolved.contains(&entry.dedup_key));
    let mut notices = Vec::new();
    for candidate in candidates.iter().take(100) {
        let id = key(&candidate.key);
        if let Some(entry) = next.iter_mut().find(|entry| entry.dedup_key == id) {
            // Keep an active condition armed even after the log retention age.
            entry.fired_at = now;
            continue;
        }
        next.push(NotificationLogEntry {
            dedup_key: id.clone(),
            fired_at: now,
            notice: None,
        });
        if !runtime.initialized
            || !runtime.failed.is_empty()
            || !settings.enabled
            || crate::quota::in_quiet_hours(settings.quiet_hours, hour)
            || crate::quota::in_quiet_hours(
                settings.quiet_hours,
                candidate.observed_at.with_timezone(&chrono::Local).hour() as u8,
            )
            || !candidate.enabled
            || !candidate.fresh
            || candidate.observed_at > now
        {
            continue;
        }
        let notice = Notice {
            // State is keyed by the condition; delivery is keyed by this edge.
            // A resolved/rearmed condition must survive consumer deduplication.
            id: format!("{id}:{}", now.timestamp_micros()),
            route: candidate.route,
            provider: candidate.provider.clone(),
            code: candidate.code.clone(),
            observed_at: candidate.observed_at,
            delivered_at: now,
        };
        next.push(NotificationLogEntry {
            dedup_key: format!("{PREFIX}recent/{id}/{}", now.timestamp_millis()),
            fired_at: now,
            notice: Some(notice.clone()),
        });
        notices.push(notice);
    }
    (notices, next)
}

pub fn recent(log: &[NotificationLogEntry], now: DateTime<Utc>) -> Vec<Notice> {
    let mut notices: Vec<_> = log
        .iter()
        .filter_map(|entry| entry.notice.clone())
        .filter(|notice| {
            notice.delivered_at <= now && now - notice.delivered_at <= Duration::days(30)
        })
        .collect();
    notices.sort_by_key(|notice| std::cmp::Reverse(notice.delivered_at));
    notices.truncate(20);
    notices
}

#[derive(Serialize)]
pub struct Snapshot {
    pub available: bool,
    pub as_of: DateTime<Utc>,
    pub notifications: NotificationSettings,
    pub alerts: Vec<Notice>,
    pub recent: Vec<Notice>,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(now: DateTime<Utc>) -> Candidate {
        Candidate {
            key: "incident:codex".into(),
            route: Route::ProviderStatus,
            provider: "Codex".into(),
            code: "provider_incident".into(),
            observed_at: now,
            enabled: true,
            fresh: true,
        }
    }
    #[test]
    fn observation_from_quiet_hour_is_consumed_after_quiet_hours_end() {
        let now = Utc::now();
        let observed_hour = now.with_timezone(&chrono::Local).hour() as u8;
        let settings = NotificationSettings {
            enabled: true,
            quiet_hours: Some((observed_hour, (observed_hour + 1) % 24)),
            ..Default::default()
        };
        let runtime = Runtime {
            initialized: true,
            ..Default::default()
        };
        let awake_hour = (observed_hour + 2) % 24;
        let (alerts, log) = evaluate(
            &[candidate(now)],
            &[],
            &settings,
            &[],
            &runtime,
            now,
            awake_hour,
        );
        assert!(alerts.is_empty());
        let resumed = NotificationSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(evaluate(
            &[candidate(now)],
            &[],
            &resumed,
            &log,
            &runtime,
            now,
            awake_hour
        )
        .0
        .is_empty());
    }
    #[test]
    fn quiet_disabled_and_startup_are_consumed_without_replay() {
        let now = Utc::now();
        for (initialized, enabled, hour) in [(false, true, 12), (true, false, 12), (true, true, 23)]
        {
            let runtime = Runtime {
                initialized,
                ..Runtime::default()
            };
            let settings = NotificationSettings {
                enabled,
                quiet_hours: Some((22, 7)),
                ..Default::default()
            };
            let (alerts, log) =
                evaluate(&[candidate(now)], &[], &settings, &[], &runtime, now, hour);
            assert!(alerts.is_empty());
            let settings = NotificationSettings {
                enabled: true,
                ..Default::default()
            };
            let runtime = Runtime {
                initialized: true,
                ..Default::default()
            };
            assert!(
                evaluate(&[candidate(now)], &[], &settings, &log, &runtime, now, 12)
                    .0
                    .is_empty()
            );
        }
    }
    #[test]
    fn resolution_rearms_but_unknown_and_stale_do_not() {
        let now = Utc::now();
        let settings = NotificationSettings {
            enabled: true,
            ..Default::default()
        };
        let runtime = Runtime {
            initialized: true,
            ..Default::default()
        };
        let (alerts, log) = evaluate(&[candidate(now)], &[], &settings, &[], &runtime, now, 12);
        assert_eq!(alerts.len(), 1);
        assert_eq!(recent(&log, now).len(), 1);
        let (_, unknown) = evaluate(&[], &[], &settings, &log, &runtime, now, 12);
        assert!(evaluate(
            &[candidate(now)],
            &[],
            &settings,
            &unknown,
            &runtime,
            now,
            12
        )
        .0
        .is_empty());
        let (_, cleared) = evaluate(
            &[],
            &[key("incident:codex")],
            &settings,
            &log,
            &runtime,
            now,
            12,
        );
        let later = now + Duration::seconds(1);
        let rearmed = evaluate(
            &[candidate(later)],
            &[],
            &settings,
            &cleared,
            &runtime,
            later,
            12,
        )
        .0;
        assert_eq!(rearmed.len(), 1);
        assert_ne!(alerts[0].id, rearmed[0].id);
        let mut stale = candidate(now);
        stale.fresh = false;
        assert!(evaluate(&[stale], &[], &settings, &[], &runtime, now, 12)
            .0
            .is_empty());
    }
    #[test]
    fn persistence_failure_hash_suppresses_recovered_delivery() {
        let now = Utc::now();
        let runtime = Runtime {
            initialized: true,
            failed: BTreeSet::from([key("incident:codex")]),
            ..Default::default()
        };
        let settings = NotificationSettings {
            enabled: true,
            ..Default::default()
        };
        assert!(
            evaluate(&[candidate(now)], &[], &settings, &[], &runtime, now, 12)
                .0
                .is_empty()
        );
    }
}
