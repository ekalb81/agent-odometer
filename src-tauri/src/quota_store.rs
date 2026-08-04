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
//! Stored at `<config_dir>/agent-odometer/quota-v1.json` using the same
//! atomic write pattern as `rates.rs::RateCard::save` (temp file + rename).

use crate::provider::ProviderId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const QUOTA_STORE_VERSION: u32 = 1;

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
    /// Deliberately not dollar-denominated: pricing/model-resolution is
    /// owned by the frontend's `credits.ts` (see `docs/ARCHITECTURE.md`'s
    /// pricing-catalog contract — "the dashboard remains the owner of
    /// interactive and date-scoped pricing"), so re-deriving it inside this
    /// Rust service would create a second, driftable pricing
    /// implementation. A dollar-denominated project budget is deferred;
    /// see the PR description.
    Tokens,
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
    /// (#41's `project_key`). Only valid with `unit: Tokens`.
    #[serde(default)]
    pub project_key: Option<String>,
    pub unit: BudgetUnit,
    /// Which window this budget watches, matching
    /// `quota::QuotaWindowKind::as_str()` ("burst", "daily", "weekly",
    /// "monthly"). Required for `PercentOfWindow`; ignored for `Tokens`.
    #[serde(default)]
    pub window_kind: Option<String>,
    /// Rolling period for a `Tokens` budget. Ignored for `PercentOfWindow`,
    /// whose period is the provider's own window.
    #[serde(default)]
    pub period_hours: Option<u32>,
    /// Percent-used (0-100) for `PercentOfWindow`, or a raw token count for
    /// `Tokens`.
    pub threshold: f64,
    #[serde(default = "default_true")]
    pub enabled: bool,
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
}

/// One armed budget crossing. Existence of an entry means "already
/// notified (or would have, but for quiet hours/disabled) for this budget's
/// current crossing" — see `quota::evaluate_alerts` for the arm/re-arm
/// semantics that make this both deduplicated and reset-aware.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationLogEntry {
    pub dedup_key: String,
    pub fired_at: DateTime<Utc>,
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
            notifications: NotificationSettings::default(),
            notification_log: Vec::new(),
            max_cache_age_secs: default_max_cache_age_secs(),
        }
    }
}

fn quota_store_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("agent-odometer").join("quota-v1.json"))
}

impl QuotaStoreFile {
    /// Loads from disk, falling back to defaults for a missing, unreadable,
    /// or malformed file (logged, never a hard failure — quota bookkeeping
    /// must not block the rest of the app from starting).
    pub fn load() -> Self {
        let Some(path) = quota_store_path() else {
            return Self::default();
        };
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Self>(&raw) {
                Ok(mut store) => {
                    store.version = QUOTA_STORE_VERSION;
                    store
                }
                Err(error) => {
                    tracing::warn!(
                        "quota-v1.json at {:?} is malformed ({}); using defaults",
                        path,
                        error
                    );
                    Self::default()
                }
            },
            Err(error) => {
                tracing::warn!(
                    "could not read quota-v1.json at {:?} ({}); using defaults",
                    path,
                    error
                );
                Self::default()
            }
        }
    }

    /// Atomic-ish write to `<config_dir>/agent-odometer/quota-v1.json`.
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
        self.notification_log
            .retain(|entry| now.signed_duration_since(entry.fired_at) <= retention);
        if self.notification_log.len() > MAX_LOG_ENTRIES {
            let excess = self.notification_log.len() - MAX_LOG_ENTRIES;
            self.notification_log.drain(0..excess);
        }
    }
}

/// Wire shape for `get_quota_config`/`set_quota_config`: everything in
/// `QuotaStoreFile` except the notification dedup log, which is internal
/// bookkeeping the frontend never needs to see or round-trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuotaConfigWire {
    pub budgets: Vec<QuotaBudget>,
    pub notifications: NotificationSettings,
    pub max_cache_age_secs: i64,
}

impl From<&QuotaStoreFile> for QuotaConfigWire {
    fn from(store: &QuotaStoreFile) -> Self {
        Self {
            budgets: store.budgets.clone(),
            notifications: store.notifications.clone(),
            max_cache_age_secs: store.max_cache_age_secs,
        }
    }
}

/// Validates a candidate config before it is persisted. Fail-closed: the
/// caller must not write a config that fails this.
pub fn validate_quota_config(config: &QuotaConfigWire) -> Result<(), String> {
    if config.max_cache_age_secs <= 0 {
        return Err("max_cache_age_secs must be positive".to_string());
    }
    let mut seen_ids = std::collections::HashSet::new();
    for budget in &config.budgets {
        if budget.id.trim().is_empty() {
            return Err("budget id must not be empty".to_string());
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
                if budget.window_kind.is_none() {
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
            BudgetUnit::Tokens => {
                if let Some(hours) = budget.period_hours {
                    if hours == 0 {
                        return Err(format!(
                            "budget '{}' period_hours must be positive when set",
                            budget.id
                        ));
                    }
                }
            }
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
            budgets,
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
    fn valid_config_passes() {
        assert!(validate_quota_config(&wire(vec![percent_budget()])).is_ok());
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
            });
        }
        store.notification_log.push(NotificationLogEntry {
            dedup_key: "ancient".into(),
            fired_at: now - chrono::Duration::days(60),
        });
        store.prune_log(now, chrono::Duration::days(30));
        assert!(store.notification_log.len() <= MAX_LOG_ENTRIES);
        assert!(!store
            .notification_log
            .iter()
            .any(|e| e.dedup_key == "ancient"));
    }
}
