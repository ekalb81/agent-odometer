//! Shared, read-only integration front door. No transcript scan or network use.
use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::{
    config::Config, history_store::HistoryStore, query_control::QueryControl, rates::RateCard,
};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    IntegrationNotConfigured,
    ServerLaunchFailed,
    ProtocolVersionMismatch,
    ToolCatalogMismatch,
    LedgerNotReady,
    ScanInProgress,
    SessionNotFound,
    PricingIncomplete,
    QueryTooBroad,
    SnapshotExpired,
    QueryFailed,
    QueryCancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub evidence: &'static str,
    pub next_action: &'static str,
}

impl DiagnosticCode {
    pub fn diagnostic(self) -> Diagnostic {
        let (evidence, next_action) = match self {
            Self::IntegrationNotConfigured => ("No matching Odometer MCP entry was verified in the selected scope.", "Preview an entry or copy the manual setup command."),
            Self::ServerLaunchFailed => ("The configured server could not complete a bounded launch.", "Check the executable and working directory, then test again."),
            Self::ProtocolVersionMismatch => ("The server handshake does not match the supported protocol.", "Update the client/server and start a fresh task."),
            Self::ToolCatalogMismatch => ("The advertised tool names or input schemas differ from this version.", "Restart the client after updating Odometer, then test again."),
            Self::LedgerNotReady => ("A readable durable ledger is unavailable.", "Open Odometer and wait for history preparation; do not treat missing usage as zero."),
            Self::ScanInProgress => ("The desktop has not completed its current source scan.", "Wait for the scan to finish before drawing completeness conclusions."),
            Self::SessionNotFound => ("The requested session is not in the current query snapshot.", "Use session_report to discover a current session key first."),
            Self::PricingIncomplete => ("Some observed models lack direct current pricing evidence.", "Inspect diagnostics_report and pricing provenance before comparing costs."),
            Self::QueryTooBroad => ("The query exceeded a bounded work or output limit.", "Narrow the date window or session limit and retry."),
            Self::SnapshotExpired => ("The previously observed query snapshot is no longer current.", "Call odometer_status and repeat discovery before retrying."),
            Self::QueryFailed => ("The read-only query could not produce a usable result.", "Inspect odometer_status and diagnostics_report, then retry a narrow query."),
            Self::QueryCancelled => ("The query was cancelled before completion.", "Retry only if the result is still needed."),
        };
        Diagnostic {
            code: self,
            evidence,
            next_action,
        }
    }
    pub fn for_query_failure(message: &str) -> Self {
        if message.contains("cancelled") {
            Self::QueryCancelled
        } else if message.contains("limit")
            || message.contains("deadline")
            || message.contains("timed out")
        {
            Self::QueryTooBroad
        } else if message.contains("history is unavailable") {
            Self::LedgerNotReady
        } else {
            Self::QueryFailed
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Observation {
    pub captured_at: Option<DateTime<Utc>>,
    pub age_seconds: Option<i64>,
    /// A diagnostic observation marker, not an atomic ledger revision/cursor.
    pub generation: Option<String>,
    pub token_event_from: Option<DateTime<Utc>>,
    pub token_event_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Serialize)]
pub struct IntegrationStatus {
    pub schema_version: u32,
    pub server_version: &'static str,
    pub protocol_version: &'static str,
    pub generated_at: DateTime<Utc>,
    pub ledger_available: bool,
    pub scan_status: &'static str,
    pub sessions: Option<u64>,
    pub observation: Observation,
    pub providers: Vec<crate::query::HeadlessProviderDiagnostic>,
    pub pricing_models: Vec<ModelCoverage>,
    pub pricing_authority: &'static str,
    pub quota_authority: &'static str,
    pub dimensions: &'static [&'static str],
    pub filters: &'static [&'static str],
    pub suggested_next_calls: Vec<&'static str>,
    pub diagnostics: Vec<Diagnostic>,
    pub limitations: &'static [&'static str],
}

#[derive(Debug, Serialize)]
pub struct ModelCoverage {
    pub provider: String,
    pub model: String,
    pub plan_basis: crate::rates::PricingBasis,
    pub api_estimate_basis: crate::rates::PricingBasis,
    pub resolved_plan_model: String,
    pub resolved_api_model: String,
}

pub fn status(
    store: Option<&HistoryStore>,
    rates: &RateCard,
    config: &Config,
    now: DateTime<Utc>,
    control: Option<&QueryControl>,
) -> Result<IntegrationStatus> {
    let report =
        crate::query::diagnostics_report_controlled(store, config, rates, now, false, control)?;
    let observed = store
        .map(HistoryStore::integration_observation)
        .transpose()?;
    let captured = observed
        .and_then(|value| value.captured_at_ms)
        .and_then(DateTime::from_timestamp_millis);
    let mut diagnostics = Vec::new();
    if store.is_none() {
        diagnostics.push(DiagnosticCode::LedgerNotReady.diagnostic());
    }
    if report
        .providers
        .iter()
        .flat_map(|provider| &provider.models)
        .any(|model| {
            matches!(
                model.basis,
                crate::rates::PricingBasis::Unavailable
                    | crate::rates::PricingBasis::Fallback
                    | crate::rates::PricingBasis::Stale
            )
        })
    {
        diagnostics.push(DiagnosticCode::PricingIncomplete.diagnostic());
    }
    let pricing_models = report
        .providers
        .iter()
        .flat_map(|provider| {
            provider.models.iter().map(|model| {
                let tokens = crate::model::TokenTotals::default();
                let plan = crate::query::price_tokens(
                    rates,
                    &provider.provider,
                    &model.model,
                    None,
                    &tokens,
                    crate::query::RateTable::Plan,
                    now,
                );
                let api = crate::query::price_tokens(
                    rates,
                    &provider.provider,
                    &model.model,
                    None,
                    &tokens,
                    crate::query::RateTable::ApiEstimate,
                    now,
                );
                ModelCoverage {
                    provider: provider.provider.clone(),
                    model: model.model.clone(),
                    plan_basis: plan.basis,
                    api_estimate_basis: api.basis,
                    resolved_plan_model: plan.resolved_model,
                    resolved_api_model: api.resolved_model,
                }
            })
        })
        .collect();
    Ok(IntegrationStatus {
        schema_version: SCHEMA_VERSION, server_version: env!("CARGO_PKG_VERSION"),
        protocol_version: crate::mcp_server::PROTOCOL_VERSION, generated_at: now,
        ledger_available: store.is_some(), scan_status: "unknown_in_headless_query",
        sessions: store.map(HistoryStore::session_count).transpose()?,
        observation: Observation {
            captured_at: captured, age_seconds: captured.filter(|value| *value <= now).map(|value| (now - value).num_seconds()),
            generation: observed.and_then(|value| value.captured_at_ms.map(|at| format!("observation:{at}:{}", value.snapshot_count))),
            token_event_from: observed.and_then(|value| value.token_from_ms).and_then(DateTime::from_timestamp_millis),
            token_event_to: observed.and_then(|value| value.token_to_ms).and_then(DateTime::from_timestamp_millis),
        }, providers: report.providers, pricing_models,
        pricing_authority: "Observed tokens and plan credits are distinct from API USD estimates and as-of billing scenarios. Inspect each report's cost surfaces and provenance.",
        quota_authority: "Not queried here. quota_report returns source provenance and staleness; transcript evidence is not an authoritative live quota.",
        dimensions: &["provider", "model", "project", "session", "category", "tool", "context", "finding", "utc_hour"],
        filters: &["from: inclusive UTC YYYY-MM-DD", "to: inclusive UTC YYYY-MM-DD", "session_report limit: 1..1000"],
        suggested_next_calls: if store.is_some() { vec!["usage_report", "session_report", "workflow_metrics", "quota_report"] } else { vec!["diagnostics_report"] },
        diagnostics,
        limitations: &["A readable ledger does not prove a complete current scan.", "Observation generation identifies freshness metadata, not a transactional snapshot or resume cursor.", "Token-event date coverage excludes sessions without recorded token events.", "Cohort comparisons are observational; they do not demonstrate causation.", "No transcript bodies, prompts, tool arguments, credentials, or network polling are exposed."],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_ledger_is_not_a_priced_empty_snapshot() {
        let report = status(
            None,
            &RateCard::default(),
            &Config::default(),
            Utc::now(),
            None,
        )
        .unwrap();
        assert!(!report.ledger_available);
        assert_eq!(report.sessions, None);
        assert_eq!(report.observation.age_seconds, None);
        assert_eq!(report.observation.generation, None);
        assert_eq!(report.scan_status, "unknown_in_headless_query");
        assert_eq!(report.diagnostics[0].code, DiagnosticCode::LedgerNotReady);
        assert_eq!(report.suggested_next_calls, ["diagnostics_report"]);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("source_directory"));
    }
}
