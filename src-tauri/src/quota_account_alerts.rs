//! Explicit live-account threshold rules. These consume only the consented,
//! cached LiveQuotaService status; transcript quota and ledger usage never
//! acquire an inferred account identity here.

use crate::provider::codex_provider_id;
use crate::quota::{self, BudgetEvaluation, QuotaAlert, QuotaProvenance, QuotaUnit};
use crate::quota_accounts::{LiveQuotaAccountView, LiveQuotaStatus};
use crate::quota_store::{
    live_account_log_key, BudgetUnit, LiveAccountBudget, NotificationLogEntry,
    NotificationSettings, QuotaBudget,
};
use chrono::{DateTime, Local, Timelike, Utc};

pub struct LiveAccountAlert {
    pub alert: QuotaAlert,
    pub observed_at: DateTime<Utc>,
}

fn current_window(
    rule: &LiveAccountBudget,
    status: &LiveQuotaStatus,
    now: DateTime<Utc>,
) -> Option<(f64, DateTime<Utc>)> {
    if status.configuration_error.is_some() || status.busy {
        return None;
    }
    let mut accounts = status.accounts.iter().filter(|account| {
        account.provider == "codex"
            && account.consent.enabled
            && account.consent.account_id == rule.account_id
            && account.consent.consented_at == rule.consented_at
    });
    let account: &LiveQuotaAccountView = accounts.next()?;
    if accounts.next().is_some() || account.unavailable.is_some() {
        return None;
    }
    let observed_at = account.observed_at?;
    if observed_at > now || now - observed_at > chrono::Duration::minutes(10) {
        return None;
    }
    let mut buckets = account
        .buckets
        .iter()
        .filter(|bucket| bucket.limit_id == rule.limit_id);
    let bucket = buckets.next()?;
    if buckets.next().is_some()
        || bucket.snapshot.provider != codex_provider_id()
        || bucket.snapshot.provenance != QuotaProvenance::LiveProvider
        || bucket.snapshot.unavailable.is_some()
    {
        return None;
    }
    let mut windows = bucket.snapshot.windows.iter().filter(|window| {
        window.unit == QuotaUnit::Percent
            && window.kind.as_str() == rule.window_kind
            && window.window_minutes == Some(rule.window_minutes)
    });
    let window = windows.next()?;
    if windows.next().is_some()
        || window.observed_at != observed_at
        || window.resets_at.is_some_and(|at| at <= now)
        || window.stale
        || window.unavailable.is_some()
    {
        return None;
    }
    let value = window.used?;
    (value.is_finite() && (0.0..=100.0).contains(&value)).then_some((value, observed_at))
}

/// Apply the existing edge/quiet/master policy to exact live identities. An
/// unknown reading leaves the prior armed key untouched, so an outage or
/// account switch cannot be mistaken for an observed drop below threshold.
pub fn evaluate_live_account_alerts(
    rules: &[LiveAccountBudget],
    status: &LiveQuotaStatus,
    settings: &NotificationSettings,
    log: &[NotificationLogEntry],
    now: DateTime<Utc>,
    current_local_hour: u8,
) -> (Vec<LiveAccountAlert>, Vec<NotificationLogEntry>) {
    let mut next_log = log.to_vec();
    let mut alerts = Vec::new();
    for rule in rules.iter().filter(|rule| rule.enabled).take(32) {
        let Some((value, observed_at)) = current_window(rule, status, now) else {
            continue;
        };
        let budget = QuotaBudget {
            id: live_account_log_key(&rule.id),
            provider: codex_provider_id(),
            project_key: None,
            unit: BudgetUnit::PercentOfWindow,
            window_kind: Some(rule.window_kind.clone()),
            period_hours: None,
            threshold: rule.threshold_percent,
            enabled: true,
        };
        let observed_local_hour = observed_at.with_timezone(&Local).hour() as u8;
        let mut effective_settings = settings.clone();
        if quota::in_quiet_hours(settings.quiet_hours, observed_local_hour) {
            // A reading captured during quiet hours is consumed even if the
            // collector first sees it after quiet hours end.
            effective_settings.enabled = false;
        }
        let (crossings, updated_log) = quota::evaluate_alerts(
            &[BudgetEvaluation {
                budget: &budget,
                current_value: Some(value),
            }],
            &effective_settings,
            &next_log,
            now,
            current_local_hour,
        );
        next_log = updated_log;
        alerts.extend(
            crossings
                .into_iter()
                .map(|alert| LiveAccountAlert { alert, observed_at }),
        );
    }
    (alerts, next_log)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota_accounts::{LiveQuotaBucketView, QuotaAccountConsent};
    use crate::quota_live::{LiveQuotaBucket, LiveQuotaWindow};

    fn fixture(
        now: DateTime<Utc>,
        used: f64,
    ) -> (LiveAccountBudget, LiveQuotaStatus, NotificationSettings) {
        let consented_at = now - chrono::Duration::days(1);
        let rule = LiveAccountBudget {
            id: "live-rule-1".into(),
            account_id: "synthetic-account".into(),
            consented_at,
            limit_id: "synthetic-limit".into(),
            window_kind: "burst".into(),
            window_minutes: 300,
            threshold_percent: 80.0,
            enabled: true,
        };
        let bucket = LiveQuotaBucket {
            limit_id: rule.limit_id.clone(),
            limit_name: None,
            spend_control_reached: None,
            primary: Some(LiveQuotaWindow {
                used_percent: used,
                window_minutes: Some(300),
                resets_at: None,
            }),
            secondary: None,
            credits: None,
        };
        let status = LiveQuotaStatus {
            accounts: vec![LiveQuotaAccountView {
                provider: "codex",
                consent: QuotaAccountConsent {
                    account_id: rule.account_id.clone(),
                    label: "Synthetic".into(),
                    consented_at,
                    enabled: true,
                },
                observed_at: Some(now),
                ordinary_usage_allowed: None,
                buckets: vec![LiveQuotaBucketView {
                    limit_id: rule.limit_id.clone(),
                    limit_name: None,
                    spend_control_reached: None,
                    snapshot: quota::live_bucket_snapshot(&bucket, now, now),
                }],
                unavailable: None,
            }],
            busy: false,
            configuration_error: None,
        };
        let settings = NotificationSettings {
            enabled: true,
            ..Default::default()
        };
        (rule, status, settings)
    }

    #[test]
    fn exact_live_identity_crosses_once_and_requires_observed_drop_to_rearm() {
        let now = Utc::now();
        let (rule, mut status, settings) = fixture(now, 85.0);
        let (first, log) = evaluate_live_account_alerts(
            std::slice::from_ref(&rule),
            &status,
            &settings,
            &[],
            now,
            12,
        );
        assert_eq!(first.len(), 1);
        assert_eq!(first[0].observed_at, now);
        assert_eq!(log[0].dedup_key, live_account_log_key(&rule.id));
        let (repeat, armed) = evaluate_live_account_alerts(
            std::slice::from_ref(&rule),
            &status,
            &settings,
            &log,
            now,
            12,
        );
        assert!(repeat.is_empty());
        status.accounts[0].unavailable = Some("account_changed");
        let (unknown, unchanged) = evaluate_live_account_alerts(
            std::slice::from_ref(&rule),
            &status,
            &settings,
            &armed,
            now,
            12,
        );
        assert!(unknown.is_empty());
        assert_eq!(unchanged, armed);
        let (_, mut status, _) = fixture(now, 25.0);
        let (below, rearmed) = evaluate_live_account_alerts(
            std::slice::from_ref(&rule),
            &status,
            &settings,
            &armed,
            now,
            12,
        );
        assert!(below.is_empty());
        assert!(rearmed.is_empty());
        status.accounts[0].buckets[0].snapshot.windows[0].used = Some(90.0);
        let (again, _) =
            evaluate_live_account_alerts(&[rule], &status, &settings, &rearmed, now, 12);
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn consent_generation_source_ambiguity_and_staleness_fail_closed() {
        let now = Utc::now();
        let (rule, status, settings) = fixture(now, 90.0);
        let assert_unavailable = |candidate: &LiveQuotaStatus| {
            let (alerts, log) = evaluate_live_account_alerts(
                std::slice::from_ref(&rule),
                candidate,
                &settings,
                &[],
                now,
                12,
            );
            assert!(alerts.is_empty());
            assert!(log.is_empty());
        };
        let mut changed = status;
        changed.accounts[0].consent.consented_at -= chrono::Duration::seconds(1);
        assert_unavailable(&changed);
        changed.accounts[0].consent.consented_at = rule.consented_at;
        changed.accounts[0].consent.enabled = false;
        assert_unavailable(&changed);
        changed.accounts[0].consent.enabled = true;
        changed.accounts[0].buckets[0].snapshot.provenance = QuotaProvenance::TranscriptDerived;
        assert_unavailable(&changed);
        changed.accounts[0].buckets[0].snapshot.provenance = QuotaProvenance::LiveProvider;
        let (_, mut duplicate, _) = fixture(now, 90.0);
        let original_bucket = changed.accounts[0].buckets[0].clone();
        changed.accounts[0].buckets = vec![
            original_bucket.clone(),
            duplicate.accounts[0].buckets.remove(0),
        ];
        assert_unavailable(&changed);
        changed.accounts[0].buckets = vec![original_bucket];
        changed.accounts[0].observed_at = Some(now - chrono::Duration::minutes(11));
        assert_unavailable(&changed);
        changed.accounts[0].observed_at = Some(now + chrono::Duration::seconds(1));
        assert_unavailable(&changed);
    }

    #[test]
    fn expired_wrong_window_and_disabled_policy_never_deliver_or_rearm() {
        let now = Utc::now();
        let (rule, mut status, mut settings) = fixture(now, 85.0);
        settings.enabled = false;
        let (disabled, armed) = evaluate_live_account_alerts(
            std::slice::from_ref(&rule),
            &status,
            &settings,
            &[],
            now,
            12,
        );
        assert!(disabled.is_empty());
        assert_eq!(armed.len(), 1);
        settings.enabled = true;
        let (enabled, _) = evaluate_live_account_alerts(
            std::slice::from_ref(&rule),
            &status,
            &settings,
            &armed,
            now,
            12,
        );
        assert!(
            enabled.is_empty(),
            "enabling shared delivery must not replay"
        );
        status.accounts[0].buckets[0].snapshot.windows[0].resets_at = Some(now);
        status.accounts[0].buckets[0].snapshot.windows[0].used = Some(1.0);
        let (expired, unchanged) = evaluate_live_account_alerts(
            std::slice::from_ref(&rule),
            &status,
            &settings,
            &armed,
            now,
            12,
        );
        assert!(expired.is_empty());
        assert_eq!(unchanged, armed, "expired low value cannot rearm");
        status.accounts[0].buckets[0].snapshot.windows[0].resets_at = None;
        status.accounts[0].buckets[0].snapshot.windows[0].window_minutes = Some(301);
        let (_, unchanged) =
            evaluate_live_account_alerts(&[rule], &status, &settings, &armed, now, 12);
        assert_eq!(
            unchanged, armed,
            "duration mismatch cannot borrow another window"
        );
    }

    #[test]
    fn observed_during_quiet_is_consumed_after_quiet_ends() {
        let now = Utc::now();
        let (rule, status, mut settings) = fixture(now, 85.0);
        let observed_hour = now.with_timezone(&Local).hour() as u8;
        settings.quiet_hours = Some((observed_hour, (observed_hour + 1) % 24));
        let after_quiet_hour = (observed_hour + 1) % 24;
        let (alerts, log) = evaluate_live_account_alerts(
            std::slice::from_ref(&rule),
            &status,
            &settings,
            &[],
            now,
            after_quiet_hour,
        );
        assert!(alerts.is_empty());
        assert_eq!(log.len(), 1);
        let (repeat, _) =
            evaluate_live_account_alerts(&[rule], &status, &settings, &log, now, after_quiet_hour);
        assert!(repeat.is_empty());
    }
}
