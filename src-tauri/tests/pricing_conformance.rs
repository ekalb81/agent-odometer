//! Cross-engine pricing conformance, backend half (issue #47).
//!
//! Odometer prices usage twice: in `src/lib/credits.ts` for the desktop, and
//! in `query.rs` for the CLI and MCP server. #47's DRY boundary says one
//! query service should serve every surface, and its first acceptance
//! criterion is that equivalent queries reconcile across them. Nothing
//! enforced that, so the two could drift silently.
//!
//! This test and `src/lib/pricingConformance.test.ts` read the *same*
//! fixture and assert against the *same* `expected` block, so agreement with
//! that file is agreement with each other — no cross-language process
//! invocation, and the fixture is reviewable on its own.
//!
//! Expectations are generated from the desktop engine, because that is what
//! users see today. A failure here therefore means "the CLI would report a
//! different number than the app", which is the thing #47 exists to prevent.

use std::collections::BTreeMap;

use odometer_lib::model::TierBucket;
use odometer_lib::query::{price_tokens, RateTable};
use odometer_lib::rates::{PricingBasis, RateCard};
use serde::Deserialize;

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

/// Prices one case through the backend engine, shaped like the desktop's
/// `SessionCredits` so the two are directly comparable.
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

    let mut by_model: BTreeMap<String, (f64, PricingBasis, bool)> = BTreeMap::new();
    let mut missing: Vec<String> = Vec::new();
    let mut unpriced: Vec<String> = Vec::new();
    let mut total = 0.0;
    let mut any_answerable = false;

    for bucket in &case.buckets {
        let priced = price_tokens(
            &fixture.rate_card,
            &case.harness,
            &bucket.model,
            bucket.service_tier.as_deref(),
            &bucket.tokens,
            table,
            now,
        );
        let is_unpriced = fixture.rate_card.unpriced_models.contains(&bucket.model);
        if is_unpriced && !unpriced.contains(&bucket.model) {
            unpriced.push(bucket.model.clone());
        }
        if matches!(
            priced.basis,
            PricingBasis::Fallback | PricingBasis::Unavailable
        ) && !is_unpriced
            && !missing.contains(&bucket.model)
        {
            missing.push(bucket.model.clone());
        }
        let amount = priced.amount.unwrap_or(0.0);
        if priced.amount.is_some() {
            any_answerable = true;
        }
        total += amount;
        let entry =
            by_model
                .entry(bucket.model.clone())
                .or_insert((0.0, priced.basis, is_unpriced));
        entry.0 += amount;
        // Once any bucket for a model downgrades to `estimated`, a later
        // cleaner bucket must not paper back over it — the desktop engine
        // has the same rule.
        if entry.1 != PricingBasis::Estimated {
            entry.1 = priced.basis;
        }
    }

    missing.sort();
    unpriced.sort();

    Expectation {
        total: if case.buckets.is_empty() || any_answerable {
            Some(round(total))
        } else {
            None
        },
        by_model: by_model
            .into_iter()
            .map(|(model, (cost, basis, is_unpriced))| ModelExpectation {
                model,
                cost: round(cost),
                basis: basis_name(basis).to_owned(),
                unpriced: is_unpriced,
            })
            .collect(),
        missing_models: missing,
        unpriced_models: unpriced,
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

/// The fixture is shared, so the backend must be able to load exactly what
/// the desktop loads. A `RateCard` that only deserializes on one side would
/// make the whole comparison vacuous.
#[test]
fn the_shared_fixture_parses_with_the_backend_rate_card_type() {
    let fixture = load();
    assert!(!fixture.cases.is_empty());
    assert!(fixture.rate_card.models.contains_key("base-model"));
    assert!(fixture
        .rate_card
        .floating_model_aliases
        .contains_key("floating-model"));
}

#[test]
fn backend_pricing_matches_the_desktop_engine() {
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
         Each one is a number the CLI would report differently from the app (issue #47).{}",
        divergences.len(),
        fixture.cases.len(),
        divergences.join("")
    );
}

/// Every recorded divergence must carry a disposition and a reason.
///
/// Without this, `backend_expected` becomes a way to silence a real
/// disagreement by writing down whatever the backend happens to do — which
/// would turn the oracle into a rubber stamp. Three are known today, and the
/// count is pinned so a fourth cannot appear unnoticed.
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
        2,
        "the known desktop/backend pricing divergences changed; update this count \
         deliberately and say why in the fixture (issue #47)"
    );
}
