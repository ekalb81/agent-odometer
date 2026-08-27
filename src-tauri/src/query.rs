//! UI-independent read-only query and pricing service (issue #47).
//!
//! Issue #47's first scope bullet asks for "a UI-independent
//! application/query service", and its DRY boundary is explicit: "Tauri
//! commands, CLI, API, MCP, statusline, tray, and exports adapt one query
//! service. No surface owns SQL, pricing, quota forecasting, provider
//! parsing, or project normalization."
//!
//! Before this module there were already two pricing implementations —
//! `credits.ts` for the desktop, and a simplified private copy in
//! `turn_receipts.rs` for the hook. The second one resolved a model by
//! direct lookup with a fallback and nothing else, so it silently ignored
//! `model_aliases`, `floating_model_aliases`, `free_local_models`, and card
//! staleness. A receipt for an aliased model was therefore priced at the
//! harness fallback rate while the desktop priced the same usage correctly.
//!
//! This module is that one Rust-side implementation. It is deliberately free
//! of Tauri types so a CLI, a local API, or an MCP server can call it with
//! nothing but a `HistoryStore` and a `RateCard`.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::history_store::HistoryStore;
use crate::model::{RangeTotals, TierBucket, TokenTotals};
use crate::provider::codex_provider_id;
use crate::rates::{ModelRate, PricingBasis, RateCard};

/// Which rate table a query prices against.
///
/// Codex publishes both a plan-credit rate and an OpenAI API USD rate for
/// the same models; every other provider has one table. Keeping this an
/// explicit argument rather than a guess means a caller always states which
/// currency it is reporting in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateTable {
    /// `RateCard::models` — plan credits for Codex, USD elsewhere.
    Plan,
    /// `RateCard::api_models` — OpenAI API USD rates, Codex only.
    Api,
}

/// One priced amount plus the provenance that produced it.
///
/// `amount: None` is "not priceable", which is distinct from a priced zero
/// and must stay distinct all the way to whatever renders it — the same
/// contract `PricingBasis` exists to carry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PricedAmount {
    pub amount: Option<f64>,
    pub basis: PricingBasis,
    /// The rate-table key actually used, which differs from the requested
    /// model whenever an alias or a fallback resolved it.
    pub resolved_model: String,
}

/// Prices one bucket of usage through the card's own resolver.
///
/// The resolver is [`RateCard::resolve_model_pricing`] — the same one the
/// desktop uses — so aliases, floating aliases, free/local declarations,
/// known-unpriced models, and card staleness all behave identically here.
pub fn price_tokens(
    rates: &RateCard,
    harness: &str,
    model: &str,
    service_tier: Option<&str>,
    tokens: &TokenTotals,
    table: RateTable,
    now: DateTime<Utc>,
) -> PricedAmount {
    let rate_table = match table {
        RateTable::Api => &rates.api_models,
        RateTable::Plan => &rates.models,
    };
    // Codex is the only provider with an API table; asking for one anywhere
    // else is unanswerable rather than zero.
    if table == RateTable::Api
        && (harness != codex_provider_id().as_str() || rates.api_models.is_empty())
    {
        return PricedAmount {
            amount: None,
            basis: PricingBasis::Unavailable,
            resolved_model: model.to_owned(),
        };
    }

    let resolution = rates.resolve_model_pricing(model, harness, rate_table, now);
    match resolution.basis {
        // Explicitly zero by declaration, not "no rate found".
        PricingBasis::FreeLocal => PricedAmount {
            amount: Some(0.0),
            basis: resolution.basis,
            resolved_model: resolution.resolved_model,
        },
        PricingBasis::Unavailable => PricedAmount {
            amount: None,
            basis: resolution.basis,
            resolved_model: resolution.resolved_model,
        },
        _ => {
            let Some(rate) = rate_table.get(&resolution.resolved_model) else {
                return PricedAmount {
                    amount: None,
                    basis: PricingBasis::Unavailable,
                    resolved_model: resolution.resolved_model,
                };
            };
            let amount = token_cost(tokens, rate, service_tier_multiplier(model, service_tier));
            PricedAmount {
                amount: Some(amount),
                basis: crate::rates::downgrade_for_cache_creation_fallback(
                    resolution.basis,
                    rate,
                    tokens.cache_creation_input_tokens,
                ),
                resolved_model: resolution.resolved_model,
            }
        }
    }
}

/// Cost of `tokens` at `rate`, in the rate's own currency unit.
///
/// Cached-read and cache-creation are both disjoint subsets of
/// `input_tokens`; both are subtracted before pricing the remainder at the
/// plain input rate, so neither subset is ever priced twice. Reasoning
/// output is likewise a subset of `output_tokens`.
pub fn token_cost(tokens: &TokenTotals, rate: &ModelRate, multiplier: f64) -> f64 {
    let non_cached_input = tokens
        .input_tokens
        .saturating_sub(tokens.cached_input_tokens)
        .saturating_sub(tokens.cache_creation_input_tokens);
    let non_reasoning_output = tokens
        .output_tokens
        .saturating_sub(tokens.reasoning_output_tokens);
    ((non_cached_input as f64 * rate.input)
        + (tokens.cached_input_tokens as f64 * rate.cached_input)
        + (tokens.cache_creation_input_tokens as f64 * rate.cache_creation_rate())
        + (non_reasoning_output as f64 * rate.output)
        + (tokens.reasoning_output_tokens as f64 * rate.reasoning))
        / 1_000_000.0
        * multiplier
}

/// Service-tier price multiplier for a model, where the provider charges one.
pub fn service_tier_multiplier(model: &str, service_tier: Option<&str>) -> f64 {
    if service_tier != Some("fast") {
        return 1.0;
    }
    match model {
        "gpt-5.5" => 2.5,
        "gpt-5.4" => 2.0,
        _ => 1.0,
    }
}

/// Usage for one model in a reported window.
#[derive(Debug, Clone, Serialize)]
pub struct ModelUsage {
    pub model: String,
    /// Provider that produced this usage, which is what decides the
    /// currency — Codex bills in plan credits, Claude in USD.
    pub harness: String,
    pub tokens: TokenTotals,
    /// Cost in `currency`, absent when not priceable.
    pub cost: Option<f64>,
    pub currency: String,
    pub basis: PricingBasis,
}

/// A date-range usage report, aggregated from the durable ledger.
///
/// `schema_version` is part of the payload rather than a header, so a
/// consumer that stores or pipes the JSON keeps the version with the data
/// (issue #47: "Schemas are versioned").
#[derive(Debug, Clone, Serialize)]
pub struct RangeReport {
    pub schema_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub sessions: usize,
    /// Sessions that contributed no usage in the window. Reported rather
    /// than filtered out silently, so "no activity" is distinguishable from
    /// "the query missed them".
    pub sessions_without_usage: usize,
    pub tokens: TokenTotals,
    pub by_model: Vec<ModelUsage>,
    /// Totals keyed by currency, never summed across them.
    ///
    /// Codex bills in plan credits and Claude in USD; adding them produces a
    /// number that is not money in any unit. The desktop keeps them separate
    /// for exactly this reason, and a CLI that quietly summed them would
    /// disagree with it while looking tidier.
    pub cost_by_currency: BTreeMap<String, f64>,
    /// Models excluded from `cost` because no rate resolved for them, so a
    /// total is never quietly a floor presented as complete.
    pub unpriced_models: Vec<String>,
}

/// The schema version of [`RangeReport`]. Bump on any incompatible change;
/// additive optional fields do not require one.
pub const RANGE_REPORT_SCHEMA_VERSION: u32 = 1;

/// Builds a date-range report straight from the ledger's rollups.
///
/// Never parses a transcript and never runs a scan — issue #47 requires a
/// statusline-grade path that "does not trigger a full corpus parse on every
/// shell prompt", and reading the durable rollups is what makes that
/// possible.
pub fn range_report(
    store: &HistoryStore,
    rates: &RateCard,
    harness_for: impl Fn(&str) -> String,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<RangeReport> {
    let keys = store.session_keys()?;
    let windows = vec![(from, to)];
    let per_window = store.range_totals_multi(&keys, &windows)?;
    let totals = per_window.into_iter().next().unwrap_or_default();

    let mut tokens = TokenTotals::default();
    // Keyed by (harness, model), not model alone: two providers can use the
    // same model name, and folding them together would attribute all of that
    // usage to whichever session was seen first — including its currency.
    let mut by_model: BTreeMap<(String, String), TokenTotals> = BTreeMap::new();
    let mut sessions_with_usage = 0usize;

    for key in &keys {
        let Some(range) = totals.get(key) else {
            continue;
        };
        if range.tokens.total_tokens == 0 && range.buckets.is_empty() {
            continue;
        }
        sessions_with_usage += 1;
        accumulate(&mut tokens, &range.tokens);
        let harness = harness_for(key);
        for bucket in &range.buckets {
            let entry = by_model
                .entry((harness.clone(), bucket.model.clone()))
                .or_default();
            accumulate(entry, &bucket.tokens);
        }
    }

    let mut cost_by_currency: BTreeMap<String, f64> = BTreeMap::new();
    let mut unpriced: BTreeSet<String> = BTreeSet::new();
    let mut models = Vec::with_capacity(by_model.len());
    for ((harness, model), model_tokens) in by_model {
        let currency = rates
            .currencies
            .get(&harness)
            .cloned()
            .unwrap_or_else(|| rates.currency.clone());
        let priced = price_tokens(
            rates,
            &harness,
            &model,
            None,
            &model_tokens,
            RateTable::Plan,
            now,
        );
        match priced.amount {
            Some(amount) => {
                *cost_by_currency.entry(currency.clone()).or_insert(0.0) += amount;
            }
            None => {
                unpriced.insert(model.clone());
            }
        }
        models.push(ModelUsage {
            model,
            harness,
            tokens: model_tokens,
            cost: priced.amount,
            currency,
            basis: priced.basis,
        });
    }

    Ok(RangeReport {
        schema_version: RANGE_REPORT_SCHEMA_VERSION,
        from,
        to,
        sessions: sessions_with_usage,
        sessions_without_usage: keys.len().saturating_sub(sessions_with_usage),
        tokens,
        by_model: models,
        cost_by_currency,
        unpriced_models: unpriced.into_iter().collect(),
    })
}

fn accumulate(into: &mut TokenTotals, from: &TokenTotals) {
    into.input_tokens += from.input_tokens;
    into.cached_input_tokens += from.cached_input_tokens;
    into.cache_creation_input_tokens += from.cache_creation_input_tokens;
    into.output_tokens += from.output_tokens;
    into.reasoning_output_tokens += from.reasoning_output_tokens;
    into.total_tokens += from.total_tokens;
}

/// Sums the priceable buckets of a single session's range totals, for a
/// caller that already holds them (the turn-receipt hook).
pub fn price_range_totals(
    rates: &RateCard,
    harness: &str,
    range: &RangeTotals,
    table: RateTable,
    now: DateTime<Utc>,
) -> (Option<f64>, Vec<String>) {
    price_buckets(rates, harness, &range.buckets, table, now)
}

/// Sums the priceable buckets in `buckets`, returning the total and the
/// models that could not be priced.
///
/// A model that cannot be priced is reported rather than treated as zero:
/// the caller needs to be able to say the total is a floor.
pub fn price_buckets(
    rates: &RateCard,
    harness: &str,
    buckets: &[TierBucket],
    table: RateTable,
    now: DateTime<Utc>,
) -> (Option<f64>, Vec<String>) {
    let mut total = 0.0;
    let mut priced = false;
    let mut omitted: BTreeSet<String> = BTreeSet::new();
    for bucket in buckets {
        let amount = price_tokens(
            rates,
            harness,
            &bucket.model,
            bucket.service_tier.as_deref(),
            &bucket.tokens,
            table,
            now,
        );
        match amount.amount {
            Some(value) => {
                total += value;
                priced = true;
            }
            None => {
                omitted.insert(bucket.model.clone());
            }
        }
    }
    (priced.then_some(total), omitted.into_iter().collect())
}

/// How far back a quota query loads sessions.
///
/// Provider windows run from a few hours to a week; a fortnight of
/// *last-observed* history covers the longest of them with room for a
/// late-arriving or clock-skewed observation, while still loading a handful
/// of sessions rather than the corpus. Issue #43's honest-state requirement cuts both ways here — too
/// short a lookback would report "no data" for a window that has evidence,
/// which is a wrong answer rather than a missing one.
pub const QUOTA_LOOKBACK_DAYS: i64 = 14;

/// Provider quota snapshots, built from recently-active sessions only
/// (issue #43).
///
/// Rate-limit observations live inside `session_json`, so this is the one
/// query in this module that materializes sessions. It bounds that by
/// `last_seen_at_ms` through the index #168 added, so a quota answer costs
/// a few sessions rather than the whole ledger — which is what lets one
/// quota service feed a CLI or statusline as well as the desktop, per #43's
/// DRY boundary.
///
/// `max_cache_age` marks a reading stale rather than hiding it. A stale
/// number is a different and more honest fact than no number, and both are
/// different from zero usage — the distinction #43's first acceptance
/// criterion is about.
pub fn quota_snapshots(
    store: &HistoryStore,
    now: DateTime<Utc>,
    max_cache_age: chrono::Duration,
) -> Result<Vec<crate::quota::QuotaSnapshot>> {
    let cutoff = now - chrono::Duration::days(QUOTA_LOOKBACK_DAYS);
    let keys = store.session_keys_since(cutoff.timestamp_millis())?;
    let index = crate::quota::QuotaPointsIndex::new();
    for stored in store.load_many(&keys)? {
        index.update_session(&stored.key, &stored.session);
    }
    Ok(index.snapshots(now, max_cache_age))
}

/// Usage for one project in a reported window.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectUsage {
    /// Effective (post-merge) project key. Always safe to emit: it is a
    /// hash (`repo:…` / `path:…`), never a path.
    pub project_key: String,
    /// Local alias when set, else the auto-computed label. For a
    /// `fallback_path_identity` project the auto label *is* an absolute
    /// local path, so a caller that emits this off-device must redact —
    /// see [`ProjectUsage::redacted_label`].
    pub label: String,
    /// True when `label` is an absolute local path rather than a project
    /// name, because no repository or workspace root was found.
    pub label_is_path: bool,
    pub sessions: usize,
    pub tokens: TokenTotals,
    /// Totals keyed by currency, never summed across them — the same rule
    /// [`RangeReport`] follows and for the same reason.
    pub cost_by_currency: BTreeMap<String, f64>,
}

/// A per-project usage report (issue #47's `projects` command, over #41's
/// project dimension).
#[derive(Debug, Clone, Serialize)]
pub struct ProjectReport {
    pub schema_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub projects: Vec<ProjectUsage>,
    /// Sessions with usage in the window that belong to no project — a real
    /// state (no working directory), reported rather than dropped so the
    /// per-project totals can be reconciled against the overall one.
    pub sessions_without_project: usize,
}

pub const PROJECT_REPORT_SCHEMA_VERSION: u32 = 1;

impl ProjectUsage {
    /// The label as it should appear in anything that can leave the device.
    ///
    /// `project_identity`'s contract puts redaction at "the presentation
    /// layer, applied where data leaves the desktop". A CLI is such a layer:
    /// its output gets piped into files, pasted into issues, and read by
    /// agents. So a path-derived label falls back to the already-hashed
    /// project key, which stays stable and correlatable across runs without
    /// naming a directory on this machine.
    pub fn redacted_label(&self) -> &str {
        if self.label_is_path {
            &self.project_key
        } else {
            &self.label
        }
    }

    /// [`Self::redacted_label`], or the exact label when the caller has
    /// explicitly opted in to local paths.
    pub fn label_for(&self, include_paths: bool) -> &str {
        if include_paths {
            &self.label
        } else {
            self.redacted_label()
        }
    }
}

/// Groups windowed usage by project, applying the local alias and merge
/// overrides (#41) exactly as the desktop does.
///
/// Reads project identity from `durable_sessions`' own columns rather than
/// from session content, so this costs a column scan plus the rollup reads
/// `range_report` already makes — not a walk of the corpus.
pub fn project_report(
    store: &HistoryStore,
    rates: &RateCard,
    harness_for: impl Fn(&str) -> String,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<ProjectReport> {
    let overrides: std::collections::HashMap<String, crate::history_store::ProjectOverrideRow> =
        store
            .list_project_overrides()?
            .into_iter()
            .map(|row| (row.project_key.clone(), row))
            .collect();
    let session_overrides = store.list_session_project_overrides()?;
    let rows = store.session_project_rows()?;
    let keys: Vec<String> = rows.iter().map(|row| row.session_key.clone()).collect();
    let totals = store
        .range_totals_multi(&keys, &[(from, to)])?
        .into_iter()
        .next()
        .unwrap_or_default();

    struct Accumulated {
        label: String,
        label_is_path: bool,
        sessions: usize,
        tokens: TokenTotals,
        buckets: Vec<TierBucket>,
        harness: String,
    }
    let mut by_project: BTreeMap<String, Accumulated> = BTreeMap::new();
    let mut without_project = 0usize;

    for row in &rows {
        let Some(range) = totals.get(&row.session_key) else {
            continue;
        };
        if range.tokens.total_tokens == 0 && range.buckets.is_empty() {
            continue;
        }
        // A per-session reassignment (#41) wins over the auto-computed key,
        // then merges fold that into its canonical project.
        let raw = session_overrides
            .get(&row.session_key)
            .cloned()
            .or_else(|| row.project_key.clone());
        let Some(raw) = raw else {
            without_project += 1;
            continue;
        };
        let canonical = crate::history_store::resolve_canonical_project_key(&overrides, &raw);
        let alias = overrides
            .get(&canonical)
            .and_then(|row| row.display_label.clone());
        let aliased = alias.is_some();
        let label = alias
            .or_else(|| row.label.clone())
            .unwrap_or_else(|| canonical.clone());

        let entry = by_project.entry(canonical).or_insert_with(|| Accumulated {
            label,
            // An explicit local alias replaces the path entirely, so a
            // renamed project is no longer path-identified.
            label_is_path: !aliased && row.provenance.as_deref() == Some("fallback_path_identity"),
            sessions: 0,
            tokens: TokenTotals::default(),
            buckets: Vec::new(),
            harness: harness_for(&row.session_key),
        });
        entry.sessions += 1;
        accumulate(&mut entry.tokens, &range.tokens);
        entry.buckets.extend(range.buckets.iter().cloned());
    }

    let projects = by_project
        .into_iter()
        .map(|(project_key, accumulated)| {
            // Only the path-identity case carries a path: a repository or
            // workspace root yields a name.
            let label_is_path = accumulated.label_is_path;
            let mut cost_by_currency: BTreeMap<String, f64> = BTreeMap::new();
            let currency = rates
                .currencies
                .get(&accumulated.harness)
                .cloned()
                .unwrap_or_else(|| rates.currency.clone());
            let (cost, _unpriced) = price_buckets(
                rates,
                &accumulated.harness,
                &accumulated.buckets,
                RateTable::Plan,
                now,
            );
            if let Some(cost) = cost {
                cost_by_currency.insert(currency, cost);
            }
            ProjectUsage {
                project_key,
                label: accumulated.label,
                label_is_path,
                sessions: accumulated.sessions,
                tokens: accumulated.tokens,
                cost_by_currency,
            }
        })
        .collect();

    Ok(ProjectReport {
        schema_version: PROJECT_REPORT_SCHEMA_VERSION,
        from,
        to,
        projects,
        sessions_without_project: without_project,
    })
}
