//! Local persistence for quota soft budgets, notification settings, and the
//! notification dedup log (issue #43).
//!
//! This is deliberately its own file with its own versioning, separate from
//! both `rates.rs` (the pricing/plan authority) and `history_store.rs` (the
//! durable usage ledger). Quota snapshots and the budgets/alerts derived
//! from them are provenance-bearing *external readings*, not usage facts —
//! folding them into the ledger would make the ledger's meaning ambiguous
//! (see `docs/ARCHITECTURE.md`'s durable-history contract and issue #43's
//! "Where quota state lives" scope note). If this ever needs to grow beyond
//! a small JSON document (e.g. a long-running forecast history), it gets its
//! own store and migrations independent of `history-v1.sqlite3`, not a
//! table added to it.
//!
//! Stored at `<config_dir>/agent-odometer/quota-v3.json` using the same
//! atomic write pattern as `rates.rs::RateCard::save` (temp file + rename).

use crate::provider::ProviderId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const QUOTA_STORE_VERSION: u32 = 3;
pub const LIVE_ACCOUNT_LOG_PREFIX: &str = "live-account:";

/// Keeps the dedup log bounded regardless of how long the app runs.
const MAX_LOG_ENTRIES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetUnit {
    /// Percent used (0-100) of a provider-reported, account-wide quota
    /// window (`quota::QuotaWindow.used`). Provider-scoped only — an
    /// account-wide figure cannot be meaningfully split per project, so
    /// `QuotaBudget::project_key` must be `None` for this unit (validated
    /// in `commands.rs::validate_quota_config`).
    PercentOfWindow,
    /// Raw token count, summed over the budget's rolling `period_hours`.
    Tokens,
    /// Current USD API estimate from the shared query service, not a bill
    /// or subscription allowance. Incomplete pricing is unavailable.
    Usd,
}

fn default_true() -> bool {
    true
}

/// One soft (advisory-only — hard enforcement is issue #46) usage budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaBudget {
    pub id: String,
    pub provider: ProviderId,
    /// `None` = provider-wide; `Some(project_key)` scopes it to one project
    /// (#41's `project_key`). Valid with token and USD estimate budgets.
    #[serde(default)]
    pub project_key: Option<String>,
    pub unit: BudgetUnit,
    /// Which window this budget watches, matching
    /// `quota::QuotaWindowKind::as_str()` ("burst", "daily", "weekly",
    /// "monthly"). Required for `PercentOfWindow`; ignored for token/USD budgets.
    #[serde(default)]
    pub window_kind: Option<String>,
    /// Rolling period for a token or USD estimate budget. Ignored for `PercentOfWindow`,
    /// whose period is the provider's own window.
    #[serde(default)]
    pub period_hours: Option<u32>,
    /// Percent-used (0-100) for `PercentOfWindow`, or a raw token count for
    /// `Tokens`.
    pub threshold: f64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

/// One explicit, consent-generation-bound live-provider alert rule. This is
/// separate from transcript budgets, which have no reliable account identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveAccountBudget {
    pub id: String,
    pub account_id: String,
    pub consented_at: DateTime<Utc>,
    pub limit_id: String,
    pub window_kind: String,
    pub window_minutes: u64,
    pub threshold_percent: f64,
    #[serde(default)]
    pub enabled: bool,
}

pub fn live_account_log_key(id: &str) -> String {
    format!("{LIVE_ACCOUNT_LOG_PREFIX}{id}")
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationSettings {
    /// Opt-in: no alert is ever surfaced while this is false. Crossings are
    /// still tracked (see `quota::evaluate_alerts`) so enabling this later
    /// never dumps a backlog of alerts for crossings that already happened.
    #[serde(default)]
    pub enabled: bool,
    /// Local-hour `[start, end)` range (0-23) during which alerts are
    /// tracked but not surfaced. `start > end` wraps past midnight.
    #[serde(default)]
    pub quiet_hours: Option<(u8, u8)>,
    /// New categories are off by default; the existing master gates all delivery.
    #[serde(default)]
    pub ambient: crate::ambient::Categories,
}

/// One armed budget crossing. Existence of an entry means "already
/// notified (or would have, but for quiet hours/disabled) for this budget's
/// current crossing" — see `quota::evaluate_alerts` for the arm/re-arm
/// semantics that make this both deduplicated and reset-aware.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationLogEntry {
    pub dedup_key: String,
    pub fired_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<crate::ambient::Notice>,
}

fn quota_store_version() -> u32 {
    QUOTA_STORE_VERSION
}

fn default_max_cache_age_secs() -> i64 {
    6 * 60 * 60 // 6 hours
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaStoreFile {
    #[serde(default = "quota_store_version")]
    pub version: u32,
    #[serde(default)]
    pub budgets: Vec<QuotaBudget>,
    #[serde(default)]
    pub live_account_budgets: Vec<LiveAccountBudget>,
    #[serde(default)]
    pub notifications: NotificationSettings,
    #[serde(default)]
    pub notification_log: Vec<NotificationLogEntry>,
    /// How old a quota observation can be before it is flagged `stale`
    /// (see `quota::QuotaWindow.stale`). Not currently user-editable
    /// through a dedicated field in the wire config beyond what
    /// `set_quota_config` accepts; kept alongside the budgets it gates so
    /// `QuotaConfigWire` maps to this file 1:1.
    #[serde(default = "default_max_cache_age_secs")]
    pub max_cache_age_secs: i64,
}

impl Default for QuotaStoreFile {
    fn default() -> Self {
        Self {
            version: QUOTA_STORE_VERSION,
            budgets: Vec::new(),
            live_account_budgets: Vec::new(),
            notifications: NotificationSettings::default(),
            notification_log: Vec::new(),
            max_cache_age_secs: default_max_cache_age_secs(),
        }
    }
}

fn quota_store_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("agent-odometer").join("quota-v3.json"))
}

impl QuotaStoreFile {
    /// Loads from disk, falling back to defaults for a missing, unreadable,
    /// or malformed file (logged, never a hard failure — quota bookkeeping
    /// must not block the rest of the app from starting).
    pub fn load() -> Self {
        Self::load_checked().unwrap_or_else(|_| {
            tracing::warn!("quota configuration unavailable; automatic quota activity disabled");
            Self::default()
        })
    }

    /// Preserve both legacy files so older releases cannot overwrite live
    /// account rules they do not understand. Reads alone never migrate/write.
    pub fn load_checked() -> Result<Self, String> {
        let path = quota_store_path().ok_or("quota configuration location unavailable")?;
        Self::load_at(&path)
    }

    fn load_at(path: &std::path::Path) -> Result<Self, String> {
        use std::io::Read;
        let v2 = path.with_file_name("quota-v2.json");
        let v1 = path.with_file_name("quota-v1.json");
        let selected = if path
            .try_exists()
            .map_err(|_| "quota configuration unreadable; existing file preserved")?
        {
            path
        } else if v2
            .try_exists()
            .map_err(|_| "quota configuration unreadable; existing file preserved")?
        {
            &v2
        } else {
            &v1
        };
        let file = match std::fs::File::open(selected) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(_) => return Err("quota configuration unreadable; existing file preserved".into()),
        };
        let mut raw = String::new();
        file.take(512_001)
            .read_to_string(&mut raw)
            .map_err(|_| "quota configuration unreadable; existing file preserved")?;
        if raw.len() > 512_000 {
            return Err("quota configuration is too large; existing file preserved".into());
        }
        if selected == path {
            let value: serde_json::Value = serde_json::from_str(&raw)
                .map_err(|_| "quota configuration invalid; existing file preserved")?;
            let required = [
                "version",
                "budgets",
                "live_account_budgets",
                "notifications",
                "notification_log",
                "max_cache_age_secs",
            ];
            if required.iter().any(|key| value.get(key).is_none()) {
                return Err("quota configuration invalid; existing file preserved".into());
            }
        }
        let mut store: Self = serde_json::from_str(&raw)
            .map_err(|_| "quota configuration invalid; existing file preserved")?;
        let valid_version = if selected == path {
            store.version == QUOTA_STORE_VERSION
        } else if selected == v2 {
            store.version == 2
        } else {
            store.version == 1
        };
        if !valid_version {
            return Err("quota configuration requires a newer Odometer version".into());
        }
        if selected != path {
            // Legacy formats never authorized live-account alert rules.
            // Unknown extension fields cannot silently opt in during migration.
            store.live_account_budgets.clear();
        }
        validate_quota_config(&QuotaConfigWire::from(&store))?;
        store.version = QUOTA_STORE_VERSION;
        Ok(store)
    }

    /// Atomic-ish write to `<config_dir>/agent-odometer/quota-v3.json`.
    pub fn save(&self) -> anyhow::Result<()> {
        let path =
            quota_store_path().ok_or_else(|| anyhow::anyhow!("could not determine config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        let serialized = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, &serialized)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Bounds the dedup log by both age and count so long-running installs
    /// cannot grow it unboundedly.
    pub fn prune_log(&mut self, now: DateTime<Utc>, retention: chrono::Duration) {
        let armed_live_keys: std::collections::HashSet<_> = self
            .live_account_budgets
            .iter()
            .map(|rule| live_account_log_key(&rule.id))
            .collect();
        let mut seen_live = std::collections::HashSet::new();
        self.notification_log.retain(|entry| {
            if armed_live_keys.contains(&entry.dedup_key) {
                // Unknown observations never resolve a crossing. Configured
                // live keys are bounded by 32 rules, independent of log age.
                seen_live.insert(entry.dedup_key.clone())
            } else {
                now.signed_duration_since(entry.fired_at) <= retention
            }
        });
        self.notification_log.sort_by_key(|entry| entry.fired_at);
        let mut excess = self.notification_log.len().saturating_sub(MAX_LOG_ENTRIES);
        self.notification_log.retain(|entry| {
            if excess > 0 && !armed_live_keys.contains(&entry.dedup_key) {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }
}

/// Wire shape for `get_quota_config`/`set_quota_config`: everything in
/// `QuotaStoreFile` except the notification dedup log, which is internal
/// bookkeeping the frontend never needs to see or round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaConfigWire {
    /// Optimistic edit revision; excludes automatic notification bookkeeping.
    #[serde(default)]
    pub revision: Option<String>,
    pub budgets: Vec<QuotaBudget>,
    #[serde(default)]
    pub live_account_budgets: Vec<LiveAccountBudget>,
    pub notifications: NotificationSettings,
    pub max_cache_age_secs: i64,
}

impl QuotaStoreFile {
    pub fn config_revision(&self) -> String {
        let bytes = serde_json::to_vec(&(
            &self.budgets,
            &self.live_account_budgets,
            &self.notifications,
            self.max_cache_age_secs,
        ))
        .expect("validated quota settings serialize");
        format!("{:016x}", crate::stable_hash::fnv1a64(&bytes))
    }

    pub fn check_revision(&self, revision: Option<&str>) -> Result<(), String> {
        if revision.is_some_and(|revision| revision != self.config_revision()) {
            return Err("Quota settings changed. Reload before saving your edits.".into());
        }
        Ok(())
    }
}

impl From<&QuotaStoreFile> for QuotaConfigWire {
    fn from(store: &QuotaStoreFile) -> Self {
        Self {
            revision: Some(store.config_revision()),
            budgets: store.budgets.clone(),
            live_account_budgets: store.live_account_budgets.clone(),
            notifications: store.notifications.clone(),
            max_cache_age_secs: store.max_cache_age_secs,
        }
    }
}

/// Validates a candidate config before it is persisted. Fail-closed: the
/// caller must not write a config that fails this.
pub fn validate_quota_config(config: &QuotaConfigWire) -> Result<(), String> {
    if !(1..=31_536_000).contains(&config.max_cache_age_secs) {
        return Err("max_cache_age_secs must be between 1 and 31536000".to_string());
    }
    if config.budgets.len() > 64 {
        return Err("at most 64 soft budgets are supported".into());
    }
    if config.live_account_budgets.len() > 32 {
        return Err("at most 32 live account alert rules are supported".into());
    }
    if config
        .notifications
        .quiet_hours
        .is_some_and(|(start, end)| start > 23 || end > 23)
    {
        return Err("quiet hours must be local hours from 0 to 23".into());
    }
    let mut seen_ids = std::collections::HashSet::new();
    for budget in &config.budgets {
        if crate::provider::ProviderRegistry::builtin()
            .adapter(&budget.provider)
            .is_none()
        {
            return Err("budget provider is not supported".into());
        }
        if budget.project_key.as_ref().is_some_and(|key| {
            key.trim().is_empty() || key.len() > 1024 || key.chars().any(char::is_control)
        }) {
            return Err("budget project key is invalid".into());
        }
        if budget.id.trim().is_empty()
            || budget.id.len() > 128
            || budget.id.starts_with(crate::ambient::PREFIX)
            || budget.id.starts_with(LIVE_ACCOUNT_LOG_PREFIX)
        {
            return Err("budget id must contain 1 to 128 bytes".to_string());
        }
        if !seen_ids.insert(budget.id.as_str()) {
            return Err(format!("duplicate budget id '{}'", budget.id));
        }
        if !budget.threshold.is_finite() || budget.threshold <= 0.0 {
            return Err(format!("budget '{}' threshold must be positive", budget.id));
        }
        match budget.unit {
            BudgetUnit::PercentOfWindow => {
                if budget.project_key.is_some() {
                    return Err(format!(
                        "budget '{}' is percent_of_window and account-wide; it cannot be scoped to a project",
                        budget.id
                    ));
                }
                if !matches!(
                    budget.window_kind.as_deref(),
                    Some("burst" | "daily" | "weekly" | "monthly")
                ) {
                    return Err(format!(
                        "budget '{}' is percent_of_window and must name a window_kind",
                        budget.id
                    ));
                }
                if budget.threshold > 100.0 {
                    return Err(format!(
                        "budget '{}' is percent_of_window and threshold must be <= 100",
                        budget.id
                    ));
                }
            }
            BudgetUnit::Tokens | BudgetUnit::Usd => {
                if let Some(hours) = budget.period_hours {
                    if !(1..=8760).contains(&hours) {
                        return Err(format!(
                            "budget '{}' period_hours must be between 1 and 8760",
                            budget.id
                        ));
                    }
                }
            }
        }
    }
    let mut live_ids = std::collections::HashSet::new();
    let mut live_windows = std::collections::HashSet::new();
    for budget in &config.live_account_budgets {
        if budget.id.trim().is_empty()
            || budget.id.len() > 128
            || budget.id.chars().any(char::is_control)
            || budget.account_id.is_empty()
            || budget.account_id.len() > 256
            || budget.account_id.chars().any(char::is_control)
            || budget.limit_id.is_empty()
            || budget.limit_id.len() > 128
            || budget.limit_id.chars().any(char::is_control)
            || !matches!(
                budget.window_kind.as_str(),
                "burst" | "daily" | "weekly" | "monthly"
            )
            || !(1..=527_040).contains(&budget.window_minutes)
            || !budget.threshold_percent.is_finite()
            || !(0.0..=100.0).contains(&budget.threshold_percent)
            || budget.threshold_percent == 0.0
            || !live_ids.insert(&budget.id)
            || !live_windows.insert((
                &budget.account_id,
                budget.consented_at,
                &budget.limit_id,
                &budget.window_kind,
                budget.window_minutes,
            ))
        {
            return Err("live account alert rule is invalid or duplicated".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::codex_provider_id;

    fn wire(budgets: Vec<QuotaBudget>) -> QuotaConfigWire {
        QuotaConfigWire {
            revision: None,
            budgets,
            live_account_budgets: Vec::new(),
            notifications: NotificationSettings::default(),
            max_cache_age_secs: 3600,
        }
    }

    fn percent_budget() -> QuotaBudget {
        QuotaBudget {
            id: "b1".into(),
            provider: codex_provider_id(),
            project_key: None,
            unit: BudgetUnit::PercentOfWindow,
            window_kind: Some("burst".into()),
            period_hours: None,
            threshold: 80.0,
            enabled: true,
        }
    }

    #[test]
    fn legacy_migration_preserves_ambient_policy_and_never_falls_back_from_bad_v3() {
        let dir = tempfile::tempdir().unwrap();
        let v1 = dir.path().join("quota-v1.json");
        let v2 = dir.path().join("quota-v2.json");
        let v3 = dir.path().join("quota-v3.json");
        let legacy = r#"{"version":1,"budgets":[],"notifications":{"enabled":true}}"#;
        std::fs::write(&v1, legacy).unwrap();
        let from_v1 = QuotaStoreFile::load_at(&v3).unwrap();
        assert_eq!(from_v1.version, 3);
        assert!(from_v1.notifications.enabled);
        assert!(!v3.exists());
        assert_eq!(std::fs::read_to_string(&v1).unwrap(), legacy);

        let now = Utc::now();
        let mut old_store = QuotaStoreFile {
            version: 2,
            ..Default::default()
        };
        old_store.notifications.ambient.attention = true;
        old_store.notification_log.push(NotificationLogEntry {
            dedup_key: "@ambient/condition".into(),
            fired_at: now,
            notice: Some(crate::ambient::Notice {
                id: "@ambient/recent".into(),
                route: crate::ambient::Route::Attention,
                provider: "Codex".into(),
                code: "attention".into(),
                observed_at: now,
                delivered_at: now,
            }),
        });
        let mut old_value = serde_json::to_value(&old_store).unwrap();
        old_value
            .as_object_mut()
            .unwrap()
            .remove("live_account_budgets");
        let v2_bytes = serde_json::to_vec(&old_value).unwrap();
        std::fs::write(&v2, &v2_bytes).unwrap();
        let migrated = QuotaStoreFile::load_at(&v3).unwrap();
        assert_eq!(migrated.version, 3);
        assert!(migrated.notifications.ambient.attention);
        assert_eq!(migrated.notification_log, old_store.notification_log);
        assert!(migrated.live_account_budgets.is_empty());
        assert!(!v3.exists());
        let mut extended_v2 = old_value.clone();
        extended_v2["live_account_budgets"] = serde_json::json!([{
            "id": "unrecognized-extension", "account_id": "synthetic-account",
            "consented_at": "2026-10-01T00:00:00Z", "limit_id": "synthetic-limit",
            "window_kind": "burst", "window_minutes": 300,
            "threshold_percent": 80.0, "enabled": true
        }]);
        std::fs::write(&v2, serde_json::to_vec(&extended_v2).unwrap()).unwrap();
        assert!(QuotaStoreFile::load_at(&v3)
            .unwrap()
            .live_account_budgets
            .is_empty());
        assert!(!v3.exists(), "migration must remain read-only");

        // A new active file wins over any subsequent write by an old binary.
        std::fs::write(&v3, serde_json::to_vec(&migrated).unwrap()).unwrap();
        std::fs::write(&v2, legacy).unwrap();
        assert_eq!(
            QuotaStoreFile::load_at(&v3).unwrap().notification_log,
            old_store.notification_log
        );
        for invalid in [r#"{"version":99}"#, "{}", "not json"] {
            std::fs::write(&v3, invalid).unwrap();
            assert!(QuotaStoreFile::load_at(&v3).is_err());
            assert_eq!(std::fs::read_to_string(&v3).unwrap(), invalid);
            assert_eq!(std::fs::read_to_string(&v2).unwrap(), legacy);
        }
    }

    #[test]
    fn stale_config_edits_fail_without_conflicting_with_alert_bookkeeping() {
        let mut store = QuotaStoreFile::default();
        let revision = store.config_revision();
        store.notification_log.push(NotificationLogEntry {
            dedup_key: "b1".into(),
            fired_at: Utc::now(),
            notice: None,
        });
        assert!(store.check_revision(Some(&revision)).is_ok());
        store.budgets.push(percent_budget());
        assert!(store.check_revision(Some(&revision)).is_err());
        assert!(store.check_revision(Some(&store.config_revision())).is_ok());
    }

    #[test]
    fn valid_config_passes() {
        assert!(validate_quota_config(&wire(vec![percent_budget()])).is_ok());
    }

    #[test]
    fn unreadable_newer_and_oversized_v3_never_fall_back_to_legacy() {
        let dir = tempfile::tempdir().unwrap();
        let v3 = dir.path().join("quota-v3.json");
        let v2 = dir.path().join("quota-v2.json");
        let mut legacy = QuotaStoreFile {
            version: 2,
            ..Default::default()
        };
        legacy.notifications.enabled = true;
        std::fs::write(&v2, serde_json::to_vec(&legacy).unwrap()).unwrap();
        std::fs::create_dir(&v3).unwrap();
        assert!(QuotaStoreFile::load_at(&v3).is_err());
        std::fs::remove_dir(&v3).unwrap();
        let newer = QuotaStoreFile {
            version: 4,
            ..Default::default()
        };
        let newer_bytes = serde_json::to_vec(&newer).unwrap();
        std::fs::write(&v3, &newer_bytes).unwrap();
        assert!(QuotaStoreFile::load_at(&v3).unwrap_err().contains("newer"));
        assert_eq!(std::fs::read(&v3).unwrap(), newer_bytes);
        std::fs::write(&v3, " ".repeat(512_001)).unwrap();
        assert!(QuotaStoreFile::load_at(&v3).is_err());
        assert_eq!(std::fs::metadata(&v3).unwrap().len(), 512_001);
    }

    #[test]
    fn configured_live_crossing_survives_unknown_beyond_retention_and_log_pressure() {
        let now = Utc::now();
        let old = now - chrono::Duration::days(40);
        let rule: LiveAccountBudget = serde_json::from_value(serde_json::json!({
            "id": "synthetic", "account_id": "account", "consented_at": "2026-10-01T00:00:00Z",
            "limit_id": "model", "window_kind": "burst", "window_minutes": 300, "threshold_percent": 80.0, "enabled": true
        })).unwrap();
        let mut store = QuotaStoreFile::default();
        store.live_account_budgets.push(rule.clone());
        store.notification_log = (0..600)
            .map(|i| NotificationLogEntry {
                dedup_key: format!("ordinary-{i}"),
                fired_at: now,
                notice: None,
            })
            .collect();
        store.notification_log.push(NotificationLogEntry {
            dedup_key: live_account_log_key(&rule.id),
            fired_at: old,
            notice: None,
        });
        store.prune_log(now, chrono::Duration::days(30));
        assert_eq!(store.notification_log.len(), 500);
        let budget = QuotaBudget {
            id: live_account_log_key(&rule.id),
            provider: codex_provider_id(),
            project_key: None,
            unit: BudgetUnit::PercentOfWindow,
            window_kind: Some("burst".into()),
            period_hours: None,
            threshold: 80.0,
            enabled: true,
        };
        let settings = NotificationSettings {
            enabled: true,
            ..Default::default()
        };
        let (unknown, log) = crate::quota::evaluate_alerts(
            &[crate::quota::BudgetEvaluation {
                budget: &budget,
                current_value: None,
            }],
            &settings,
            &store.notification_log,
            now,
            12,
        );
        assert!(unknown.is_empty());
        let (again, _) = crate::quota::evaluate_alerts(
            &[crate::quota::BudgetEvaluation {
                budget: &budget,
                current_value: Some(90.0),
            }],
            &settings,
            &log,
            now,
            12,
        );
        assert!(
            again.is_empty(),
            "retention cannot turn unknown into an observed resolution"
        );
        store.live_account_budgets.clear();
        store.prune_log(now, chrono::Duration::days(30));
        assert!(!store
            .notification_log
            .iter()
            .any(|entry| entry.dedup_key == budget.id));
    }

    #[test]
    fn live_rules_default_off_and_reserved_keys_cannot_collide() {
        let mut rule: LiveAccountBudget = serde_json::from_value(serde_json::json!({
            "id": "synthetic", "account_id": "account", "consented_at": "2026-10-01T00:00:00Z",
            "limit_id": "model", "window_kind": "burst", "window_minutes": 300, "threshold_percent": 80.0
        })).unwrap();
        assert!(!rule.enabled);
        let mut config = wire(vec![]);
        config.live_account_budgets.push(rule.clone());
        assert!(validate_quota_config(&config).is_ok());
        config.live_account_budgets.push(rule.clone());
        assert!(validate_quota_config(&config).is_err());
        config.live_account_budgets.pop();
        rule.threshold_percent = f64::NAN;
        config.live_account_budgets[0] = rule;
        assert!(validate_quota_config(&config).is_err());
        let mut config = wire(vec![QuotaBudget {
            id: live_account_log_key("synthetic"),
            provider: codex_provider_id(),
            project_key: None,
            unit: BudgetUnit::Tokens,
            window_kind: None,
            period_hours: Some(24),
            threshold: 1.0,
            enabled: true,
        }]);
        assert!(validate_quota_config(&config).is_err());
        config.budgets[0].id = "ordinary".into();
        assert!(validate_quota_config(&config).is_ok());
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let mut b2 = percent_budget();
        b2.threshold = 50.0;
        let result = validate_quota_config(&wire(vec![percent_budget(), b2]));
        assert!(result.is_err());
    }

    #[test]
    fn percent_of_window_budgets_cannot_be_project_scoped() {
        let mut b = percent_budget();
        b.project_key = Some("proj:abc".into());
        assert!(validate_quota_config(&wire(vec![b])).is_err());
    }

    #[test]
    fn percent_of_window_budgets_require_a_window_kind() {
        let mut b = percent_budget();
        b.window_kind = None;
        assert!(validate_quota_config(&wire(vec![b])).is_err());
    }

    #[test]
    fn percent_of_window_threshold_over_100_is_rejected() {
        let mut b = percent_budget();
        b.threshold = 150.0;
        assert!(validate_quota_config(&wire(vec![b])).is_err());
    }

    #[test]
    fn token_budgets_allow_project_scoping() {
        let b = QuotaBudget {
            id: "b2".into(),
            provider: codex_provider_id(),
            project_key: Some("proj:abc".into()),
            unit: BudgetUnit::Tokens,
            window_kind: None,
            period_hours: Some(24),
            threshold: 500_000.0,
            enabled: true,
        };
        assert!(validate_quota_config(&wire(vec![b])).is_ok());
    }

    #[test]
    fn nonpositive_threshold_is_rejected() {
        let mut b = percent_budget();
        b.threshold = 0.0;
        assert!(validate_quota_config(&wire(vec![b])).is_err());
    }

    // `save`/`load` are deliberately not exercised against the real
    // filesystem here (matching `rates.rs`, which has no such test either):
    // `dirs::config_dir()` resolves the actual OS profile directory and
    // cannot be safely redirected per-test on every platform. Serialization
    // round-tripping (the part that is platform-independent and safe to
    // test) is covered below; the temp-file-plus-rename write path mirrors
    // `RateCard::save`, already exercised in production use.
    #[test]
    fn store_serializes_and_deserializes_losslessly() {
        let mut store = QuotaStoreFile::default();
        store.budgets.push(percent_budget());
        store.notifications.enabled = true;
        store.notification_log.push(NotificationLogEntry {
            dedup_key: "b1".into(),
            fired_at: Utc::now(),
            notice: None,
        });

        let serialized = serde_json::to_string_pretty(&store).unwrap();
        let restored: QuotaStoreFile = serde_json::from_str(&serialized).unwrap();
        assert_eq!(restored, store);
    }

    #[test]
    fn missing_fields_default_for_forward_compatibility() {
        let minimal = "{}";
        let restored: QuotaStoreFile = serde_json::from_str(minimal).unwrap();
        assert_eq!(restored.version, QUOTA_STORE_VERSION);
        assert!(restored.budgets.is_empty());
        assert!(!restored.notifications.enabled);
        assert_eq!(restored.max_cache_age_secs, default_max_cache_age_secs());
    }

    #[test]
    fn prune_log_bounds_by_age_and_count() {
        let mut store = QuotaStoreFile::default();
        let now = Utc::now();
        for i in 0..(MAX_LOG_ENTRIES + 10) {
            store.notification_log.push(NotificationLogEntry {
                dedup_key: format!("k{i}"),
                fired_at: now,
                notice: None,
            });
        }
        store.notification_log.push(NotificationLogEntry {
            dedup_key: "ancient".into(),
            fired_at: now - chrono::Duration::days(60),
            notice: None,
        });
        store.prune_log(now, chrono::Duration::days(30));
        assert!(store.notification_log.len() <= MAX_LOG_ENTRIES);
        assert!(!store
            .notification_log
            .iter()
            .any(|e| e.dedup_key == "ancient"));
    }
}
