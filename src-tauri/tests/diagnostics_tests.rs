//! Synthetic-provider fixtures for the provider-registry diagnostics report
//! (issue #39): missing roots, unreadable files, mixed valid/invalid
//! records, estimated tokens, stale rates, disabled capabilities, and
//! healthy operation. Every fixture goes through the pure
//! `build_provider_diagnostic`/`ProviderDiagnosticInput` API directly —
//! no real transcripts, paths, or filesystem access, matching the module's
//! separation between pure health resolution and the impure `generate_report`
//! assembly that reads real application state.

use odometer_lib::diagnostics::{
    build_provider_diagnostic, DiagnosticRoot, DiscoveryHealth, LedgerHealth, PricingHealth,
    ProviderCapabilitiesWire, ProviderDiagnosticInput, ProviderHealthState, RetentionRiskLevel,
    RootKind,
};
use odometer_lib::provider::{claude_code_provider_id, codex_provider_id};

fn healthy_input() -> ProviderDiagnosticInput {
    ProviderDiagnosticInput {
        id: codex_provider_id(),
        display_name: "Codex".to_string(),
        registered: true,
        source_configuration_valid: true,
        capabilities: ProviderCapabilitiesWire {
            archived_sources: true,
            session_index: true,
        },
        roots: vec![
            DiagnosticRoot {
                kind: RootKind::Live,
                path: "/synthetic/codex/sessions".into(),
                exists: true,
                is_default: true,
            },
            DiagnosticRoot {
                kind: RootKind::Archive,
                path: "/synthetic/codex/archived_sessions".into(),
                exists: true,
                is_default: true,
            },
        ],
        discovery: DiscoveryHealth {
            discovered_files: 12,
            parsed_files: 12,
            skipped_files: 0,
            parse_failures: 0,
            cache_hits: 10,
            cache_misses: 2,
        },
        ledger: LedgerHealth {
            history_store_available: true,
            durable_sessions: 12,
            available_sessions: 12,
            collision_sessions: 0,
        },
        pricing: PricingHealth {
            models_observed: 2,
            models_priced: 2,
            unpriced_models_used: Vec::new(),
            fallback_models_used: Vec::new(),
            fallback_used: false,
            rates_fetched_at: Some("2026-08-01T00:00:00Z".to_string()),
            rates_stale: false,
        },
        archive_roots_configured: 1,
    }
}

#[test]
fn healthy_operation_resolves_ready_with_a_healthy_notice_and_no_reasons() {
    let diagnostic = build_provider_diagnostic(healthy_input());
    assert_eq!(diagnostic.state, ProviderHealthState::Ready);
    assert!(diagnostic.reasons.is_empty());
    assert_eq!(diagnostic.notices.len(), 1);
    assert_eq!(diagnostic.notices[0].code, "healthy");
    assert_eq!(diagnostic.retention.level, RetentionRiskLevel::None);
    assert_eq!(diagnostic.quota.reason_code, "quota_source_not_implemented");
}

#[test]
fn missing_roots_with_no_activity_resolve_not_detected() {
    let mut input = healthy_input();
    for root in &mut input.roots {
        root.exists = false;
    }
    input.discovery = DiscoveryHealth::default();
    input.ledger = LedgerHealth::default();

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::NotDetected);
    assert!(diagnostic
        .reasons
        .iter()
        .any(|reason| reason.code == "no_roots_found"));
}

#[test]
fn one_missing_root_with_otherwise_healthy_activity_is_degraded_not_not_detected() {
    let mut input = healthy_input();
    input.roots[1].exists = false; // archive root missing; live root still present

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Degraded);
    assert!(diagnostic
        .reasons
        .iter()
        .any(|reason| reason.code == "root_missing"));
}

#[test]
fn unreadable_files_surface_as_parse_failures_and_degrade_the_provider() {
    let mut input = healthy_input();
    input.discovery.parse_failures = 3;

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Degraded);
    assert!(diagnostic
        .reasons
        .iter()
        .any(|reason| reason.code == "parse_failures_detected"));
}

/// A file with some malformed/unknown records alongside valid ones must not
/// itself fail the whole file (see AGENTS.md's parser invariants) — the scan
/// counts it as parsed, and the provider stays healthy rather than crying
/// wolf over data the parser already tolerated.
#[test]
fn mixed_valid_and_invalid_records_within_a_file_do_not_falsely_degrade_the_provider() {
    let mut input = healthy_input();
    input.discovery.discovered_files = 1;
    input.discovery.parsed_files = 1;
    input.discovery.skipped_files = 0;
    input.discovery.parse_failures = 0;

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Ready);
    assert!(diagnostic.reasons.is_empty());
}

/// A model with no published rate is priced via the fallback rate rather
/// than a precise one — the "estimated tokens" scenario from the issue.
#[test]
fn models_priced_only_via_fallback_rate_are_reported_as_estimated() {
    let mut input = healthy_input();
    input.pricing = PricingHealth {
        models_observed: 2,
        models_priced: 1,
        unpriced_models_used: Vec::new(),
        fallback_models_used: vec!["gpt-unreleased-preview".to_string()],
        fallback_used: true,
        rates_fetched_at: Some("2026-08-01T00:00:00Z".to_string()),
        rates_stale: false,
    };

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Degraded);
    assert!(diagnostic
        .reasons
        .iter()
        .any(|reason| reason.code == "pricing_fallback_used"));
    assert_eq!(
        diagnostic.pricing.fallback_models_used,
        vec!["gpt-unreleased-preview".to_string()]
    );
}

#[test]
fn a_rate_card_that_has_not_been_refreshed_recently_is_flagged_stale() {
    let mut input = healthy_input();
    input.pricing.rates_stale = true;

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Degraded);
    assert!(diagnostic
        .reasons
        .iter()
        .any(|reason| reason.code == "rates_stale"));
}

/// Archive roots configured for a provider whose capability flag reports no
/// archive support (Claude Code today, or any provider with the capability
/// disabled) are ignored by the scanner but must stay diagnosable rather
/// than silently dropped.
#[test]
fn archive_roots_configured_for_a_provider_without_archive_support_are_flagged() {
    let input = ProviderDiagnosticInput {
        id: claude_code_provider_id(),
        display_name: "Claude Code".to_string(),
        capabilities: ProviderCapabilitiesWire {
            archived_sources: false,
            session_index: false,
        },
        archive_roots_configured: 1,
        roots: vec![DiagnosticRoot {
            kind: RootKind::Live,
            path: "/synthetic/claude/projects".into(),
            exists: true,
            is_default: true,
        }],
        ..healthy_input()
    };

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Degraded);
    assert!(diagnostic
        .reasons
        .iter()
        .any(|reason| reason.code == "archive_configured_unsupported"));
    // No archive capability at all raises retention risk even when the
    // durable ledger is available to back it up.
    assert_eq!(diagnostic.retention.level, RetentionRiskLevel::Moderate);
}

#[test]
fn ledger_unavailable_raises_retention_risk_to_high_without_archive_support() {
    let input = ProviderDiagnosticInput {
        capabilities: ProviderCapabilitiesWire {
            archived_sources: false,
            session_index: false,
        },
        ledger: LedgerHealth {
            history_store_available: false,
            ..LedgerHealth::default()
        },
        ..healthy_input()
    };

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Degraded);
    assert!(diagnostic
        .reasons
        .iter()
        .any(|reason| reason.code == "ledger_unavailable"));
    assert_eq!(diagnostic.retention.level, RetentionRiskLevel::High);
}

#[test]
fn an_unregistered_provider_id_resolves_unsupported_regardless_of_other_input() {
    let input = ProviderDiagnosticInput {
        registered: false,
        ..healthy_input()
    };

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Unsupported);
    assert_eq!(diagnostic.reasons.len(), 1);
    assert_eq!(diagnostic.reasons[0].code, "provider_not_registered");
}

#[test]
fn invalid_source_configuration_degrades_every_registered_provider() {
    let input = ProviderDiagnosticInput {
        source_configuration_valid: false,
        ..healthy_input()
    };

    let diagnostic = build_provider_diagnostic(input);
    assert_eq!(diagnostic.state, ProviderHealthState::Degraded);
    assert!(diagnostic
        .reasons
        .iter()
        .any(|reason| reason.code == "source_configuration_invalid"));
}

#[test]
fn reason_and_notice_text_never_embeds_a_raw_path() {
    // Defense in depth for the export redaction boundary: every generic
    // reason/notice message must stand alone without interpolating a path,
    // so export redaction (frontend) never has to scrub free-form text.
    let mut input = healthy_input();
    input.roots[1].exists = false;
    input.discovery.parse_failures = 1;
    input.ledger.history_store_available = false;
    input.pricing.fallback_used = true;
    input.pricing.fallback_models_used = vec!["gpt-unreleased-preview".to_string()];
    input.pricing.rates_stale = true;

    let diagnostic = build_provider_diagnostic(input);
    assert!(!diagnostic.reasons.is_empty());
    for reason in &diagnostic.reasons {
        assert!(!reason.message.contains('/'), "{}", reason.message);
        assert!(!reason.message.contains('\\'), "{}", reason.message);
    }
}
