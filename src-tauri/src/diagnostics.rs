//! Provider-registry diagnostics (issue #39): one report, per provider,
//! covering capability flags, configured/default roots and their existence,
//! discovery/parse counters, ledger health, pricing coverage, transcript-
//! retention risk, and quota-source status.
//!
//! This module deliberately never walks the filesystem for transcripts or
//! parses anything. It reads:
//! - registry metadata (`crate::provider`)
//! - the last completed scan's counters (`AppState::last_scan_summary`,
//!   populated at the existing `scanner::scan_all` call site)
//! - the in-memory session projection already hydrated by scans/the watcher
//!   (`AppState::sessions`)
//! - the loaded rate card (`crate::rates::RateCard`)
//! - cheap `Path::exists()` checks on the small, bounded set of configured
//!   roots
//!
//! `evaluate_state` (and the `ProviderDiagnosticInput` it consumes) are kept
//! separate from `generate_report` so provider-health resolution is
//! exhaustively unit testable with synthetic inputs — missing roots,
//! unreadable files, mixed valid/invalid records, estimated tokens, stale
//! rates, disabled capabilities, healthy operation — without touching the
//! real filesystem, `AppState`, or the builtin registry. See
//! `tests/diagnostics_tests.rs` for those fixtures.

use crate::config::Config;
use crate::model::SourceAvailability;
use crate::provider::{ProviderCapabilities, ProviderId, ProviderRegistry};
use crate::rates::RateCard;
use crate::scan_cache::ColdReason;
use crate::store::AppState;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Bounds sample sizes in the report so it stays fast and cannot grow
/// unboundedly for a corpus with many distinct model strings.
const MAX_MODEL_SAMPLE: usize = 25;
/// A rate card whose `fetched_at` is older than this is flagged as
/// potentially stale pricing. `fetched_at: None` (the bundled default) is not
/// flagged: it has no fetch history to compare against.
const RATES_STALE_AFTER_DAYS: i64 = 180;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderHealthState {
    Ready,
    Degraded,
    Unsupported,
    NotDetected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticReason {
    /// Machine-stable identifier for tooling/tests.
    pub code: String,
    /// Human-readable, deliberately generic text. Never interpolates a path,
    /// prompt, or other user content, so it never needs redaction on export.
    pub message: String,
}

fn reason(code: &str, message: &str) -> DiagnosticReason {
    DiagnosticReason {
        code: code.to_string(),
        message: message.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    Live,
    Archive,
    SessionIndex,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiagnosticRoot {
    pub kind: RootKind,
    pub path: PathBuf,
    pub exists: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct DiscoveryHealth {
    pub discovered_files: u64,
    pub parsed_files: u64,
    pub skipped_files: u64,
    pub parse_failures: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct LedgerHealth {
    pub history_store_available: bool,
    pub durable_sessions: u64,
    pub available_sessions: u64,
    pub collision_sessions: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PricingHealth {
    pub models_observed: usize,
    pub models_priced: usize,
    /// Bounded sample of used models known to have no published price
    /// (excluded from cost estimates rather than priced with a fallback).
    pub unpriced_models_used: Vec<String>,
    /// Bounded sample of used models priced only via the harness fallback
    /// rate because they are entirely unknown to the rate card.
    pub fallback_models_used: Vec<String>,
    pub fallback_used: bool,
    pub rates_fetched_at: Option<String>,
    pub rates_stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RetentionRiskLevel {
    None,
    Moderate,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RetentionHealth {
    pub level: RetentionRiskLevel,
    pub supports_archive: bool,
    pub archive_roots_configured: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaStatus {
    NotAvailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuotaHealth {
    pub status: QuotaStatus,
    pub reason_code: String,
    pub message: String,
}

/// There is no quota/rate-limit API implementation yet for any provider.
/// Always reports `not_available` with a fixed reason rather than inventing
/// one from transcript rate-limit snapshots (those already power the
/// separate subscription-usage view and turn receipts).
fn quota_health() -> QuotaHealth {
    QuotaHealth {
        status: QuotaStatus::NotAvailable,
        reason_code: "quota_source_not_implemented".to_string(),
        message: "Odometer does not read a live quota or rate-limit API for this provider yet. \
                  Quota values shown elsewhere (Codex rate-limit snapshots) come only from \
                  transcript observations, not this diagnostic."
            .to_string(),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ProviderCapabilitiesWire {
    pub archived_sources: bool,
    pub session_index: bool,
}

impl From<ProviderCapabilities> for ProviderCapabilitiesWire {
    fn from(value: ProviderCapabilities) -> Self {
        Self {
            archived_sources: value.archived_sources,
            session_index: value.session_index,
        }
    }
}

/// Pure input to provider-health resolution. Deliberately independent of
/// `AppState`/`Config`/`RateCard` so tests can construct synthetic providers
/// without touching the filesystem, the durable archive, or the builtin
/// registry.
#[derive(Debug, Clone)]
pub struct ProviderDiagnosticInput {
    pub id: ProviderId,
    pub display_name: String,
    /// False when this id came from configuration but no adapter is
    /// registered for it in this build (a dormant/unknown provider entry).
    pub registered: bool,
    /// False when the saved session-source configuration as a whole is
    /// invalid (e.g. ambiguous overlapping roots) and scanning is disabled
    /// for every provider until it is corrected.
    pub source_configuration_valid: bool,
    pub capabilities: ProviderCapabilitiesWire,
    pub roots: Vec<DiagnosticRoot>,
    pub discovery: DiscoveryHealth,
    pub ledger: LedgerHealth,
    pub pricing: PricingHealth,
    pub archive_roots_configured: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderDiagnostic {
    pub id: ProviderId,
    pub display_name: String,
    pub registered: bool,
    pub state: ProviderHealthState,
    /// Reasons that drove the `state` determination (blocking).
    pub reasons: Vec<DiagnosticReason>,
    /// Additional non-blocking observations.
    pub notices: Vec<DiagnosticReason>,
    pub capabilities: ProviderCapabilitiesWire,
    pub roots: Vec<DiagnosticRoot>,
    pub discovery: DiscoveryHealth,
    pub ledger: LedgerHealth,
    pub pricing: PricingHealth,
    pub retention: RetentionHealth,
    pub quota: QuotaHealth,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticsReport {
    pub generated_at: DateTime<Utc>,
    /// False when the saved session-source configuration is ambiguous or
    /// otherwise invalid, disabling scanning for every provider until
    /// settings are corrected. See `Config::provider_sources`.
    pub source_configuration_valid: bool,
    pub cache_cold_reason: Option<ColdReason>,
    pub last_scan_at: Option<DateTime<Utc>>,
    pub providers: Vec<ProviderDiagnostic>,
}

/// Resolves one provider's health state and reasons from its diagnostic
/// input. Pure and filesystem-free so it is exhaustively unit testable.
fn evaluate_state(
    input: &ProviderDiagnosticInput,
) -> (
    ProviderHealthState,
    Vec<DiagnosticReason>,
    Vec<DiagnosticReason>,
) {
    let notices = Vec::new();

    if !input.registered {
        return (
            ProviderHealthState::Unsupported,
            vec![reason(
                "provider_not_registered",
                "This provider id appears in configuration, but no adapter is registered for it \
                 in this build. Its sources stay dormant.",
            )],
            notices,
        );
    }

    let mut reasons = Vec::new();
    if !input.source_configuration_valid {
        reasons.push(reason(
            "source_configuration_invalid",
            "The saved session-source configuration is invalid (for example, overlapping roots); \
             scanning is disabled for every provider until it is corrected in Settings.",
        ));
    }

    let any_root_exists = input.roots.iter().any(|root| root.exists);
    let has_activity = input.discovery.discovered_files > 0 || input.ledger.durable_sessions > 0;
    if !any_root_exists && !has_activity {
        reasons.push(reason(
            "no_roots_found",
            "No configured or default root for this provider exists on disk, and no sessions \
             have been observed.",
        ));
        return (ProviderHealthState::NotDetected, reasons, notices);
    }

    if input.roots.iter().any(|root| !root.exists) {
        reasons.push(reason(
            "root_missing",
            "One or more configured roots for this provider do not exist on disk.",
        ));
    }
    if input.discovery.parse_failures > 0 {
        reasons.push(reason(
            "parse_failures_detected",
            "The most recent scan could not parse one or more files for this provider.",
        ));
    }
    if !input.ledger.history_store_available {
        reasons.push(reason(
            "ledger_unavailable",
            "The durable session archive is unavailable this run; retention and collision \
             protection are degraded until it is available again.",
        ));
    }
    if !input.capabilities.archived_sources && input.archive_roots_configured > 0 {
        reasons.push(reason(
            "archive_configured_unsupported",
            "Archive roots are configured for this provider, but it does not support archived \
             sources; they are ignored.",
        ));
    }
    if input.pricing.fallback_used {
        reasons.push(reason(
            "pricing_fallback_used",
            "One or more models used by this provider have no published rate and are estimated \
             with the configured fallback rate.",
        ));
    }
    if !input.pricing.unpriced_models_used.is_empty() {
        reasons.push(reason(
            "pricing_unpriced_models",
            "One or more models used by this provider are known to have no published price and \
             are excluded from cost estimates.",
        ));
    }
    if input.pricing.rates_stale {
        reasons.push(reason(
            "rates_stale",
            "The pricing rate card has not been refreshed recently and may not reflect current \
             provider pricing.",
        ));
    }

    if reasons.is_empty() {
        (
            ProviderHealthState::Ready,
            reasons,
            vec![reason("healthy", "No issues detected for this provider.")],
        )
    } else {
        (ProviderHealthState::Degraded, reasons, notices)
    }
}

fn retention_health(input: &ProviderDiagnosticInput) -> RetentionHealth {
    let level = if input.capabilities.archived_sources {
        RetentionRiskLevel::None
    } else if input.ledger.history_store_available {
        RetentionRiskLevel::Moderate
    } else {
        RetentionRiskLevel::High
    };
    RetentionHealth {
        level,
        supports_archive: input.capabilities.archived_sources,
        archive_roots_configured: input.archive_roots_configured,
    }
}

/// Builds one provider's full diagnostic from its pure input. Exposed for
/// tests; `generate_report` is the only production caller.
pub fn build_provider_diagnostic(input: ProviderDiagnosticInput) -> ProviderDiagnostic {
    let (state, reasons, notices) = evaluate_state(&input);
    let retention = retention_health(&input);
    ProviderDiagnostic {
        id: input.id,
        display_name: input.display_name,
        registered: input.registered,
        state,
        reasons,
        notices,
        capabilities: input.capabilities,
        roots: input.roots,
        discovery: input.discovery,
        ledger: input.ledger,
        pricing: input.pricing,
        retention,
        quota: quota_health(),
    }
}

// ---------------------------------------------------------------------------
// Assembly: gathers real inputs from AppState/Config/RateCard and calls the
// pure resolver above. This is the only part of the module that touches the
// filesystem (cheap `Path::exists()` checks on a handful of configured
// roots) or global state, and it never re-parses or re-walks transcripts.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
struct ProviderSessionStats {
    durable_sessions: u64,
    available_sessions: u64,
    collision_sessions: u64,
    models_used: HashSet<String>,
}

/// One pass over the already-loaded in-memory session projection (no new
/// discovery or parsing). `storage_id` embeds a `:collision:` suffix exactly
/// when the durable archive detected competing sources for one provider
/// identity (see `history_store::reconcile_session`).
fn collect_session_stats(state: &AppState) -> HashMap<ProviderId, ProviderSessionStats> {
    let mut stats: HashMap<ProviderId, ProviderSessionStats> = HashMap::new();
    for entry in state.sessions.iter() {
        let session = entry.value();
        let per_provider = stats.entry(session.harness.clone()).or_default();
        per_provider.durable_sessions += 1;
        if session.source_availability == SourceAvailability::Present {
            per_provider.available_sessions += 1;
        }
        if session.storage_id.contains(":collision:") {
            per_provider.collision_sessions += 1;
        }
        // Generous cap well above the final wire sample size (MAX_MODEL_SAMPLE)
        // so a pathological corpus with many distinct model strings cannot
        // grow this set unboundedly while still observing plenty of models.
        if per_provider.models_used.len() < MAX_MODEL_SAMPLE * 4 {
            if let Some(model) = &session.model {
                per_provider.models_used.insert(model.clone());
            }
            for model in session.tokens_by_model.keys() {
                per_provider.models_used.insert(model.clone());
            }
        }
    }
    stats
}

fn pricing_health(models_used: &HashSet<String>, rates: &RateCard) -> PricingHealth {
    let unpriced: HashSet<&str> = rates.unpriced_models.iter().map(String::as_str).collect();
    let mut used: Vec<&String> = models_used.iter().collect();
    used.sort();

    let mut priced = 0usize;
    let mut unpriced_used = Vec::new();
    let mut fallback_used_models = Vec::new();
    for model in used {
        if rates.models.contains_key(model) {
            priced += 1;
        } else if unpriced.contains(model.as_str()) {
            unpriced_used.push(model.clone());
        } else {
            fallback_used_models.push(model.clone());
        }
    }
    unpriced_used.truncate(MAX_MODEL_SAMPLE);
    fallback_used_models.truncate(MAX_MODEL_SAMPLE);
    let fallback_used = !fallback_used_models.is_empty();

    let rates_stale = rates
        .fetched_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|parsed| {
            Utc::now().signed_duration_since(parsed.with_timezone(&Utc))
                > chrono::Duration::days(RATES_STALE_AFTER_DAYS)
        })
        .unwrap_or(false);

    PricingHealth {
        models_observed: models_used.len(),
        models_priced: priced,
        unpriced_models_used: unpriced_used,
        fallback_models_used: fallback_used_models,
        fallback_used,
        rates_fetched_at: rates.fetched_at.clone(),
        rates_stale,
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    crate::paths::normalized_path_key(left, true) == crate::paths::normalized_path_key(right, true)
}

fn make_root(kind: RootKind, path: &Path, defaults: &[PathBuf]) -> DiagnosticRoot {
    DiagnosticRoot {
        kind,
        path: path.to_path_buf(),
        exists: path.exists(),
        is_default: defaults.iter().any(|default| same_path(default, path)),
    }
}

fn default_live_roots(id: &ProviderId) -> Vec<PathBuf> {
    if id == &crate::provider::codex_provider_id() {
        vec![crate::config::codex_home_dir().join("sessions")]
    } else if id == &crate::provider::claude_code_provider_id() {
        vec![crate::config::claude_config_dir().join("projects")]
    } else {
        Vec::new()
    }
}

fn default_archive_roots(id: &ProviderId) -> Vec<PathBuf> {
    if id == &crate::provider::codex_provider_id() {
        vec![crate::config::codex_home_dir().join("archived_sessions")]
    } else {
        Vec::new()
    }
}

fn default_session_index_roots(id: &ProviderId) -> Vec<PathBuf> {
    if id == &crate::provider::codex_provider_id() {
        vec![crate::config::codex_home_dir().join("session_index.jsonl")]
    } else {
        Vec::new()
    }
}

fn provider_roots(
    id: &ProviderId,
    provider_config: &crate::config::ProviderSourceConfig,
    capabilities: ProviderCapabilitiesWire,
) -> Vec<DiagnosticRoot> {
    let default_live = default_live_roots(id);
    let default_archive = default_archive_roots(id);
    let default_index = default_session_index_roots(id);

    let mut roots = Vec::new();
    for root in &provider_config.live_roots {
        roots.push(make_root(RootKind::Live, root, &default_live));
    }
    for root in &provider_config.archive_roots {
        roots.push(make_root(RootKind::Archive, root, &default_archive));
    }
    if capabilities.session_index {
        if let Some(index_path) = &provider_config.session_index_path {
            roots.push(make_root(
                RootKind::SessionIndex,
                index_path,
                &default_index,
            ));
        }
    }
    roots
}

/// Assembles the full provider diagnostics report from live application
/// state. Synchronous and bounded: a handful of `Path::exists()` checks plus
/// one pass over the already-loaded session map, so it is safe to call
/// on-demand from a Tauri command without delaying startup or needing
/// cancellation.
pub fn generate_report(state: &AppState, config: &Config, rates: &RateCard) -> DiagnosticsReport {
    let registry = ProviderRegistry::builtin();
    let normalized = config.clone().normalized();
    let source_configuration_valid = config.provider_sources().is_ok();
    let scan_summary = state.last_scan_summary();
    let cache_cold_reason = *state.cold_reason.lock().unwrap();
    let session_stats = collect_session_stats(state);

    let mut seen_ids: HashSet<ProviderId> = HashSet::new();
    let mut providers = Vec::new();

    for descriptor in registry.descriptors() {
        seen_ids.insert(descriptor.id.clone());
        let capabilities = ProviderCapabilitiesWire::from(descriptor.capabilities);
        let provider_config = normalized
            .providers
            .get(&descriptor.id)
            .cloned()
            .unwrap_or_default();
        let roots = provider_roots(&descriptor.id, &provider_config, capabilities);

        let counts = scan_summary
            .as_ref()
            .and_then(|summary| summary.report.per_provider.get(&descriptor.id).copied())
            .unwrap_or_default();
        let discovery = DiscoveryHealth {
            discovered_files: counts.discovered,
            parsed_files: counts.parsed,
            skipped_files: counts.skipped,
            parse_failures: counts.parse_failures,
            cache_hits: counts.cache_hits,
            cache_misses: counts.cache_misses,
        };

        let stats = session_stats
            .get(&descriptor.id)
            .cloned()
            .unwrap_or_default();
        let ledger = LedgerHealth {
            history_store_available: state.history.is_some(),
            durable_sessions: stats.durable_sessions,
            available_sessions: stats.available_sessions,
            collision_sessions: stats.collision_sessions,
        };
        let pricing = pricing_health(&stats.models_used, rates);

        providers.push(build_provider_diagnostic(ProviderDiagnosticInput {
            id: descriptor.id.clone(),
            display_name: descriptor.display_name.to_string(),
            registered: true,
            source_configuration_valid,
            capabilities,
            roots,
            discovery,
            ledger,
            pricing,
            archive_roots_configured: provider_config.archive_roots.len(),
        }));
    }

    // Providers named in configuration without a registered adapter: dormant
    // (config.rs already logs and skips them when building the scan's source
    // set), but still worth surfacing so a stale or mistyped provider id is
    // diagnosable instead of silently ignored.
    for (id, provider_config) in &normalized.providers {
        if seen_ids.contains(id) {
            continue;
        }
        let capabilities = ProviderCapabilitiesWire::default();
        let roots = provider_roots(id, provider_config, capabilities);
        providers.push(build_provider_diagnostic(ProviderDiagnosticInput {
            id: id.clone(),
            display_name: id.as_str().to_string(),
            registered: false,
            source_configuration_valid,
            capabilities,
            roots,
            discovery: DiscoveryHealth::default(),
            ledger: LedgerHealth {
                history_store_available: state.history.is_some(),
                ..LedgerHealth::default()
            },
            pricing: pricing_health(&HashSet::new(), rates),
            archive_roots_configured: provider_config.archive_roots.len(),
        }));
    }

    DiagnosticsReport {
        generated_at: Utc::now(),
        source_configuration_valid,
        cache_cold_reason,
        last_scan_at: scan_summary.map(|summary| summary.completed_at),
        providers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(models: &[&str], unpriced: &[&str], fetched_at: Option<&str>) -> RateCard {
        RateCard {
            version: 1,
            currency: "USD".into(),
            unit: "per_1m_tokens".into(),
            source_url: "https://example.test/rates".into(),
            fetched_at: fetched_at.map(str::to_string),
            models: models
                .iter()
                .map(|model| {
                    (
                        model.to_string(),
                        crate::rates::ModelRate {
                            input: 1.0,
                            cached_input: 0.1,
                            output: 2.0,
                            reasoning: 2.0,
                        },
                    )
                })
                .collect(),
            fallback_model: "fallback".into(),
            currencies: Default::default(),
            fallback_models: Default::default(),
            api_models: Default::default(),
            unpriced_models: unpriced.iter().map(|m| m.to_string()).collect(),
            pricing_catalog: Default::default(),
        }
    }

    fn used(models: &[&str]) -> HashSet<String> {
        models.iter().map(|m| m.to_string()).collect()
    }

    #[test]
    fn pricing_health_distinguishes_priced_unpriced_and_fallback_models() {
        let rates = card(&["gpt-priced"], &["gpt-known-unpriced"], None);
        let health = pricing_health(
            &used(&["gpt-priced", "gpt-known-unpriced", "gpt-unknown"]),
            &rates,
        );
        assert_eq!(health.models_observed, 3);
        assert_eq!(health.models_priced, 1);
        assert_eq!(health.unpriced_models_used, vec!["gpt-known-unpriced"]);
        assert_eq!(health.fallback_models_used, vec!["gpt-unknown"]);
        assert!(health.fallback_used);
        assert!(!health.rates_stale, "no fetch history is not flagged stale");
    }

    #[test]
    fn pricing_health_flags_rate_cards_not_refreshed_recently() {
        let stale = card(&["gpt-priced"], &[], Some("2020-01-01T00:00:00Z"));
        let health = pricing_health(&used(&["gpt-priced"]), &stale);
        assert!(!health.fallback_used);
        assert!(health.rates_stale);

        let fresh_at = Utc::now().to_rfc3339();
        let fresh = card(&["gpt-priced"], &[], Some(&fresh_at));
        assert!(!pricing_health(&used(&["gpt-priced"]), &fresh).rates_stale);
    }

    #[test]
    fn pricing_health_sample_is_bounded() {
        let many_unpriced: Vec<String> = (0..100).map(|i| format!("unpriced-{i}")).collect();
        let refs: Vec<&str> = many_unpriced.iter().map(String::as_str).collect();
        let rates = card(&[], &refs, None);
        let used_models: HashSet<String> = many_unpriced.iter().cloned().collect();
        let health = pricing_health(&used_models, &rates);
        assert_eq!(health.models_observed, 100);
        assert_eq!(health.unpriced_models_used.len(), MAX_MODEL_SAMPLE);
    }

    #[test]
    fn retention_risk_escalates_without_archive_capability_or_ledger() {
        let base = ProviderDiagnosticInput {
            id: crate::provider::claude_code_provider_id(),
            display_name: "Claude Code".into(),
            registered: true,
            source_configuration_valid: true,
            capabilities: ProviderCapabilitiesWire {
                archived_sources: true,
                session_index: false,
            },
            roots: Vec::new(),
            discovery: DiscoveryHealth::default(),
            ledger: LedgerHealth {
                history_store_available: true,
                ..LedgerHealth::default()
            },
            pricing: PricingHealth::default(),
            archive_roots_configured: 0,
        };
        assert_eq!(retention_health(&base).level, RetentionRiskLevel::None);

        let no_archive_capability = ProviderDiagnosticInput {
            capabilities: ProviderCapabilitiesWire {
                archived_sources: false,
                session_index: false,
            },
            ..base.clone()
        };
        assert_eq!(
            retention_health(&no_archive_capability).level,
            RetentionRiskLevel::Moderate
        );

        let no_ledger_either = ProviderDiagnosticInput {
            ledger: LedgerHealth {
                history_store_available: false,
                ..LedgerHealth::default()
            },
            ..no_archive_capability
        };
        assert_eq!(
            retention_health(&no_ledger_either).level,
            RetentionRiskLevel::High
        );
    }

    #[test]
    fn quota_is_always_reported_not_available_with_a_fixed_reason() {
        let quota = quota_health();
        assert_eq!(quota.status, QuotaStatus::NotAvailable);
        assert_eq!(quota.reason_code, "quota_source_not_implemented");
        assert!(!quota.message.is_empty());
    }
}
