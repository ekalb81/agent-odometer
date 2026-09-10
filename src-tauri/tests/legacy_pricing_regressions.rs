//! Regression gaps retained when the duplicate TypeScript pricing engine was
//! removed. The frozen conformance corpus covers the remaining migrated cases;
//! alias cycles and bundled dimension guards already live in Rust unit tests.
use chrono::{DateTime, Utc};
use odometer_lib::model::{Session, TierBucket, TokenHistoryPoint, TokenTotals};
use odometer_lib::query::{price_buckets_detailed, price_session_details, RateTable};
use odometer_lib::rates::{ModelRate, PricingBasis, RateCard};
use serde_json::{json, Value};

fn fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/conformance/detail-pricing-cases.json"
    ))
    .unwrap()
}

fn card() -> RateCard {
    serde_json::from_value(fixture()["rate_card"].clone()).unwrap()
}

fn now() -> DateTime<Utc> {
    "2026-08-01T00:00:00Z".parse().unwrap()
}

fn bucket(input_tokens: u64, cache_creation_input_tokens: u64) -> TierBucket {
    TierBucket {
        model: "base-model".into(),
        service_tier: None,
        tokens: TokenTotals {
            input_tokens,
            cache_creation_input_tokens,
            total_tokens: input_tokens,
            ..TokenTotals::default()
        },
    }
}

#[test]
fn fractional_events_keep_sub_cent_precision_without_per_event_rounding() {
    let rates = card();
    let buckets = vec![bucket(333_333, 0); 37];
    let priced = price_buckets_detailed(&rates, "codex", &buckets, RateTable::Plan, now()).unwrap();
    assert!((priced.total - 123.33321).abs() < 1e-9);
    assert!((priced.by_model[0].cost - 123.33321).abs() < 1e-9);

    let tiny =
        price_buckets_detailed(&rates, "codex", &[bucket(1, 0)], RateTable::Plan, now()).unwrap();
    assert!(tiny.total > 0.0);
    assert!((tiny.total - 0.00001).abs() < 1e-12);
}

#[test]
fn explicit_zero_cache_write_price_is_direct_while_missing_premium_is_estimated() {
    let mut rates = card();
    rates
        .models
        .get_mut("base-model")
        .unwrap()
        .cache_creation_input = Some(0.0);
    let buckets = [bucket(50_000, 50_000)];
    let free = price_buckets_detailed(&rates, "codex", &buckets, RateTable::Plan, now()).unwrap();
    assert_eq!(free.total, 0.0);
    assert_eq!(free.by_model[0].basis, PricingBasis::Direct);
    assert!(!free.by_model[0].unpriced);
    assert!(free.missing_models.is_empty());

    rates
        .models
        .get_mut("base-model")
        .unwrap()
        .cache_creation_input = None;
    let estimated =
        price_buckets_detailed(&rates, "codex", &buckets, RateTable::Plan, now()).unwrap();
    assert_eq!(estimated.total, 0.5);
    assert_eq!(estimated.by_model[0].basis, PricingBasis::Estimated);
}

#[test]
fn an_unresolvable_model_is_reported_without_a_misleading_free_row() {
    let mut rates = card();
    rates.models.clear();
    let priced = price_buckets_detailed(
        &rates,
        "codex",
        &[bucket(1_000_000, 0)],
        RateTable::Plan,
        now(),
    )
    .unwrap();
    assert_eq!(priced.total, 0.0);
    assert!(priced.by_model.is_empty());
    assert_eq!(priced.missing_models, ["base-model"]);
    assert!(priced.unpriced_models.is_empty());
}

#[test]
fn floating_alias_expiry_is_inclusive_through_the_last_utc_second() {
    let mut rates = card();
    rates
        .floating_model_aliases
        .get_mut("floating-model")
        .unwrap()
        .expires_at = "2026-11-13".parse().unwrap();
    for (at, basis, cost) in [
        ("2026-11-13T23:59:59Z", PricingBasis::FloatingAlias, 10.0),
        ("2026-11-14T00:00:00Z", PricingBasis::Fallback, 99.0),
    ] {
        let mut usage = bucket(1_000_000, 0);
        usage.model = "floating-model".into();
        let result = price_buckets_detailed(
            &rates,
            "codex",
            &[usage],
            RateTable::Plan,
            at.parse().unwrap(),
        )
        .unwrap();
        assert_eq!(result.total, cost);
        assert_eq!(result.by_model[0].basis, basis);
    }
}

#[test]
fn an_undatable_floating_alias_cannot_enter_the_backend_rate_card() {
    let mut value = fixture()["rate_card"].clone();
    value["floating_model_aliases"]["floating-model"]["expires_at"] = json!("not-a-date");
    assert!(serde_json::from_value::<RateCard>(value).is_err());
}

#[test]
fn a_resumed_delta_does_not_replace_direct_request_threshold_evidence() {
    let source = fixture();
    let rates = card();
    let mut session: Session = serde_json::from_value(source["base_session"].clone()).unwrap();
    session.tokens_history = serde_json::from_value(json!([{
        "timestamp": "2026-08-01T00:30:00Z",
        "model": "base-model", "service_tier": null,
        "request_input_tokens": 50, "total_tokens": 400_000,
        "delta": bucket(400_000, 0).tokens,
    }]))
    .unwrap();
    // The fixture threshold is >100 request tokens. The 400K delta represents
    // resumed history reconciliation and must not trigger that request rule.
    let details = price_session_details(session, &rates, now());
    let dated = details.pricing.time_aware_api.unwrap();
    assert_eq!(dated.pricing.total, 4.0);
    assert!(dated.applied_modifiers.is_empty());
    assert!(dated.conditional_evidence_missing.is_empty());
    assert_eq!(dated.applied_rate_periods, ["before"]);
}

#[test]
fn detail_plan_clamps_each_observed_event_before_merging_costs() {
    let mut rates = card();
    let rate = ModelRate {
        input: 10.0,
        cached_input: 1.0,
        cache_creation_input: Some(12.5),
        output: 20.0,
        reasoning: 30.0,
    };
    rates.models.insert("synthetic".into(), rate.clone());
    rates.api_models.insert("synthetic".into(), rate);
    let input = TokenTotals {
        input_tokens: 100,
        total_tokens: 100,
        ..TokenTotals::default()
    };
    let output = TokenTotals {
        output_tokens: 100,
        total_tokens: 100,
        ..TokenTotals::default()
    };
    // Independently reconciled counters can make one event's subset exceed
    // its parent even when the cumulative session totals are consistent.
    // Legacy detail pricing clamps that event before summing its cost with
    // other events; merging the usage first silently removes a real charge.
    for (name, events, expected) in [
        (
            "cached input",
            [
                input.clone(),
                TokenTotals {
                    cached_input_tokens: 200,
                    ..input.clone()
                },
            ],
            0.0012,
        ),
        (
            "cache creation",
            [
                input.clone(),
                TokenTotals {
                    cache_creation_input_tokens: 200,
                    ..input.clone()
                },
            ],
            0.0035,
        ),
        (
            "combined cache subsets",
            [
                input.clone(),
                TokenTotals {
                    cached_input_tokens: 100,
                    cache_creation_input_tokens: 100,
                    ..input.clone()
                },
            ],
            0.00235,
        ),
        (
            "reasoning output",
            [
                output.clone(),
                TokenTotals {
                    reasoning_output_tokens: 200,
                    ..output
                },
            ],
            0.008,
        ),
    ] {
        let mut session: Session =
            serde_json::from_value(fixture()["base_session"].clone()).unwrap();
        session.tokens_history = events
            .into_iter()
            .map(|delta| TokenHistoryPoint {
                timestamp: now(),
                model: Some("synthetic".into()),
                service_tier: None,
                request_input_tokens: None,
                total_tokens: delta.total_tokens,
                delta,
            })
            .collect();
        let details = price_session_details(session, &rates, now());
        assert!(
            (details.pricing.plan.total - expected).abs() < 1e-12,
            "{name}: expected {expected}, got {}",
            details.pricing.plan.total
        );
        assert_eq!(details.pricing.plan.by_model.len(), 1);
        assert_eq!(
            details.pricing.plan.by_model[0].cost,
            details.pricing.plan.total
        );
        assert_eq!(details.pricing.plan.by_model[0].basis, PricingBasis::Direct);
        assert_eq!(details.pricing.plan, details.pricing.flat_api.unwrap());
    }
}
