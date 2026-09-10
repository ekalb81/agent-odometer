//! Response-only desktop pricing. Persisted sessions remain rate-independent.
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::{Session, SessionSummary, TierBucket, TokenTotals};
use crate::provider::{codex_provider_id, ProviderRegistry};
use crate::query::{
    price_buckets_detailed, service_tier_multiplier, token_cost, PricedModel, PricedSurface,
    RangePricing, RateTable,
};
use crate::rates::{PricingBasis, PricingProvenance, PricingSurface, RateCard};

#[derive(Debug, Clone, Serialize)]
pub struct SummaryPricing {
    pub pricing: RangePricing,
    pub categories: BTreeMap<String, RangePricing>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionDetails {
    #[serde(flatten)]
    pub session: Session,
    pub pricing: SessionPricing,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionPricing {
    pub plan: PricedSurface,
    pub flat_api: Option<PricedSurface>,
    pub turn_prices: BTreeMap<String, TurnPrices>,
    pub time_aware_api: Option<TimeAwarePricing>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnPrices {
    pub plan: TurnPrice,
    pub api: Option<TurnPrice>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnPrice {
    pub cost: f64,
    pub fallback_used: bool,
    pub unpriced: bool,
    pub basis: PricingBasis,
}

#[derive(Debug, Clone, Serialize)]
pub struct TimeAwarePricing {
    #[serde(flatten)]
    pub pricing: PricedSurface,
    pub surface: PricingSurface,
    pub applied_rate_periods: Vec<String>,
    pub applied_modifiers: Vec<String>,
    pub conditional_evidence_missing: Vec<String>,
    pub cache_write_pricing_unmodeled: bool,
    pub unobserved_cache_write_input_multipliers: Vec<f64>,
    pub rules: Vec<PricingRuleSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PricingRuleSummary {
    pub id: String,
    pub label: String,
    pub from: DateTime<Utc>,
    pub to: Option<DateTime<Utc>>,
    pub provenance: PricingProvenance,
}

pub fn price_surfaces(
    buckets: &[TierBucket],
    harness: &str,
    rates: &RateCard,
    now: DateTime<Utc>,
) -> RangePricing {
    RangePricing {
        plan: price_buckets_detailed(rates, harness, buckets, RateTable::Plan, now)
            .expect("plan table always applies"),
        api: price_buckets_detailed(rates, harness, buckets, RateTable::Api, now),
    }
}

pub fn price_summary(
    summary: &SessionSummary,
    rates: &RateCard,
    now: DateTime<Utc>,
) -> SummaryPricing {
    SummaryPricing {
        pricing: price_surfaces(&summary.buckets, summary.harness.as_str(), rates, now),
        categories: summary
            .category_totals
            .iter()
            .map(|(category, metric)| {
                let name = serde_json::to_value(category)
                    .expect("category serializes")
                    .as_str()
                    .expect("category is a string")
                    .to_owned();
                (
                    name,
                    price_surfaces(&metric.buckets, summary.harness.as_str(), rates, now),
                )
            })
            .collect(),
    }
}

/// Preserves desktop `tokensCost`, including null models and its historical
/// lack of a cache-write provenance downgrade on individual turns.
pub fn price_turn(
    tokens: &TokenTotals,
    model: Option<&str>,
    tier: Option<&str>,
    harness: &str,
    rates: &RateCard,
    table: RateTable,
    now: DateTime<Utc>,
) -> TurnPrice {
    let unpriced =
        model.is_some_and(|model| rates.unpriced_models.iter().any(|value| value == model));
    let mut result = TurnPrice {
        cost: 0.0,
        fallback_used: false,
        unpriced,
        basis: PricingBasis::Unavailable,
    };
    let Some(model) = model.filter(|_| !unpriced) else {
        return result;
    };
    let table = match table {
        RateTable::Plan => &rates.models,
        RateTable::Api => &rates.api_models,
    };
    let resolution = rates.resolve_model_pricing(model, harness, table, now);
    result.basis = resolution.basis;
    result.fallback_used = resolution.basis == PricingBasis::Fallback;
    if let Some(rate) = table.get(&resolution.resolved_model) {
        result.cost = token_cost(tokens, rate, service_tier_multiplier(model, tier));
    }
    result
}

pub fn price_session_details(
    session: Session,
    rates: &RateCard,
    now: DateTime<Utc>,
) -> SessionDetails {
    let harness = session.harness.as_str();
    let history: Vec<_> = session
        .tokens_history
        .iter()
        .filter_map(|event| {
            event.model.as_ref().map(|model| TierBucket {
                model: model.clone(),
                service_tier: event.service_tier.clone(),
                tokens: event.delta.clone(),
            })
        })
        .collect();
    let fallback_buckets = session
        .tokens_history
        .is_empty()
        .then(|| session.tier_buckets());
    // Preserve per-event clamping when a reconciled subset exceeds its
    // parent counter. Merging usage first can hide that event's charge.
    let mut plan = price_buckets_detailed(
        rates,
        harness,
        fallback_buckets.as_deref().unwrap_or(&history),
        RateTable::Plan,
        now,
    )
    .expect("plan table always applies");
    if session.tokens_history.is_empty() {
        // Legacy detail fallback displays unknown model rows even without a
        // usable rate; bucket summaries intentionally omit those rows.
        for model in session.tokens_by_model.keys() {
            if !plan.by_model.iter().any(|entry| &entry.model == model) {
                let resolution = rates.resolve_model_pricing(model, harness, &rates.models, now);
                plan.by_model.push(PricedModel {
                    model: model.clone(),
                    cost: 0.0,
                    basis: resolution.basis,
                    unpriced: false,
                });
            }
        }
        plan.by_model.sort_by(|a, b| a.model.cmp(&b.model));
    }
    // API reference has always used event history, including an empty zero
    // when history is absent, independently of the plan fallback above.
    let is_codex = session.harness == codex_provider_id();
    let flat_api = price_buckets_detailed(
        rates,
        harness,
        &history,
        if is_codex {
            RateTable::Api
        } else {
            RateTable::Plan
        },
        now,
    );
    let turn_prices = session
        .turns
        .iter()
        .map(|turn| {
            (
                turn.turn_id.clone(),
                TurnPrices {
                    plan: price_turn(
                        &turn.tokens,
                        turn.model.as_deref(),
                        turn.service_tier.as_deref(),
                        harness,
                        rates,
                        RateTable::Plan,
                        now,
                    ),
                    api: (is_codex && flat_api.is_some()).then(|| {
                        price_turn(
                            &turn.tokens,
                            turn.model.as_deref(),
                            turn.service_tier.as_deref(),
                            harness,
                            rates,
                            RateTable::Api,
                            now,
                        )
                    }),
                },
            )
        })
        .collect();
    let time_aware_api = time_aware_pricing(&session, rates);
    SessionDetails {
        session,
        pricing: SessionPricing {
            plan,
            flat_api,
            turn_prices,
            time_aware_api,
        },
    }
}

fn push_unique<T: PartialEq>(values: &mut Vec<T>, value: T) {
    if !values.contains(&value) {
        values.push(value);
    }
}

pub fn time_aware_pricing(session: &Session, rates: &RateCard) -> Option<TimeAwarePricing> {
    if session.tokens_history.is_empty() || rates.pricing_catalog.rate_periods.is_empty() {
        return None;
    }
    // Preserve the desktop's current provider-to-scenario mapping.
    let surface = if session.harness == codex_provider_id() {
        PricingSurface::OpenaiApiUsd
    } else {
        PricingSurface::AnthropicApiUsd
    };
    let mut result = TimeAwarePricing {
        pricing: PricedSurface {
            total: 0.0,
            by_model: Vec::new(),
            missing_models: Vec::new(),
            unpriced_models: Vec::new(),
        },
        surface,
        applied_rate_periods: Vec::new(),
        applied_modifiers: Vec::new(),
        conditional_evidence_missing: Vec::new(),
        cache_write_pricing_unmodeled: false,
        unobserved_cache_write_input_multipliers: Vec::new(),
        rules: Vec::new(),
    };
    let mut cache_creation_observed = false;
    for event in &session.tokens_history {
        let Some(model) = event.model.as_deref() else {
            if event.delta.total_tokens > 0 {
                return None;
            }
            continue;
        };
        if rates.unpriced_models.iter().any(|value| value == model) {
            return None;
        }
        let catalog = &rates.pricing_catalog;
        let period = catalog.rate_at(surface, model, event.timestamp)?;
        let modifiers = event
            .request_input_tokens
            .map(|input| catalog.modifiers_for_request(surface, model, event.timestamp, input))
            .unwrap_or_default();
        if event.request_input_tokens.is_none() {
            for modifier in &catalog.conditional_modifiers {
                if modifier.surface == surface
                    && modifier.model == model
                    && event.timestamp >= modifier.from
                    && modifier.to.is_none_or(|end| event.timestamp < end)
                {
                    push_unique(
                        &mut result.conditional_evidence_missing,
                        modifier.id.clone(),
                    );
                }
            }
        }
        let input = modifiers
            .iter()
            .fold(1.0, |value, modifier| value * modifier.multipliers.input);
        let output = modifiers
            .iter()
            .fold(1.0, |value, modifier| value * modifier.multipliers.output);
        let mut rate = period.rate.clone();
        rate.input *= input;
        rate.cached_input *= input;
        rate.cache_creation_input = Some(period.rate.cache_creation_rate() * input);
        rate.output *= output;
        rate.reasoning *= output;
        let cost = token_cost(
            &event.delta,
            &rate,
            service_tier_multiplier(model, event.service_tier.as_deref()),
        );
        result.pricing.total += cost;
        let basis = if period.rate.cache_creation_rate_is_fallback()
            && event.delta.cache_creation_input_tokens > 0
        {
            PricingBasis::Estimated
        } else {
            PricingBasis::Direct
        };
        if let Some(existing) = result
            .pricing
            .by_model
            .iter_mut()
            .find(|entry| entry.model == model)
        {
            existing.cost += cost;
            if existing.basis != PricingBasis::Estimated {
                existing.basis = basis;
            }
        } else {
            result.pricing.by_model.push(PricedModel {
                model: model.to_owned(),
                cost,
                basis,
                unpriced: false,
            });
        }
        push_unique(&mut result.applied_rate_periods, period.id.clone());
        if let Some(multiplier) = period.cache_write_input_multiplier {
            push_unique(
                &mut result.unobserved_cache_write_input_multipliers,
                multiplier,
            );
            if event.delta.cache_creation_input_tokens > 0 {
                cache_creation_observed = true;
            }
        }
        for modifier in modifiers {
            push_unique(&mut result.applied_modifiers, modifier.id.clone());
        }
    }
    result.cache_write_pricing_unmodeled =
        !result.unobserved_cache_write_input_multipliers.is_empty() && !cache_creation_observed;
    result.rules = rates
        .pricing_catalog
        .rate_periods
        .iter()
        .filter(|rule| result.applied_rate_periods.contains(&rule.id))
        .map(|rule| PricingRuleSummary {
            id: rule.id.clone(),
            label: rule.label.clone(),
            from: rule.from,
            to: rule.to,
            provenance: rule.provenance.clone(),
        })
        .chain(
            rates
                .pricing_catalog
                .conditional_modifiers
                .iter()
                .filter(|rule| result.applied_modifiers.contains(&rule.id))
                .map(|rule| PricingRuleSummary {
                    id: rule.id.clone(),
                    label: rule.label.clone(),
                    from: rule.from,
                    to: rule.to,
                    provenance: rule.provenance.clone(),
                }),
        )
        .collect();
    Some(result)
}

pub fn enrich_correlation_pricing(
    result: &mut crate::correlation::CorrelationResult,
    rates: &RateCard,
    now: DateTime<Utc>,
) {
    for event in &mut result.results {
        for observation in [&mut event.before, &mut event.after] {
            observation.pricing_by_harness.clear();
            for descriptor in ProviderRegistry::builtin().descriptors() {
                let buckets = observation
                    .buckets_by_harness
                    .get(&descriptor.id)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                observation.pricing_by_harness.insert(
                    descriptor.id.clone(),
                    price_surfaces(buckets, descriptor.id.as_str(), rates, now),
                );
            }
            for (harness, buckets) in &observation.buckets_by_harness {
                observation
                    .pricing_by_harness
                    .entry(harness.clone())
                    .or_insert_with(|| price_surfaces(buckets, harness.as_str(), rates, now));
            }
        }
    }
}
