//! Frozen pricing conformance oracle (issue #47).
//!
//! The shared Rust query service now prices every desktop, CLI and MCP
//! surface. Expectations were captured from the accepted desktop engine
//! before its removal and remain fixed; there is no regeneration command.
//!
//! This suite exercises production aggregation, ledger enrichment and wire
//! serialization. The frontend suite consumes the same expected payloads
//! through real projection/export functions, with no copied pricing formula.

use odometer_lib::history_store::HistoryStore;
use odometer_lib::model::{RangeTotals, Session, TierBucket, TokenHistoryPoint, TokenTotals};
use odometer_lib::provider::ProviderId;
use odometer_lib::query::{enrich_range_pricing, price_buckets_detailed, RateTable};
use odometer_lib::rates::{PricingBasis, RateCard};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
struct Fixture {
    rate_card: RateCard,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    harness: String,
    table: String,
    buckets: Vec<TierBucket>,
    expected: Expectation,
    /// Present only where the two engines genuinely disagree today. The
    /// backend is asserted against this instead, so the difference is
    /// pinned rather than hidden: either engine moving fails this test.
    #[serde(default)]
    backend_expected: Option<Expectation>,
    /// `fix_desktop`, `fix_backend`, or `representational`.
    #[serde(default)]
    divergence_disposition: Option<String>,
    #[serde(default)]
    divergence_reason: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
struct Expectation {
    total: Option<f64>,
    by_model: Vec<ModelExpectation>,
    missing_models: Vec<String>,
    unpriced_models: Vec<String>,
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
struct ModelExpectation {
    model: String,
    cost: f64,
    basis: String,
    unpriced: bool,
}

/// Matches the fixture's own rounding, so a float's last bits cannot fail a
/// cross-language comparison that is really about pricing behaviour.
fn round(value: f64) -> f64 {
    (value * 1e9).round() / 1e9
}

fn basis_name(basis: PricingBasis) -> &'static str {
    match basis {
        PricingBasis::Direct => "direct",
        PricingBasis::Aliased => "aliased",
        PricingBasis::FloatingAlias => "floating_alias",
        PricingBasis::Fallback => "fallback",
        PricingBasis::Estimated => "estimated",
        PricingBasis::FreeLocal => "free_local",
        PricingBasis::Subscription => "subscription",
        PricingBasis::Stale => "stale",
        PricingBasis::Unavailable => "unavailable",
    }
}

/// Prices one case through the backend engine.
///
/// Calls `query::price_buckets_detailed` — the production aggregation —
/// rather than reimplementing the fold here. An oracle that reimplements
/// what it is checking proves only that the test agrees with itself; this
/// way a passing run means the code the app will actually call agrees with
/// the desktop.
fn evaluate(fixture: &Fixture, case: &Case) -> Expectation {
    let table = if case.table == "api" {
        RateTable::Api
    } else {
        RateTable::Plan
    };
    // Fixed rather than `Utc::now()`: the fixture's floating-alias expiries
    // are far past and far future precisely so neither engine's result can
    // depend on when the suite runs.
    let now = chrono::DateTime::parse_from_rfc3339("2026-08-01T00:00:00Z")
        .expect("fixed clock")
        .with_timezone(&chrono::Utc);

    let Some(priced) =
        price_buckets_detailed(&fixture.rate_card, &case.harness, &case.buckets, table, now)
    else {
        // The table does not apply to this harness at all, which the
        // desktop expresses as a null result.
        return Expectation {
            total: None,
            by_model: Vec::new(),
            missing_models: Vec::new(),
            unpriced_models: Vec::new(),
        };
    };

    Expectation {
        total: Some(round(priced.total)),
        by_model: priced
            .by_model
            .into_iter()
            .map(|entry| ModelExpectation {
                model: entry.model,
                cost: round(entry.cost),
                basis: basis_name(entry.basis).to_owned(),
                unpriced: entry.unpriced,
            })
            .collect(),
        missing_models: priced.missing_models,
        unpriced_models: priced.unpriced_models,
    }
}

fn load() -> Fixture {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/conformance/pricing-cases.json"
    );
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("could not read {path}: {error}"));
    serde_json::from_str(&raw).expect("fixture parses as the backend's own RateCard")
}

fn fixed_now() -> chrono::DateTime<chrono::Utc> {
    "2026-08-01T00:00:00Z".parse().unwrap()
}

fn raw_totals(buckets: &[TierBucket]) -> RangeTotals {
    serde_json::from_value(serde_json::json!({
        "tokens": TokenTotals::default(), "buckets": buckets
    }))
    .unwrap()
}

/// Read the actual wire surface, including snake_case provenance and null API
/// semantics, rather than assuming serializing a Rust result preserves it.
fn wire_expectation(value: &serde_json::Value) -> Expectation {
    if value.is_null() {
        return Expectation {
            total: None,
            by_model: vec![],
            missing_models: vec![],
            unpriced_models: vec![],
        };
    }
    let mut result: Expectation = serde_json::from_value(value.clone()).unwrap();
    result.total = result.total.map(round);
    for entry in &mut result.by_model {
        entry.cost = round(entry.cost);
    }
    result
}

#[test]
fn enriched_range_wire_payload_matches_every_shared_desktop_expectation() {
    let fixture = load();
    for case in &fixture.cases {
        let mut ranges = vec![HashMap::from([(
            "resident-id".into(),
            raw_totals(&case.buckets),
        )])];
        enrich_range_pricing(&mut ranges, &fixture.rate_card, fixed_now(), |_| {
            Some(ProviderId::new(&case.harness).unwrap())
        });
        let wire = serde_json::to_value(&ranges).unwrap();
        assert_eq!(
            wire_expectation(&wire[0]["resident-id"]["pricing"][&case.table]),
            case.expected,
            "{}: enriched payload must agree with the desktop oracle",
            case.name
        );
        let decoded: Vec<HashMap<String, RangeTotals>> = serde_json::from_value(wire).unwrap();
        assert!(decoded[0]["resident-id"].pricing.is_some());
    }
}

#[test]
fn enrichment_preserves_old_payloads_unknown_identity_and_absent_api_table() {
    let fixture = load();
    let raw = raw_totals(&fixture.cases[0].buckets);
    assert!(raw.pricing.is_none());
    assert!(serde_json::to_value(&raw).unwrap().get("pricing").is_none());
    let mut ranges = vec![HashMap::from([
        ("unknown".into(), raw.clone()),
        ("not_registered:session:id".into(), raw.clone()),
        ("codex:".into(), raw.clone()),
        ("codex:thread:known".into(), raw),
    ])];
    let mut card = fixture.rate_card;
    card.api_models.clear();
    enrich_range_pricing(&mut ranges, &card, fixed_now(), |_| None);
    for key in ["unknown", "not_registered:session:id", "codex:"] {
        assert!(
            ranges[0][key].pricing.is_none(),
            "{key} must not guess a provider"
        );
    }
    let wire = serde_json::to_value(&ranges).unwrap();
    assert!(wire[0]["codex:thread:known"]["pricing"]["api"].is_null());
    assert_eq!(
        wire_expectation(&wire[0]["codex:thread:known"]["pricing"]["plan"]),
        fixture.cases[0].expected
    );
    let mut empty = vec![HashMap::new()];
    enrich_range_pricing(&mut empty, &card, fixed_now(), |_| None);
    assert!(empty[0].is_empty());
}

/// Split every oracle bucket across both edge buckets and two whole hours.
/// Independent integer partitioning retains even intentionally inconsistent
/// subset fixtures exactly; no pricing formula is duplicated here.
fn quarter(tokens: &TokenTotals, index: u64) -> TokenTotals {
    let split = |n: u64| n / 4 + u64::from(index < n % 4);
    TokenTotals {
        input_tokens: split(tokens.input_tokens),
        cached_input_tokens: split(tokens.cached_input_tokens),
        cache_creation_input_tokens: split(tokens.cache_creation_input_tokens),
        output_tokens: split(tokens.output_tokens),
        reasoning_output_tokens: split(tokens.reasoning_output_tokens),
        total_tokens: split(tokens.total_tokens),
    }
}

#[test]
fn ledger_edges_and_hour_rollups_enrich_to_the_same_desktop_oracle_as_memory() {
    let fixture = load();
    let directory = tempfile::tempdir().unwrap();
    let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
    let generation = store.begin_scan().unwrap().max(1);
    let windows = vec![
        (
            Some("2026-08-01T00:15:00Z".parse().unwrap()),
            Some("2026-08-01T03:45:00Z".parse().unwrap()),
        ),
        (
            Some("2026-08-01T04:00:00Z".parse().unwrap()),
            Some("2026-08-01T04:59:59Z".parse().unwrap()),
        ),
    ];
    let template = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/sample-session.jsonl");
    let template = odometer_lib::parser::parse_file(&template, false)
        .unwrap()
        .unwrap();
    let mut memory = vec![HashMap::new(), HashMap::new()];
    let mut keys = vec![];
    for (index, case) in fixture.cases.iter().enumerate() {
        // The standalone wire test covers a literal zero bucket. The ledger
        // legitimately drops zero facts rather than retaining model labels.
        if case.buckets.is_empty() || case.buckets.iter().all(|b| b.tokens.total_tokens == 0) {
            continue;
        }
        let mut session: Session = template.clone();
        session.id = format!("oracle-{index}");
        session.harness = ProviderId::new(&case.harness).unwrap();
        session.storage_id = format!("{}:session:{}", case.harness, session.id);
        session.turns.clear();
        session.tool_observations.clear();
        session.optimization_findings.clear();
        session.tokens_history.clear();
        for hour in 0..4 {
            let timestamp = format!("2026-08-01T0{hour}:30:00Z").parse().unwrap();
            for bucket in &case.buckets {
                let delta = quarter(&bucket.tokens, hour);
                session.tokens_history.push(TokenHistoryPoint {
                    timestamp,
                    model: Some(bucket.model.clone()),
                    service_tier: bucket.service_tier.clone(),
                    request_input_tokens: None,
                    total_tokens: delta.total_tokens,
                    delta,
                });
            }
        }
        // Facts outside both partial edges must not leak into the answer.
        for at in ["2026-08-01T00:05:00Z", "2026-08-01T03:55:00Z"] {
            let mut excluded = session.tokens_history[0].clone();
            excluded.timestamp = at.parse().unwrap();
            session.tokens_history.push(excluded);
        }
        session.tokens_history.sort_by_key(|event| event.timestamp);
        let all = session.range_totals(None, None);
        session.tokens_total = all.tokens;
        session.tokens_by_model.clear();
        // This second window has a real tool call but no token usage.
        session.tool_observations.push(
            serde_json::from_value(serde_json::json!({
                "call_id": "synthetic-tool", "harness": case.harness,
                "timestamp": "2026-08-01T04:30:00Z", "model": "base-model",
                "kind": "read", "name": "read", "outcome": "success", "output_bytes": 12
            }))
            .unwrap(),
        );
        session.started_at = fixed_now();
        session.last_event_at = "2026-08-01T04:30:00Z".parse().unwrap();
        let key = session.storage_id.clone();
        store
            .observe(
                &directory.path().join(format!("session-{index}.jsonl")),
                &session,
                generation,
            )
            .unwrap();
        for (map, totals) in memory.iter_mut().zip(session.range_totals_multi(&windows)) {
            assert!(
                totals.pricing.is_none(),
                "memory aggregation stays unpriced"
            );
            map.insert(key.clone(), totals);
        }
        keys.push((key, case));
    }
    let ids: Vec<String> = keys.iter().map(|(key, _)| key.clone()).collect();
    let mut durable = store.range_totals_multi(&ids, &windows).unwrap();
    for map in &durable {
        assert!(map.values().all(|totals| totals.pricing.is_none()));
    }
    enrich_range_pricing(&mut memory, &fixture.rate_card, fixed_now(), |_| None);
    enrich_range_pricing(&mut durable, &fixture.rate_card, fixed_now(), |_| None);
    let memory_wire = serde_json::to_value(&memory).unwrap();
    let durable_wire = serde_json::to_value(&durable).unwrap();
    for (key, case) in keys {
        for window in 0..2 {
            assert_eq!(
                durable_wire[window][&key]["tokens"], memory_wire[window][&key]["tokens"],
                "{} window {window}",
                case.name
            );
            assert_eq!(
                durable_wire[window][&key]["pricing"], memory_wire[window][&key]["pricing"],
                "{} window {window}",
                case.name
            );
            assert_eq!(
                durable_wire[window][&key]["tool_metrics"],
                memory_wire[window][&key]["tool_metrics"],
                "{} window {window}",
                case.name
            );
        }
        assert_eq!(
            wire_expectation(&durable_wire[0][&key]["pricing"][&case.table]),
            case.expected,
            "{} ledger payload",
            case.name
        );
        assert_eq!(durable_wire[1][&key]["pricing"]["plan"]["total"], 0.0);
    }
}

/// The fixture is shared, so the backend must be able to load exactly what
/// the desktop loads. A `RateCard` that only deserializes on one side would
/// make the whole comparison vacuous.
#[test]
fn the_shared_fixture_parses_with_the_backend_rate_card_type() {
    let fixture = load();
    assert_eq!(
        fixture.cases.len(),
        22,
        "corpus must not silently lose cases"
    );
    assert!(fixture.rate_card.models.contains_key("base-model"));
    assert!(fixture
        .rate_card
        .floating_model_aliases
        .contains_key("floating-model"));
}

#[test]
fn backend_pricing_matches_the_frozen_desktop_oracle() {
    let fixture = load();
    let mut divergences: Vec<String> = Vec::new();

    for case in &fixture.cases {
        let actual = evaluate(&fixture, case);
        let target = case.backend_expected.as_ref().unwrap_or(&case.expected);
        if actual != *target {
            divergences.push(format!(
                "\n  case: {}\n    expected: {:?}\n    backend:  {:?}",
                case.name, target, actual
            ));
        }
    }

    assert!(
        divergences.is_empty(),
        "{} of {} pricing cases diverge from what the fixture pins. \
         Each one changes accepted pricing behaviour (issue #47).{}",
        divergences.len(),
        fixture.cases.len(),
        divergences.join("")
    );
}

/// There are no recorded divergences, and adding one must be a decision.
///
/// The fixture started with three. Two were real desktop defects, fixed in
/// #218. The last two dissolved once the backend gained the desktop's own
/// aggregation shape (`price_buckets_detailed`) instead of the test
/// approximating it — so all 22 cases now produce an identical result on
/// both sides.
///
/// `backend_expected` stays supported so a genuine, argued difference can be
/// recorded. But it is empty, and adding an entry has to be deliberate:
/// without this test it would be the path of least resistance to write down
/// whatever the backend happens to do, and the oracle becomes a rubber
/// stamp.
#[test]
fn every_recorded_divergence_is_explained_and_the_set_has_not_grown() {
    let fixture = load();
    let diverging: Vec<&Case> = fixture
        .cases
        .iter()
        .filter(|case| case.backend_expected.is_some())
        .collect();

    for case in &diverging {
        let disposition = case
            .divergence_disposition
            .as_deref()
            .unwrap_or_else(|| panic!("{}: divergence needs a disposition", case.name));
        assert!(
            matches!(
                disposition,
                "fix_desktop" | "fix_backend" | "representational"
            ),
            "{}: unknown disposition {disposition}",
            case.name
        );
        let reason = case.divergence_reason.as_deref().unwrap_or("");
        assert!(
            reason.len() > 40,
            "{}: divergence needs a reason explaining which engine is wrong and why",
            case.name
        );
    }

    for case in &diverging {
        assert_eq!(
            case.divergence_disposition.as_deref(),
            Some("representational"),
            "{}: a divergence that changes a number must be fixed, not recorded (issue #47)",
            case.name
        );
    }

    assert_eq!(
        diverging.len(),
        0,
        "a desktop/backend pricing divergence was recorded; the engines agree on every \
         case today, so adding one needs an argument in the fixture (issue #47)"
    );
}
