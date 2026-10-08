//! Frozen monetary and boundary checks for provider cards verified 2026-10-08.
//! Expected costs are manual constants, independent of the pricing implementation.
use chrono::{DateTime, Utc};
use odometer_lib::model::{Session, TokenHistoryPoint, TokenTotals, TurnInfo};
use odometer_lib::query::{price_session_details, price_tokens, RateTable};
use odometer_lib::rates::{PricingBasis, RateCard};

fn instant(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

fn usage(cache_write: bool) -> TokenTotals {
    TokenTotals {
        input_tokens: 1_000_000,
        cached_input_tokens: 200_000,
        cache_creation_input_tokens: if cache_write { 100_000 } else { 0 },
        output_tokens: 500_000,
        reasoning_output_tokens: 100_000,
        total_tokens: 1_500_000,
    }
}

fn session(model: &str, harness: &str, tier: Option<&str>, tokens: TokenTotals) -> Session {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/conformance/detail-pricing-cases.json"
    ))
    .unwrap();
    let mut session: Session = serde_json::from_value(fixture["base_session"].clone()).unwrap();
    session.harness = odometer_lib::provider::ProviderId::new(harness).unwrap();
    session.model = Some(model.into());
    session.tokens_total = tokens.clone();
    session.tokens_by_model = std::collections::HashMap::from([(model.into(), tokens.clone())]);
    session.tokens_history = vec![TokenHistoryPoint {
        timestamp: instant("2026-10-08T12:00:00Z"),
        model: Some(model.into()),
        service_tier: tier.map(str::to_owned),
        request_input_tokens: None,
        total_tokens: tokens.total_tokens,
        delta: tokens.clone(),
    }];
    session.turns = vec![TurnInfo {
        turn_id: "synthetic-price-check".into(),
        model: Some(model.into()),
        service_tier: tier.map(str::to_owned),
        tokens,
        ..Default::default()
    }];
    session
}

fn assert_amount(actual: Option<f64>, expected: f64) {
    assert!(
        actual.is_some_and(|amount| (amount - expected).abs() < 1e-10),
        "expected {expected}, received {actual:?}"
    );
}

#[test]
fn sol_standard_fast_and_priority_use_each_surfaces_rate_without_double_counting() {
    let rates = RateCard::load_bundled().unwrap();
    let now = instant("2026-10-08T12:00:00Z");
    for (model, basis) in [
        ("gpt-5.6-sol", PricingBasis::Direct),
        ("gpt-5.6", PricingBasis::Aliased),
    ] {
        for (table, base, fast) in [
            (RateTable::Plan, 332.0, 830.0),
            (RateTable::PurchasedCredits, 332.0, 664.0),
            (RateTable::IncludedAllowance, 332.0, 830.0),
            (RateTable::Api, 13.28, 26.56),
            (RateTable::ApiEstimate, 13.28, 26.56),
        ] {
            for (tier, expected) in [(None, base), (Some("fast"), fast), (Some("priority"), fast)] {
                let priced = price_tokens(&rates, "codex", model, tier, &usage(false), table, now);
                assert_eq!(priced.basis, basis, "{model} {table:?} {tier:?}");
                assert_eq!(priced.resolved_model, "gpt-5.6-sol");
                assert_amount(priced.amount, expected);
            }
            assert_eq!(
                price_tokens(
                    &rates,
                    "codex",
                    model,
                    Some("fast"),
                    &usage(false),
                    table,
                    instant("2026-10-07T23:59:59Z"),
                )
                .amount,
                None,
                "{model} {table:?}: verification must not invent older Fast pricing"
            );
        }
    }
    let raw = session("gpt-5.6-sol", "codex", Some("fast"), usage(false));
    let details = price_session_details(raw.clone(), &rates, now);
    assert_eq!(
        serde_json::to_value(&details.session).unwrap(),
        serde_json::to_value(raw).unwrap()
    );
    assert_amount(Some(details.pricing.plan.total), 830.0);
    assert_amount(Some(details.pricing.flat_api.unwrap().total), 26.56);
    let current = details.pricing.current.unwrap();
    assert_amount(Some(current.purchased_credits.total), 664.0);
    assert_amount(Some(current.included_allowance.total), 830.0);
    assert_amount(Some(current.api_estimate.total), 26.56);
    let turn = &details.pricing.turn_prices["synthetic-price-check"];
    assert_amount(Some(turn.plan.cost), 830.0);
    assert_amount(Some(turn.api.as_ref().unwrap().cost), 26.56);
    assert!(!turn.plan.unpriced);
    assert!(!turn.api.as_ref().unwrap().unpriced);
}

#[test]
fn haiku_direct_reference_and_strict_long_context_boundary_preserve_raw_usage() {
    let rates = RateCard::load_bundled().unwrap();
    let now = instant("2026-10-08T12:00:00Z");
    let direct = price_tokens(
        &rates,
        "claude_code",
        "claude-haiku-5-5",
        None,
        &usage(true),
        RateTable::Plan,
        now,
    );
    assert_eq!(direct.basis, PricingBasis::Direct);
    assert_eq!(direct.resolved_model, "claude-haiku-5-5");
    assert_amount(direct.amount, 0.3345);
    for (input, expected, modifiers) in [(100_000, 0.03345, 0), (100_001, 0.1672505, 1)] {
        let tokens = TokenTotals {
            input_tokens: input,
            cached_input_tokens: 20_000,
            cache_creation_input_tokens: 10_000,
            output_tokens: 50_000,
            reasoning_output_tokens: 10_000,
            total_tokens: input + 50_000,
        };
        let mut raw = session("claude-haiku-5-5", "claude_code", None, tokens);
        raw.tokens_history[0].request_input_tokens = Some(input);
        let details = price_session_details(raw.clone(), &rates, now);
        assert_eq!(
            serde_json::to_value(&details.session).unwrap(),
            serde_json::to_value(&raw).unwrap()
        );
        let dated = details.pricing.time_aware_api.unwrap();
        assert_amount(Some(dated.pricing.total), expected);
        assert_eq!(dated.applied_rate_periods.len(), 1);
        assert_eq!(dated.applied_modifiers.len(), modifiers);
        assert!(dated.conditional_evidence_missing.is_empty());
        assert_eq!(dated.pricing.by_model[0].basis, PricingBasis::Direct);
        assert!(!dated.cache_write_pricing_unmodeled);
        raw.tokens_history[0].timestamp = instant("2026-10-06T23:59:59Z");
        assert!(price_session_details(raw, &rates, now)
            .pricing
            .time_aware_api
            .is_none());
    }
}

#[test]
fn unsupported_speed_and_opaque_review_identity_remain_unavailable() {
    let rates = RateCard::load_bundled().unwrap();
    let now = instant("2026-10-08T12:00:00Z");
    for table in [
        RateTable::Plan,
        RateTable::PurchasedCredits,
        RateTable::IncludedAllowance,
        RateTable::Api,
        RateTable::ApiEstimate,
    ] {
        for (model, tier) in [
            ("gpt-5.6-sol", Some("ultrafast")),
            ("unknown-synthetic-model", Some("fast")),
            ("codex-auto-review", None),
            ("codex-auto-review", Some("fast")),
        ] {
            let priced = price_tokens(&rates, "codex", model, tier, &usage(false), table, now);
            assert_eq!(priced.amount, None, "{model} {table:?} {tier:?}");
            assert_eq!(priced.basis, PricingBasis::Unavailable);
        }
    }
    let raw = session("codex-auto-review", "codex", None, usage(false));
    let details = price_session_details(raw.clone(), &rates, now);
    assert_eq!(details.session.tokens_total, raw.tokens_total);
    assert_eq!(details.pricing.plan.unpriced_models, ["codex-auto-review"]);
    assert_eq!(
        details.pricing.flat_api.unwrap().unpriced_models,
        ["codex-auto-review"]
    );
    assert!(details.pricing.time_aware_api.is_none());
}
