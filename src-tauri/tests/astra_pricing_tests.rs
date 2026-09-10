use chrono::Utc;
use odometer_lib::model::{TierBucket, TokenTotals};
use odometer_lib::query::{
    price_buckets_detailed, price_tokens, service_tier_multiplier, RateTable,
};
use odometer_lib::rates::{ModelRate, PricingBasis, RateCard};

#[test]
fn bundled_new_models_resolve_directly_with_published_dimensions() {
    let rates = RateCard::load_bundled().unwrap();
    let now = "2026-09-10T12:00:00Z".parse().unwrap();
    // One million uncached + one million cached input, and one million
    // output including half a million reasoning tokens.
    let mut tokens = TokenTotals {
        input_tokens: 2_000_000,
        cached_input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        reasoning_output_tokens: 500_000,
        total_tokens: 3_000_000,
        ..Default::default()
    };
    for (model, input, cached, output, expected) in [
        ("gpt-6-astra", 10.0, 1.0, 50.0, 61.0),
        ("gpt-5.6-cyber", 12.5, 1.25, 75.0, 88.75),
        ("gpt-5.6-terra", 2.0, 0.2, 12.0, 14.2),
        ("gpt-5.6-luna", 0.2, 0.02, 1.2, 1.42),
    ] {
        for (table, multiplier) in [(RateTable::Api, 1.0), (RateTable::Plan, 25.0)] {
            let row = match table {
                RateTable::Api => &rates.api_models[model],
                RateTable::Plan => &rates.models[model],
            };
            assert_eq!(row.input, input * multiplier, "{model}");
            assert_eq!(row.cached_input, cached * multiplier, "{model}");
            assert_eq!(row.output, output * multiplier, "{model}");
            assert_eq!(row.reasoning, output * multiplier, "{model}");
            assert_eq!(row.cache_creation_input, None, "{model}");
            let price = price_tokens(&rates, "codex", model, None, &tokens, table, now);
            assert_eq!(price.basis, PricingBasis::Direct, "{model}");
            assert_eq!(price.resolved_model, model);
            assert!(
                (price.amount.unwrap() - expected * multiplier).abs() < 1e-9,
                "{model}"
            );
        }
    }
    // Add a disjoint cache-write million, without increasing uncached input.
    tokens.input_tokens += 1_000_000;
    tokens.cache_creation_input_tokens = 1_000_000;
    tokens.total_tokens += 1_000_000;
    for (model, cached, expected) in [
        ("claude-fable-5-1", 0.25, 72.75),
        ("claude-mythos-5-1", 0.25, 72.75),
        ("claude-mythos-5", 1.0, 73.5),
    ] {
        let row = &rates.models[model];
        assert_eq!(row.input, 10.0);
        assert_eq!(row.cached_input, cached);
        assert_eq!(row.cache_creation_input, Some(12.5));
        assert_eq!(row.output, 50.0);
        assert_eq!(row.reasoning, 50.0);
        let price = price_tokens(
            &rates,
            "claude_code",
            model,
            None,
            &tokens,
            RateTable::Plan,
            now,
        );
        assert_eq!(price.basis, PricingBasis::Direct, "{model}");
        assert_eq!(price.resolved_model, model);
        assert_eq!(price.amount, Some(expected), "{model}");
    }
}

#[test]
fn bundled_daybreak_red_alias_expires_instead_of_silently_retaining_old_target() {
    let rates = RateCard::load_bundled().unwrap();
    for table in [&rates.models, &rates.api_models] {
        let active = rates.resolve_model_pricing(
            "gpt-daybreak-red-latest",
            "codex",
            table,
            "2026-12-09T23:59:59Z".parse().unwrap(),
        );
        assert_eq!(active.resolved_model, "gpt-5.6-cyber");
        assert_eq!(active.basis, PricingBasis::FloatingAlias);
        let expired = rates.resolve_model_pricing(
            "gpt-daybreak-red-latest",
            "codex",
            table,
            "2026-12-10T00:00:00Z".parse().unwrap(),
        );
        assert_eq!(expired.resolved_model, rates.fallback_models["codex"]);
        assert_eq!(expired.basis, PricingBasis::Fallback);
    }
}

#[test]
fn astra_fast_distinguishes_plan_credits_from_api_usd() {
    let mut rates = RateCard::default();
    for (table, input) in [(&mut rates.models, 250.0), (&mut rates.api_models, 10.0)] {
        table.insert(
            "gpt-6-astra".into(),
            ModelRate {
                input,
                cached_input: input / 10.0,
                output: input * 5.0,
                reasoning: input * 5.0,
                cache_creation_input: None,
            },
        );
    }
    // Cached input and reasoning remain subsets, even with fast pricing.
    let tokens = TokenTotals {
        input_tokens: 2_000_000,
        cached_input_tokens: 1_000_000,
        output_tokens: 1_000_000,
        reasoning_output_tokens: 500_000,
        total_tokens: 3_000_000,
        ..Default::default()
    };
    for (table, standard, fast) in [
        (RateTable::Plan, 1525.0, 3812.5),
        (RateTable::Api, 61.0, 122.0),
    ] {
        for (tier, expected) in [(None, standard), (Some("fast"), fast)] {
            let amount = price_tokens(
                &rates,
                "codex",
                "gpt-6-astra",
                tier,
                &tokens,
                table,
                Utc::now(),
            );
            assert_eq!(amount.amount, Some(expected));
            let turn = odometer_lib::query::price_turn(
                &tokens,
                Some("gpt-6-astra"),
                tier,
                "codex",
                &rates,
                table,
                Utc::now(),
            );
            assert_eq!(turn.cost, expected);
            let surface = price_buckets_detailed(
                &rates,
                "codex",
                &[TierBucket {
                    model: "gpt-6-astra".into(),
                    service_tier: tier.map(str::to_owned),
                    tokens: tokens.clone(),
                }],
                table,
                Utc::now(),
            )
            .unwrap();
            assert_eq!(surface.total, expected);
            assert_eq!(surface.by_model[0].cost, expected);
        }
    }
}

#[test]
fn fast_multiplier_preserves_existing_models_and_ignores_unsupported_tiers() {
    for table in [RateTable::Plan, RateTable::Api] {
        for (model, expected) in [
            ("gpt-5.5", 2.5),
            ("gpt-5.4", 2.0),
            ("gpt-6-astra-mini", 1.0),
        ] {
            assert_eq!(
                service_tier_multiplier(model, Some("fast"), table),
                expected
            );
        }
        for tier in [None, Some("default"), Some("priority")] {
            assert_eq!(service_tier_multiplier("gpt-6-astra", tier, table), 1.0);
        }
    }
}
