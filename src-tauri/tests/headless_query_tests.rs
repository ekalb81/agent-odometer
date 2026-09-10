//! Synthetic ledger-to-headless tests; no private corpus or copied calculator.
use std::path::{Path, PathBuf};

use chrono::{Duration, FixedOffset, TimeZone, Utc};
use odometer_lib::config::Config;
use odometer_lib::history_store::HistoryStore;
use odometer_lib::model::{
    CategoryMetric, OptimizationFinding, Session, TaskCategory, TierBucket, TokenHistoryPoint,
    TokenTotals, ToolKind, ToolObservation, ToolOrigin, ToolOutcome,
};
use odometer_lib::provider::{
    claude_code_provider_id, codex_provider_id, gemini_cli_provider_id, ProviderId,
};
use odometer_lib::query::{
    category_report, context_report, diagnostics_report, findings_report, project_report,
    provider_for_key, range_report, session_report, statusline_report, tools_report,
};
use odometer_lib::rates::{PricingBasis, RateCard};
use serde::Deserialize;

fn instant() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap()
}

fn session(id: &str) -> Session {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-session.jsonl");
    let mut value = odometer_lib::parser::parse_file(&fixture, false)
        .unwrap()
        .unwrap();
    value.id = id.into();
    value.storage_id.clear();
    value.started_at = instant();
    value.last_event_at = instant();
    value.tokens_history.clear();
    value.tokens_by_model.clear();
    value.tokens_total = TokenTotals::default();
    value.turns.clear();
    value.tool_observations.clear();
    value.category_totals.clear();
    value.optimization_findings.clear();
    value.rate_limits_history.clear();
    value
}

fn ledger(root: &Path, sessions: &[Session]) -> HistoryStore {
    let store = HistoryStore::open(&root.join("history.sqlite3")).unwrap();
    let generation = store.begin_scan().unwrap().max(1);
    for (index, session) in sessions.iter().enumerate() {
        store
            .observe(
                Path::new(&format!("synthetic-{index}.jsonl")),
                session,
                generation,
            )
            .unwrap();
    }
    store
}

fn add_bucket(session: &mut Session, bucket: &TierBucket, offset: i64) {
    session.tokens_total += &bucket.tokens;
    *session
        .tokens_by_model
        .entry(bucket.model.clone())
        .or_default() += &bucket.tokens;
    let timestamp = instant() + Duration::minutes(offset);
    session.last_event_at = timestamp;
    session.tokens_history.push(TokenHistoryPoint {
        timestamp,
        model: Some(bucket.model.clone()),
        service_tier: bucket.service_tier.clone(),
        request_input_tokens: None,
        total_tokens: session.tokens_total.total_tokens,
        delta: bucket.tokens.clone(),
    });
}

#[derive(Deserialize)]
struct Oracle {
    rate_card: RateCard,
    cases: Vec<OracleCase>,
}
#[derive(Deserialize)]
struct OracleCase {
    name: String,
    harness: String,
    table: String,
    buckets: Vec<TierBucket>,
    expected: serde_json::Value,
}
fn oracle() -> Oracle {
    serde_json::from_str(include_str!("../../tests/conformance/pricing-cases.json")).unwrap()
}

#[test]
fn model_report_preserves_frozen_bucket_prices_including_fast_tiers() {
    let fixture = oracle();
    for case in fixture
        .cases
        .into_iter()
        .filter(|case| case.table == "plan")
    {
        let root = tempfile::tempdir().unwrap();
        let mut input = session("oracle");
        input.harness = ProviderId::new(&case.harness).unwrap();
        for (index, bucket) in case.buckets.iter().enumerate() {
            add_bucket(&mut input, bucket, index as i64);
        }
        let store = ledger(root.path(), &[input]);
        let report = range_report(
            &store,
            &fixture.rate_card,
            |_| case.harness.clone(),
            None,
            None,
            instant(),
        )
        .unwrap();
        let expected_total = case.expected["total"].as_f64().unwrap();
        let actual_total: f64 = report.cost_by_currency.values().sum();
        assert!(
            (actual_total - expected_total).abs() < 1e-8,
            "{}: {actual_total} != {expected_total}",
            case.name
        );
        for expected in case.expected["by_model"].as_array().unwrap() {
            // The ledger intentionally omits sessions with no activity;
            // their priced total still reconciles to the frozen zero.
            if case
                .buckets
                .iter()
                .all(|bucket| bucket.tokens == TokenTotals::default())
            {
                assert!(report.by_model.is_empty());
                continue;
            }
            let actual = report
                .by_model
                .iter()
                .find(|row| row.model == expected["model"].as_str().unwrap())
                .unwrap_or_else(|| panic!("missing model in {}: {:?}", case.name, report.by_model));
            let expected_cost = expected["cost"].as_f64().unwrap();
            if expected["unpriced"] == true {
                assert!(actual.cost.is_none());
            } else {
                assert!(
                    (actual.cost.unwrap() - expected_cost).abs() < 1e-8,
                    "{}",
                    case.name
                );
            }
            assert_eq!(
                serde_json::to_value(actual.basis).unwrap(),
                expected["basis"],
                "{}",
                case.name
            );
        }
    }
}

#[test]
fn model_provenance_keeps_cache_write_estimate_across_later_clean_buckets() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("estimated");
    for (index, creation) in [200, 0].into_iter().enumerate() {
        add_bucket(
            &mut input,
            &TierBucket {
                model: "no-premium-model".into(),
                service_tier: None,
                tokens: TokenTotals {
                    input_tokens: 1000,
                    cache_creation_input_tokens: creation,
                    total_tokens: 1000,
                    ..Default::default()
                },
            },
            index as i64,
        );
    }
    let store = ledger(root.path(), &[input]);
    let report = range_report(
        &store,
        &oracle().rate_card,
        |_| "codex".into(),
        None,
        None,
        instant(),
    )
    .unwrap();
    assert_eq!(report.by_model[0].basis, PricingBasis::Estimated);
}

fn tool(at: i64, id: &str) -> ToolObservation {
    ToolObservation {
        call_id: id.into(),
        turn_id: Some("turn".into()),
        harness: codex_provider_id(),
        model: Some("base-model".into()),
        timestamp: instant() + Duration::minutes(at),
        kind: ToolKind::Read,
        name: "read".into(),
        providers: vec!["synthetic_mcp".into()],
        effective_tools: Vec::new(),
        target: None,
        resource_id: None,
        origin: ToolOrigin::Mcp,
        shell_family: None,
        language: Some("rust".into()),
        outcome: ToolOutcome::Success,
        duration_ms: Some(7),
        output_bytes: 13,
    }
}

#[test]
fn tool_only_and_finding_windows_reconcile_complete_hours_and_partial_edges() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("tools-only");
    for (index, minute) in [5, 30, 75, 140, 190, 220].into_iter().enumerate() {
        input
            .tool_observations
            .push(tool(minute, &format!("call-{index}")));
        input.optimization_findings.push(OptimizationFinding {
            timestamp: Some(instant() + Duration::minutes(minute)),
            rule_id: "repeat-read".into(),
            severity: "warning".into(),
            avoidable_calls: 2,
            ..Default::default()
        });
    }
    input.optimization_findings.push(OptimizationFinding {
        rule_id: "undated".into(),
        ..Default::default()
    });
    input.last_event_at = instant() + Duration::hours(4);
    let from = Some(instant() + Duration::minutes(15));
    let to = Some(instant() + Duration::minutes(210));
    let expected = input.range_totals(from, to);
    let store = ledger(root.path(), &[input]);
    let tools = tools_report(&store, from, to).unwrap();
    assert_eq!(tools.providers.len(), 1);
    assert_eq!(tools.providers[0].sessions, 1);
    assert_eq!(tools.providers[0].metrics, expected.tool_metrics);
    assert_eq!(tools.providers[0].metrics.calls, 4);
    assert_eq!(
        tools.providers[0].dimensions["language"].values,
        expected.tool_dimensions["language"]
    );
    let findings = findings_report(&store, from, to).unwrap();
    assert_eq!(findings.summary, expected.optimization_summary);
    assert_eq!(findings.summary.findings, 4);
    assert_eq!(findings.summary.likely_avoidable_calls, 8);
    assert!(!findings.summary.by_rule.contains_key("undated"));
    assert_eq!(
        findings_report(&store, None, None)
            .unwrap()
            .summary
            .findings,
        7
    );
}

#[test]
fn dimensions_preserve_unavailable_distinct_from_observed_zero() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("gemini");
    input.harness = gemini_cli_provider_id();
    input.tool_observations.push(tool(0, "one"));
    let store = ledger(root.path(), &[input]);
    let tools = tools_report(&store, None, None).unwrap();
    let row = &tools.providers[0];
    assert!(!row.dimensions["mcp_server"].available);
    assert!(!row.dimensions["shell_family"].available);
    assert!(row.dimensions["language"].available);
    let contexts = context_report(&store, None, None).unwrap();
    assert!(contexts.providers[0].context.available);
    assert!(contexts.providers[0]
        .context
        .values
        .values()
        .all(|row| row.tokens == 0));
}

#[test]
fn categories_preserve_complete_overlapping_session_scope_and_shared_prices() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("categories");
    let bucket = TierBucket {
        model: "gpt-5.5".into(),
        service_tier: Some("fast".into()),
        tokens: TokenTotals {
            input_tokens: 1_000_000,
            total_tokens: 1_000_000,
            ..Default::default()
        },
    };
    add_bucket(&mut input, &bucket, 180);
    input.category_totals.insert(
        TaskCategory::Coding,
        CategoryMetric {
            turns: 2,
            tokens: bucket.tokens.clone(),
            tool_calls: 4,
            buckets: vec![bucket],
        },
    );
    let store = ledger(root.path(), &[input]);
    let rates = oracle().rate_card;
    let report = category_report(
        &store,
        &rates,
        Some(instant() + Duration::minutes(30)),
        Some(instant() + Duration::minutes(40)),
        instant(),
    )
    .unwrap();
    assert_eq!(report.scope, "whole_sessions_overlapping_window");
    assert_eq!(report.sessions, 1);
    assert_eq!(report.categories[0].turns, 2);
    assert_eq!(report.categories[0].tokens.input_tokens, 1_000_000);
    assert_eq!(
        report.categories[0].pricing.as_ref().unwrap().plan.total,
        25.0
    );
    assert!(category_report(
        &store,
        &rates,
        Some(instant() + Duration::days(1)),
        None,
        instant()
    )
    .unwrap()
    .categories
    .is_empty());
}

#[test]
fn diagnostics_redacts_paths_and_marks_unobserved_runtime_health_unavailable() {
    let root = tempfile::tempdir().unwrap();
    let store = ledger(root.path(), &[session("diagnostics")]);
    let config = Config {
        session_roots: vec![PathBuf::from("/synthetic/private-root")],
        ..Default::default()
    };
    let report =
        diagnostics_report(Some(&store), &config, &oracle().rate_card, instant(), false).unwrap();
    let wire = serde_json::to_string(&report).unwrap();
    assert!(!wire.contains("private-root"));
    assert_eq!(report.scan_status, "unavailable_in_headless_query");
    assert_eq!(report.cache_status, "unavailable_in_headless_query");
    assert!(report.ledger_available);
    let codex = report
        .providers
        .iter()
        .find(|row| row.provider == "codex")
        .unwrap();
    assert_eq!(codex.ledger.as_ref().unwrap().durable_sessions, 1);
    assert_eq!(codex.quota_status, "not_queried_use_quota_report");
    let absent = diagnostics_report(None, &config, &oracle().rate_card, instant(), true).unwrap();
    assert!(!absent.ledger_available);
    assert!(absent.providers.iter().all(|row| row.ledger.is_none()));
    assert!(serde_json::to_string(&absent)
        .unwrap()
        .contains("private-root"));
}

#[test]
fn statusline_uses_local_day_bounds_and_matches_shared_ledger_prices() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("statusline");
    let bucket = TierBucket {
        model: "gpt-5.5".into(),
        service_tier: Some("fast".into()),
        tokens: TokenTotals {
            input_tokens: 1000,
            total_tokens: 1000,
            ..Default::default()
        },
    };
    for minute in [-600, -465, -430, -365, -180, 0, 1] {
        add_bucket(&mut input, &bucket, minute);
    }
    let store = ledger(root.path(), &[input]);
    let rates = oracle().rate_card;
    let report = statusline_report(
        &store,
        &rates,
        instant(),
        FixedOffset::west_opt(4 * 3600).unwrap(),
    )
    .unwrap();
    assert_eq!(report.from, instant() - Duration::hours(8));
    assert_eq!(report.tokens.total_tokens, 5000);
    let expected = range_report(
        &store,
        &rates,
        |_| "codex".into(),
        Some(report.from),
        Some(report.to),
        instant(),
    )
    .unwrap();
    assert_eq!(report.tokens, expected.tokens);
    assert_eq!(report.cost_by_currency, expected.cost_by_currency);
}

#[test]
fn empty_reports_and_unknown_provider_identity_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let store = ledger(root.path(), &[]);
    assert!(tools_report(&store, None, None)
        .unwrap()
        .providers
        .is_empty());
    assert_eq!(
        findings_report(&store, None, None)
            .unwrap()
            .summary
            .findings,
        0
    );
    assert!(
        category_report(&store, &oracle().rate_card, None, None, instant())
            .unwrap()
            .categories
            .is_empty()
    );
    assert!(provider_for_key("codex:valid").is_some());
    for key in [
        "notnamespaced",
        "codex:",
        "unknown:session",
        "Codex:session",
    ] {
        assert!(provider_for_key(key).is_none());
    }
    assert!(tools_report(
        &store,
        Some(instant()),
        Some(instant() - Duration::seconds(1))
    )
    .is_err());
    let report = statusline_report(
        &store,
        &oracle().rate_card,
        instant(),
        FixedOffset::east_opt(0).unwrap(),
    )
    .unwrap();
    assert_eq!(report.tokens.total_tokens, 0);
    assert!(report.cost_by_currency.is_empty());
}

#[test]
fn unknown_provider_usage_is_retained_without_guessed_pricing() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("future-provider");
    input.harness = ProviderId::new("future_provider").unwrap();
    let bucket = TierBucket {
        model: "base-model".into(),
        service_tier: None,
        tokens: TokenTotals {
            input_tokens: 100,
            total_tokens: 100,
            ..Default::default()
        },
    };
    add_bucket(&mut input, &bucket, 0);
    input.category_totals.insert(
        TaskCategory::Coding,
        CategoryMetric {
            turns: 1,
            tokens: bucket.tokens.clone(),
            tool_calls: 0,
            buckets: vec![bucket],
        },
    );
    let store = ledger(root.path(), &[input]);
    let rates = oracle().rate_card;
    let report = range_report(&store, &rates, |_| String::new(), None, None, instant()).unwrap();
    assert_eq!(report.tokens.total_tokens, 100);
    assert_eq!(report.by_model[0].cost, None);
    assert_eq!(report.by_model[0].basis, PricingBasis::Unavailable);
    assert!(report.cost_by_currency.is_empty());
    let sessions = session_report(
        &store,
        &rates,
        |_| String::new(),
        None,
        None,
        None,
        instant(),
    )
    .unwrap();
    assert_eq!(sessions.sessions[0].tokens.total_tokens, 100);
    assert_eq!(sessions.sessions[0].cost, None);
    let categories = category_report(&store, &rates, None, None, instant()).unwrap();
    assert_eq!(categories.categories[0].tokens.total_tokens, 100);
    assert!(categories.categories[0].harness.is_none());
    assert!(categories.categories[0].pricing.is_none());
    let line =
        statusline_report(&store, &rates, instant(), FixedOffset::east_opt(0).unwrap()).unwrap();
    assert_eq!(line.tokens.total_tokens, 100);
    assert!(line.providers[0].pricing.is_none());
    assert!(line.cost_by_currency.is_empty());
    assert!(!line.pricing_complete);
}

#[test]
fn statusline_marks_model_less_usage_unpriced_instead_of_free() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("unattributed");
    input.tokens_total = TokenTotals {
        input_tokens: 100,
        total_tokens: 100,
        ..Default::default()
    };
    input.tokens_history.push(TokenHistoryPoint {
        timestamp: instant(),
        model: None,
        service_tier: None,
        request_input_tokens: None,
        total_tokens: 100,
        delta: input.tokens_total.clone(),
    });
    let store = ledger(root.path(), &[input]);
    let line = statusline_report(
        &store,
        &oracle().rate_card,
        instant(),
        FixedOffset::east_opt(0).unwrap(),
    )
    .unwrap();
    assert_eq!(line.tokens.total_tokens, 100);
    assert_eq!(line.providers[0].unattributed_tokens.total_tokens, 100);
    assert!(!line.pricing_complete);
}

#[test]
fn shared_project_keeps_provider_currencies_separate() {
    let root = tempfile::tempdir().unwrap();
    let mut codex = session("codex-project");
    codex.working_directory = Some("/synthetic/shared-project".into());
    let bucket = TierBucket {
        model: "base-model".into(),
        service_tier: None,
        tokens: TokenTotals {
            input_tokens: 1_000_000,
            total_tokens: 1_000_000,
            ..Default::default()
        },
    };
    add_bucket(&mut codex, &bucket, 0);
    let mut claude = codex.clone();
    claude.id = "claude-project".into();
    claude.harness = claude_code_provider_id();
    let store = ledger(root.path(), &[codex, claude]);
    let rows = store.session_project_rows().unwrap();
    let canonical = rows[0].project_key.as_ref().unwrap();
    for row in &rows[1..] {
        let key = row.project_key.as_ref().unwrap();
        if key != canonical {
            store.merge_project(key, canonical).unwrap();
        }
    }
    let mut rates = oracle().rate_card;
    rates.currencies.insert("codex".into(), "credits".into());
    rates.currencies.insert("claude_code".into(), "USD".into());
    let report = project_report(
        &store,
        &rates,
        |key| provider_for_key(key).unwrap().to_string(),
        None,
        None,
        instant(),
    )
    .unwrap();
    assert_eq!(report.projects.len(), 1);
    assert_eq!(report.projects[0].cost_by_currency["credits"], 10.0);
    assert_eq!(report.projects[0].cost_by_currency["USD"], 10.0);
    assert!(report.projects[0].pricing_complete);
}

#[test]
fn project_subtotals_identify_omitted_models_and_unknown_identity() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("partial-project");
    input.working_directory = Some("/synthetic/partial-project".into());
    for (minute, model) in [(0, "base-model"), (1, "known-unpriced")] {
        add_bucket(
            &mut input,
            &TierBucket {
                model: model.into(),
                service_tier: None,
                tokens: TokenTotals {
                    input_tokens: 1_000_000,
                    total_tokens: 1_000_000,
                    ..Default::default()
                },
            },
            minute,
        );
    }
    let store = ledger(root.path(), &[input]);
    let mut rates = oracle().rate_card;
    rates.unpriced_models.push("known-unpriced".into());
    let report = project_report(&store, &rates, |_| "codex".into(), None, None, instant()).unwrap();
    assert!(!report.projects[0].pricing_complete);
    assert_eq!(report.projects[0].unpriced_models, vec!["known-unpriced"]);
    assert_eq!(
        report.projects[0].cost_by_currency.values().sum::<f64>(),
        10.0
    );
    let unknown = project_report(&store, &rates, |_| String::new(), None, None, instant()).unwrap();
    assert!(!unknown.projects[0].pricing_complete);
    assert!(unknown.projects[0].cost_by_currency.is_empty());
    assert_eq!(unknown.projects[0].tokens.total_tokens, 2_000_000);
}

#[test]
fn historical_category_totals_without_buckets_are_not_reported_as_free() {
    let root = tempfile::tempdir().unwrap();
    let mut input = session("old-categories");
    input.category_totals.insert(
        TaskCategory::Coding,
        CategoryMetric {
            turns: 1,
            tokens: TokenTotals {
                input_tokens: 200,
                total_tokens: 200,
                ..Default::default()
            },
            tool_calls: 1,
            buckets: Vec::new(),
        },
    );
    let store = ledger(root.path(), &[input]);
    let report = category_report(&store, &oracle().rate_card, None, None, instant()).unwrap();
    assert_eq!(report.categories[0].tokens.total_tokens, 200);
    assert_eq!(report.categories[0].unattributed_tokens.total_tokens, 200);
    assert!(!report.categories[0].pricing_complete);
}
