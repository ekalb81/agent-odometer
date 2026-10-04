//! Frozen official-source billing oracle, verified 2026-10-04. Constants are
//! deliberate; browser fixture generation must never rewrite this oracle.
use chrono::{DateTime, Utc};
use odometer_lib::history_store::HistoryStore;
use odometer_lib::model::{Session, TierBucket, TokenHistoryPoint, TokenTotals};
use odometer_lib::query::{
    price_buckets_detailed, price_session_details, price_surfaces, price_tokens, RateTable,
};
use odometer_lib::rates::{PricingBasis, RateCard};

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}
fn usage() -> TokenTotals {
    TokenTotals {
        input_tokens: 1_000_000,
        cached_input_tokens: 200_000,
        output_tokens: 500_000,
        reasoning_output_tokens: 100_000,
        total_tokens: 1_500_000,
        ..Default::default()
    }
}
fn session(model: &str, tier: Option<&str>, input: Option<u64>) -> Session {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/conformance/detail-pricing-cases.json"
    ))
    .unwrap();
    let mut s: Session = serde_json::from_value(fixture["base_session"].clone()).unwrap();
    s.model = Some(model.into());
    s.tokens_total = usage();
    s.tokens_by_model = [(model.into(), usage())].into();
    s.tokens_history = vec![TokenHistoryPoint {
        timestamp: at("2026-10-04T00:00:00Z"),
        model: Some(model.into()),
        service_tier: tier.map(str::to_owned),
        request_input_tokens: input,
        total_tokens: 1_500_000,
        delta: usage(),
    }];
    s
}
fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

#[test]
fn frozen_current_surfaces_are_distinct_and_canonical_aliases_keep_tiers() {
    let mut rates = RateCard::load_bundled().unwrap();
    let now = at("2026-10-04T00:00:00Z");
    for (model, purchased, included, api) in [
        ("gpt-6.1-sol", 331.0, 413.75, 13.24),
        ("gpt-6-sol", 332.0, 415.0, 13.28),
        ("gpt-6-luna", 16.6, 20.75, 0.664),
        ("gpt-6-astra", 1660.0, 2075.0, 66.4),
    ] {
        rates
            .model_aliases
            .insert("synthetic-alias".into(), model.into());
        for requested in [model, "synthetic-alias"] {
            // priority is the observed persisted value; fast is a documented
            // protocol/config synonym, not claimed observed in the local sample.
            for tier in ["priority", "fast"] {
                for (table, expected) in [
                    (RateTable::PurchasedCredits, purchased),
                    (RateTable::IncludedAllowance, included),
                    (RateTable::ApiEstimate, api),
                ] {
                    let value =
                        price_tokens(&rates, "codex", requested, Some(tier), &usage(), table, now);
                    close(value.amount.unwrap(), expected);
                    assert_eq!(value.resolved_model, model);
                }
            }
        }
    }
    for (table, expected) in [
        (RateTable::PurchasedCredits, 4980.0),
        (RateTable::IncludedAllowance, 6640.0),
        (RateTable::ApiEstimate, 199.2),
    ] {
        // Documented scenario only; no fabricated observed Ultrafast fixture.
        close(
            price_tokens(
                &rates,
                "codex",
                "gpt-6-astra",
                Some("ultrafast"),
                &usage(),
                table,
                now,
            )
            .amount
            .unwrap(),
            expected,
        );
    }
    rates
        .fallback_models
        .insert("codex".into(), "gpt-6.1-sol".into());
    let fallback = price_tokens(
        &rates,
        "codex",
        "unknown",
        None,
        &usage(),
        RateTable::PurchasedCredits,
        now,
    );
    assert_eq!(fallback.basis, PricingBasis::Fallback);
    close(fallback.amount.unwrap(), 165.5);
    for (model, tier) in [
        ("unknown", "priority"),
        ("gpt-6-sol", "ultrafast"),
        ("gpt-6-astra", "mystery"),
    ] {
        for table in [
            RateTable::PurchasedCredits,
            RateTable::IncludedAllowance,
            RateTable::ApiEstimate,
        ] {
            assert_eq!(
                price_tokens(&rates, "codex", model, Some(tier), &usage(), table, now).amount,
                None
            );
        }
    }
    assert_eq!(
        price_tokens(
            &rates,
            "codex",
            "gpt-6-sol",
            Some("priority"),
            &usage(),
            RateTable::PurchasedCredits,
            at("2026-10-03T23:59:59Z")
        )
        .amount,
        None
    );
}

#[test]
fn full_request_threshold_and_dates_do_not_cross_credit_allowance_surfaces() {
    let rates = RateCard::load_bundled().unwrap();
    for (model, short, long) in [
        ("gpt-6.1-sol", 6.62, 10.74),
        ("gpt-6-sol", 6.64, 10.78),
        ("gpt-6-luna", 0.332, 0.539),
        ("gpt-6-astra", 33.2, 53.9),
    ] {
        let low = price_session_details(
            session(model, Some("default"), Some(272_000)),
            &rates,
            at("2026-10-04T12:00:00Z"),
        )
        .pricing;
        let high = price_session_details(
            session(model, Some("default"), Some(272_001)),
            &rates,
            at("2026-10-04T12:00:00Z"),
        )
        .pricing;
        close(low.time_aware_api.unwrap().pricing.total, short);
        close(high.time_aware_api.unwrap().pricing.total, long);
        assert_eq!(
            low.dated_purchased_credits.as_ref().unwrap().pricing,
            high.dated_purchased_credits.as_ref().unwrap().pricing
        );
        assert_eq!(
            low.dated_included_allowance.as_ref().unwrap().pricing,
            high.dated_included_allowance.as_ref().unwrap().pricing
        );
        let missing = price_session_details(
            session(model, None, None),
            &rates,
            at("2026-10-04T12:00:00Z"),
        )
        .pricing
        .time_aware_api
        .unwrap();
        close(missing.pricing.total, short);
        assert_eq!(missing.conditional_evidence_missing.len(), 1);
        let mut old = session(model, Some("priority"), Some(272_001));
        old.tokens_history[0].timestamp = at("2026-10-03T23:59:59Z");
        assert!(
            price_session_details(old, &rates, at("2026-10-04T12:00:00Z"))
                .pricing
                .time_aware_api
                .is_none()
        );
    }
    let mut aliased = rates.clone();
    aliased
        .model_aliases
        .insert("synthetic".into(), "gpt-6-luna".into());
    let dated = price_session_details(
        session("synthetic", Some("priority"), Some(272_001)),
        &aliased,
        at("2026-10-04T12:00:00Z"),
    )
    .pricing;
    close(dated.time_aware_api.unwrap().pricing.total, 1.078);
}

#[test]
fn cache_writes_never_charge_codex_credits_or_double_count_cached_or_reasoning_subsets() {
    let rates = RateCard::load_bundled().unwrap();
    let mut tokens = usage();
    tokens.cache_creation_input_tokens = 100_000;
    close(
        price_tokens(
            &rates,
            "codex",
            "gpt-6.1-sol",
            Some("priority"),
            &tokens,
            RateTable::PurchasedCredits,
            at("2026-10-04T00:00:00Z"),
        )
        .amount
        .unwrap(),
        321.0,
    );
    close(
        price_tokens(
            &rates,
            "codex",
            "gpt-6.1-sol",
            Some("priority"),
            &tokens,
            RateTable::IncludedAllowance,
            at("2026-10-04T00:00:00Z"),
        )
        .amount
        .unwrap(),
        401.25,
    );
    let mut changed_reasoning = usage();
    changed_reasoning.reasoning_output_tokens = 400_000;
    assert_eq!(
        price_tokens(
            &rates,
            "codex",
            "gpt-6-luna",
            None,
            &usage(),
            RateTable::ApiEstimate,
            at("2026-10-04T00:00:00Z")
        )
        .amount,
        price_tokens(
            &rates,
            "codex",
            "gpt-6-luna",
            None,
            &changed_reasoning,
            RateTable::ApiEstimate,
            at("2026-10-04T00:00:00Z")
        )
        .amount
    );
}

#[test]
fn ledger_hour_rollups_and_edges_preserve_the_same_current_surface_oracle() {
    let rates = RateCard::load_bundled().unwrap();
    let directory = tempfile::tempdir().unwrap();
    let store = HistoryStore::open(&directory.path().join("history.sqlite3")).unwrap();
    let mut s = session("gpt-6.1-sol", Some("priority"), Some(272_001));
    s.tokens_history = [
        "2026-10-04T00:45:00Z",
        "2026-10-04T01:30:00Z",
        "2026-10-04T02:30:00Z",
        "2026-10-04T03:15:00Z",
    ]
    .into_iter()
    .map(|timestamp| {
        let mut e = s.tokens_history[0].clone();
        e.timestamp = at(timestamp);
        e
    })
    .collect();
    s.started_at = at("2026-10-04T00:00:00Z");
    s.last_event_at = at("2026-10-04T04:00:00Z");
    store
        .observe(
            &directory.path().join("synthetic.jsonl"),
            &s,
            store.begin_scan().unwrap().max(1),
        )
        .unwrap();
    let ranges = store
        .range_totals_multi(
            &[s.storage_id.clone()],
            &[(
                Some(at("2026-10-04T00:30:00Z")),
                Some(at("2026-10-04T03:30:00Z")),
            )],
        )
        .unwrap();
    let totals = &ranges[0][&s.storage_id];
    let priced = price_surfaces(&totals.buckets, "codex", &rates, at("2026-10-04T12:00:00Z"))
        .current
        .unwrap();
    close(priced.purchased_credits.total, 1324.0);
    close(priced.included_allowance.total, 1655.0);
    close(priced.api_estimate.total, 52.96);
    let expected = price_buckets_detailed(
        &rates,
        "codex",
        &s.tokens_history
            .iter()
            .map(|e| TierBucket {
                model: e.model.clone().unwrap(),
                service_tier: e.service_tier.clone(),
                tokens: e.delta.clone(),
            })
            .collect::<Vec<_>>(),
        RateTable::PurchasedCredits,
        at("2026-10-04T12:00:00Z"),
    )
    .unwrap();
    assert_eq!(expected, priced.purchased_credits);
}
