//! Frozen Standard-price oracle from official provider cards verified 2026-10-04.
//! Expected amounts are deliberate constants; no fixture regeneration computes them.
use chrono::{DateTime, Utc};
use odometer_lib::model::{Session, TierBucket, TokenHistoryPoint, TokenTotals};
use odometer_lib::query::{price_buckets_detailed, price_session_details, price_tokens, RateTable};
use odometer_lib::rates::{PricingBasis, RateCard};

fn instant(value: &str) -> DateTime<Utc> {
    value.parse().unwrap()
}

fn tokens(cache_write: bool) -> TokenTotals {
    TokenTotals {
        input_tokens: 1_000_000,
        cached_input_tokens: 200_000,
        cache_creation_input_tokens: if cache_write { 100_000 } else { 0 },
        output_tokens: 500_000,
        reasoning_output_tokens: 100_000,
        total_tokens: 1_500_000,
    }
}

#[test]
fn current_standard_prices_cover_new_models_without_fallback_or_double_counting() {
    let rates = RateCard::load_bundled().unwrap();
    let now = instant("2026-10-04T00:00:00Z");
    for (model, harness, table, writes, expected) in [
        ("gpt-6.1-sol", "codex", RateTable::Plan, false, 165.5),
        ("gpt-6-sol", "codex", RateTable::Plan, false, 166.0),
        ("gpt-6-luna", "codex", RateTable::Plan, false, 8.3),
        ("gpt-6.1-sol", "codex", RateTable::Api, false, 6.62),
        ("gpt-6-sol", "codex", RateTable::Api, false, 6.64),
        ("gpt-6-luna", "codex", RateTable::Api, false, 0.332),
        (
            "claude-opus-5-5",
            "claude_code",
            RateTable::Plan,
            true,
            13.34,
        ),
        (
            "claude-sonnet-5-5",
            "claude_code",
            RateTable::Plan,
            true,
            6.69,
        ),
        (
            "gemini-3.8-flash",
            "gemini_cli",
            RateTable::Plan,
            false,
            2.49,
        ),
        (
            "gemini-3.5-flash",
            "gemini_cli",
            RateTable::Plan,
            false,
            5.73,
        ),
        (
            "gemini-3.5-flash-lite",
            "gemini_cli",
            RateTable::Plan,
            false,
            1.496,
        ),
        (
            "gemini-3.1-flash-lite",
            "gemini_cli",
            RateTable::Plan,
            false,
            0.955,
        ),
    ] {
        let priced = price_tokens(&rates, harness, model, None, &tokens(writes), table, now);
        assert_eq!(priced.basis, PricingBasis::Direct, "{model}");
        assert!((priced.amount.unwrap() - expected).abs() < 1e-10, "{model}");
    }
    assert!(
        rates.models.contains_key("codex-auto-review"),
        "historical row survives"
    );
    assert_eq!(
        price_tokens(
            &rates,
            "codex",
            "codex-auto-review",
            None,
            &tokens(false),
            RateTable::Plan,
            now
        )
        .amount,
        None
    );
}

#[test]
fn gemini_rollover_uses_event_dates_but_expired_flat_reference_is_unavailable() {
    let mut rates = RateCard::load_bundled().unwrap();
    let before = instant("2026-12-31T23:59:59Z");
    let after = instant("2027-01-01T00:00:00Z");
    let source: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/conformance/detail-pricing-cases.json"
    ))
    .unwrap();
    let mut session: Session = serde_json::from_value(source["base_session"].clone()).unwrap();
    session.harness = odometer_lib::provider::gemini_cli_provider_id();
    session.model = Some("gemini-3.8-flash".into());
    session.tokens_history = [before, after]
        .into_iter()
        .map(|timestamp| TokenHistoryPoint {
            timestamp,
            model: session.model.clone(),
            service_tier: None,
            request_input_tokens: Some(1_000_000),
            total_tokens: 1_500_000,
            delta: tokens(false),
        })
        .collect();
    session.turns = vec![odometer_lib::model::TurnInfo {
        turn_id: "synthetic-turn".into(),
        model: session.model.clone(),
        tokens: tokens(false),
        ..Default::default()
    }];
    let dated = price_session_details(session.clone(), &rates, before)
        .pricing
        .time_aware_api
        .unwrap();
    assert_eq!(
        dated.surface,
        odometer_lib::rates::PricingSurface::GeminiApiUsd
    );
    assert!((dated.pricing.total - 7.47).abs() < 1e-10);
    assert_eq!(dated.applied_rate_periods.len(), 2);
    let bucket = TierBucket {
        model: "gemini-3.8-flash".into(),
        service_tier: None,
        tokens: tokens(false),
    };
    let flat = price_buckets_detailed(
        &rates,
        "gemini_cli",
        std::slice::from_ref(&bucket),
        RateTable::Plan,
        before,
    )
    .unwrap();
    assert!((flat.total - 2.49).abs() < 1e-10);
    let expired =
        price_buckets_detailed(&rates, "gemini_cli", &[bucket], RateTable::Plan, after).unwrap();
    assert_eq!(expired.total, 0.0);
    assert!(expired.by_model[0].unpriced);
    assert!(expired.missing_models.is_empty());
    assert_eq!(expired.unpriced_models, ["gemini-3.8-flash"]);
    let details = price_session_details(session, &rates, after);
    assert_eq!(details.pricing.plan.total, 0.0);
    assert_eq!(details.pricing.plan.unpriced_models, ["gemini-3.8-flash"]);
    assert_eq!(details.session.tokens_history.len(), 2);
    assert_eq!(details.session.tokens_history[1].delta, tokens(false));
    assert!(
        details
            .pricing
            .turn_prices
            .values()
            .next()
            .unwrap()
            .plan
            .unpriced
    );
    rates
        .model_aliases
        .insert("temporary-alias".into(), "gemini-3.8-flash".into());
    assert_eq!(
        price_tokens(
            &rates,
            "gemini_cli",
            "temporary-alias",
            None,
            &tokens(false),
            RateTable::Plan,
            after
        )
        .amount,
        None
    );
    // A deliberate custom reference is usable, but has no bundled verification.
    rates.models.get_mut("gemini-3.8-flash").unwrap().input = 1.5;
    rates
        .models
        .get_mut("gemini-3.8-flash")
        .unwrap()
        .cached_input = 0.15;
    rates.models.get_mut("gemini-3.8-flash").unwrap().output = 7.5;
    rates.models.get_mut("gemini-3.8-flash").unwrap().reasoning = 7.5;
    rates.flat_rate_expires_at.remove("gemini-3.8-flash");
    rates.rate_provenance.remove("models/gemini-3.8-flash");
    rates.upgrade_review.push("models/gemini-3.8-flash".into());
    let revised: RateCard = serde_json::from_value(serde_json::to_value(rates).unwrap()).unwrap();
    assert!(
        (price_tokens(
            &revised,
            "gemini_cli",
            "gemini-3.8-flash",
            None,
            &tokens(false),
            RateTable::Plan,
            after
        )
        .amount
        .unwrap()
            - 4.98)
            .abs()
            < 1e-10
    );
    assert!(!revised
        .rate_provenance
        .contains_key("models/gemini-3.8-flash"));
}

#[test]
fn aliases_and_fallbacks_cannot_charge_an_excluded_historical_row() {
    let mut rates = RateCard::load_bundled().unwrap();
    rates
        .model_aliases
        .insert("user-review".into(), "codex-auto-review".into());
    rates.floating_model_aliases.insert(
        "floating-review".into(),
        odometer_lib::rates::FloatingAlias {
            target: "codex-auto-review".into(),
            expires_at: instant("2027-01-01T00:00:00Z").date_naive(),
            source_url: "https://example.test/synthetic".into(),
        },
    );
    rates
        .fallback_models
        .insert("codex".into(), "codex-auto-review".into());
    let now = instant("2026-10-04T00:00:00Z");
    for model in ["user-review", "floating-review", "unknown-synthetic"] {
        assert_eq!(
            price_tokens(
                &rates,
                "codex",
                model,
                None,
                &tokens(false),
                RateTable::Plan,
                now
            )
            .amount,
            None
        );
        let bucket = TierBucket {
            model: model.into(),
            service_tier: None,
            tokens: tokens(false),
        };
        let totals =
            price_buckets_detailed(&rates, "codex", &[bucket], RateTable::Plan, now).unwrap();
        assert_eq!(totals.unpriced_models, [model]);
        assert_eq!(totals.by_model[0].basis, PricingBasis::Unavailable);
        let source: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/conformance/detail-pricing-cases.json"
        ))
        .unwrap();
        let mut session: Session = serde_json::from_value(source["base_session"].clone()).unwrap();
        session.model = Some(model.into());
        session.tokens_total = tokens(false);
        session.tokens_by_model = std::collections::HashMap::from([(model.into(), tokens(false))]);
        session.tokens_history.clear();
        session.turns = vec![odometer_lib::model::TurnInfo {
            turn_id: "excluded".into(),
            model: session.model.clone(),
            tokens: tokens(false),
            ..Default::default()
        }];
        let details = price_session_details(session, &rates, now);
        assert_eq!(details.pricing.plan.unpriced_models, [model]);
        assert!(details.pricing.turn_prices["excluded"].plan.unpriced);
        assert_eq!(details.session.tokens_total, tokens(false));
    }
}

#[test]
fn identical_gemini_events_reject_anthropic_rules_and_accept_only_gemini_rules() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/conformance/detail-pricing-cases.json"
    ))
    .unwrap();
    let case = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == "Gemini retains legacy Anthropic scenario mapping")
        .unwrap();
    let mut session_value = fixture["base_session"].clone();
    session_value
        .as_object_mut()
        .unwrap()
        .extend(case["session"].as_object().unwrap().clone());
    let session: Session = serde_json::from_value(session_value).unwrap();
    let mut rates: RateCard = serde_json::from_value(fixture["rate_card"].clone()).unwrap();
    let now = instant("2026-08-01T00:00:00Z");
    let wrong_surface = price_session_details(session.clone(), &rates, now);
    assert!(wrong_surface.pricing.time_aware_api.is_none());
    assert!((wrong_surface.pricing.plan.total - 11.45).abs() < 1e-10);
    let mut matching = rates
        .pricing_catalog
        .rate_periods
        .iter()
        .find(|period| period.id == "anthropic")
        .unwrap()
        .clone();
    matching.id = "synthetic-gemini".into();
    matching.surface = odometer_lib::rates::PricingSurface::GeminiApiUsd;
    rates.pricing_catalog.rate_periods.push(matching);
    let correct_surface = price_session_details(session, &rates, now);
    assert_eq!(correct_surface.pricing.plan, wrong_surface.pricing.plan);
    let dated = correct_surface.pricing.time_aware_api.unwrap();
    assert!((dated.pricing.total - 11.45).abs() < 1e-10);
    assert_eq!(dated.applied_rate_periods, ["synthetic-gemini"]);
}
