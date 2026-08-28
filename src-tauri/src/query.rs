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
use chrono::{DateTime, TimeZone, Timelike, Utc};
use serde::{Deserialize, Serialize};

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
    /// The same totals restated in the user's display currency, where that
    /// can be done honestly (issue #42). Additive: `cost_by_currency` stays
    /// the authoritative figure, and this is a restatement carrying its own
    /// rate and provenance. Empty when no display currency is configured.
    pub converted: Vec<ConvertedTotal>,
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

    let converted = convert_totals(rates, &cost_by_currency);
    Ok(RangeReport {
        schema_version: RANGE_REPORT_SCHEMA_VERSION,
        from,
        to,
        sessions: sessions_with_usage,
        sessions_without_usage: keys.len().saturating_sub(sessions_with_usage),
        tokens,
        by_model: models,
        cost_by_currency,
        converted,
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

/// One model's priced usage in a window, shaped exactly like the desktop's
/// `ModelCredit` (issue #47).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricedModel {
    pub model: String,
    pub cost: f64,
    pub basis: PricingBasis,
    /// The model is declared unpriceable, so `cost` is a placeholder rather
    /// than a measurement. Distinct from a fallback-priced model, whose cost
    /// is real but approximate.
    pub unpriced: bool,
}

/// A window's usage priced against one rate table, with the provenance the
/// desktop needs to render it (issue #47).
///
/// `price_buckets` returns only a total and a list of omitted models, which
/// is enough for a CLI line but loses the per-model breakdown, the
/// per-model basis, and the difference between "no rate published" and
/// "explicitly unpriceable" — all of which the desktop renders. That gap is
/// why the two engines could not share an aggregation, so this closes it:
/// the shape here is the one `credits.ts`'s `bucketsCost` produces, and
/// `tests/pricing_conformance.rs` asserts they agree case for case.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricedSurface {
    pub total: f64,
    pub by_model: Vec<PricedModel>,
    /// Models priced from a fallback rate, or not priced at all — the set
    /// the UI warns about. Excludes explicitly unpriced models, which are a
    /// different and quieter state.
    pub missing_models: Vec<String>,
    pub unpriced_models: Vec<String>,
}

/// Prices a window's buckets against one rate table, with full provenance.
///
/// `None` means the table does not apply to this harness at all — the API
/// table for a non-Codex session — which is a different answer from "priced
/// at zero" and lets a caller hide the column rather than render a nought.
/// Mirrors `apiCostFromBuckets` returning null.
pub fn price_buckets_detailed(
    rates: &RateCard,
    harness: &str,
    buckets: &[TierBucket],
    table: RateTable,
    now: DateTime<Utc>,
) -> Option<PricedSurface> {
    if table == RateTable::Api
        && (harness != codex_provider_id().as_str() || rates.api_models.is_empty())
    {
        return None;
    }

    let rate_table = match table {
        RateTable::Api => &rates.api_models,
        RateTable::Plan => &rates.models,
    };

    let mut by_model: BTreeMap<String, (f64, PricingBasis, bool)> = BTreeMap::new();
    let mut missing: BTreeSet<String> = BTreeSet::new();
    let mut unpriced: BTreeSet<String> = BTreeSet::new();
    let mut total = 0.0;

    for bucket in buckets {
        let is_unpriced = rates.unpriced_models.contains(&bucket.model);
        let resolution = rates.resolve_model_pricing(&bucket.model, harness, rate_table, now);

        if is_unpriced && resolution.basis == PricingBasis::Unavailable {
            unpriced.insert(bucket.model.clone());
            by_model
                .entry(bucket.model.clone())
                .or_insert((0.0, PricingBasis::Unavailable, true));
            continue;
        }
        if matches!(
            resolution.basis,
            PricingBasis::Fallback | PricingBasis::Unavailable
        ) {
            missing.insert(bucket.model.clone());
        }

        let Some(rate) = rate_table.get(&resolution.resolved_model) else {
            // No rate row. A free/local model is a declared zero worth
            // showing; anything else is already reported through
            // `missing_models`, and a zero row would read as "this was
            // free".
            if resolution.basis == PricingBasis::FreeLocal {
                by_model.entry(bucket.model.clone()).or_insert((
                    0.0,
                    PricingBasis::FreeLocal,
                    false,
                ));
            }
            continue;
        };

        let cost = token_cost(
            &bucket.tokens,
            rate,
            service_tier_multiplier(&bucket.model, bucket.service_tier.as_deref()),
        );
        total += cost;
        let basis = crate::rates::downgrade_for_cache_creation_fallback(
            resolution.basis,
            rate,
            bucket.tokens.cache_creation_input_tokens,
        );
        let entry = by_model
            .entry(bucket.model.clone())
            .or_insert((0.0, basis, false));
        entry.0 += cost;
        // A model can appear in several buckets, one per service tier. Once
        // any of them downgrades to `Estimated`, a later cleaner bucket must
        // not paper back over it.
        if entry.1 != PricingBasis::Estimated {
            entry.1 = basis;
        }
    }

    Some(PricedSurface {
        total,
        by_model: by_model
            .into_iter()
            .map(|(model, (cost, basis, unpriced))| PricedModel {
                model,
                cost,
                basis,
                unpriced,
            })
            .collect(),
        missing_models: missing.into_iter().collect(),
        unpriced_models: unpriced.into_iter().collect(),
    })
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

// ---------------------------------------------------------------------------
// Workflow metrics (issue #45)
// ---------------------------------------------------------------------------

/// One workflow metric, with the evidence behind it (issue #45).
///
/// #45 requires that "every metric has a documented denominator, coverage
/// rule, missing-data state". All three are carried in the value rather than
/// only in prose, because a ratio without its sample size is not
/// interpretable: 1.0 from one observation and 1.0 from ten thousand are the
/// same number and completely different facts.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowMetric {
    /// Stable identity, safe to key on across versions.
    pub id: &'static str,
    /// What the denominator counts, in one line, so a reader never has to
    /// guess what the ratio is *of*.
    pub denominator_is: &'static str,
    /// `None` when the denominator is zero — the missing-data state. This is
    /// deliberately not 0.0: "no tool calls happened" and "no tool call
    /// failed" are opposite facts about the same number.
    pub value: Option<f64>,
    pub numerator: f64,
    pub denominator: f64,
}

impl WorkflowMetric {
    fn new(
        id: &'static str,
        denominator_is: &'static str,
        numerator: f64,
        denominator: f64,
    ) -> Self {
        Self {
            id,
            denominator_is,
            // A zero denominator yields no value rather than a fabricated
            // one; `f64` division would produce NaN or infinity here and
            // poison anything that aggregated it.
            value: (denominator > 0.0).then(|| numerator / denominator),
            numerator,
            denominator,
        }
    }
}

/// A versioned set of workflow metrics over a window (issue #45).
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowMetrics {
    pub schema_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub sessions: usize,
    pub metrics: Vec<WorkflowMetric>,
}

/// Version of the metric *definitions*, not the payload shape.
///
/// #45 asks for versioned metrics because a changed denominator silently
/// changes the meaning of a series. Bump this when any metric's definition
/// changes, so a stored comparison can refuse to compare across versions
/// rather than plot a discontinuity as a trend.
pub const WORKFLOW_METRICS_VERSION: u32 = 1;

/// Computes workflow metrics from ledger rollups over a window.
///
/// Deterministic and provider-agnostic: every input is a durable fact
/// already aggregated by the ledger, so two runs over the same window give
/// the same answer, and nothing here re-parses a transcript.
///
/// Deliberately a subset of #45's list. These five are the ones the existing
/// rollups can answer honestly; the rest (median time to first edit,
/// planning/delegation rate, tools per turn) need per-turn timing the
/// rollups do not retain, and inventing them from what is stored would
/// produce a confident number with no evidence behind it.
pub fn workflow_metrics(
    store: &HistoryStore,
    rates: &RateCard,
    harness_for: impl Fn(&str) -> String,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<WorkflowMetrics> {
    let keys = store.session_keys()?;
    let totals = store
        .range_totals_multi(&keys, &[(from, to)])?
        .into_iter()
        .next()
        .unwrap_or_default();

    let mut tokens = TokenTotals::default();
    let mut tools = crate::model::ToolMetrics::default();
    let mut sessions = 0usize;
    let mut priced_tokens = 0f64;
    let mut total_priceable_tokens = 0f64;

    for key in &keys {
        let Some(range) = totals.get(key) else {
            continue;
        };
        if range.tokens.total_tokens == 0 && range.buckets.is_empty() {
            continue;
        }
        sessions += 1;
        accumulate(&mut tokens, &range.tokens);
        accumulate_tools(&mut tools, &range.tool_metrics);

        let harness = harness_for(key);
        for bucket in &range.buckets {
            let priced = price_tokens(
                rates,
                &harness,
                &bucket.model,
                bucket.service_tier.as_deref(),
                &bucket.tokens,
                RateTable::Plan,
                now,
            );
            let bucket_tokens = bucket.tokens.total_tokens as f64;
            total_priceable_tokens += bucket_tokens;
            // Only a direct or aliased resolution is real coverage. A
            // fallback rate produces a number, but it is a stand-in — and
            // counting it as covered is how a pricing gap hides.
            if matches!(
                priced.basis,
                PricingBasis::Direct | PricingBasis::Aliased | PricingBasis::FloatingAlias
            ) {
                priced_tokens += bucket_tokens;
            }
        }
    }

    let attempted_tools = (tools.successes + tools.failures) as f64;
    let reworked = tools
        .mutation_targets
        .saturating_sub(tools.one_shot_mutations) as f64;

    let metrics = vec![
        WorkflowMetric::new(
            "tool_failure_rate",
            "tool calls whose outcome was recorded as success or failure",
            tools.failures as f64,
            attempted_tools,
        ),
        WorkflowMetric::new(
            "mutation_rework_rate",
            "distinct targets mutated at least once",
            reworked,
            tools.mutation_targets as f64,
        ),
        WorkflowMetric::new(
            "context_to_output_ratio",
            "output tokens produced",
            tokens.input_tokens as f64,
            tokens.output_tokens as f64,
        ),
        WorkflowMetric::new(
            "cached_input_share",
            "input tokens",
            tokens.cached_input_tokens as f64,
            tokens.input_tokens as f64,
        ),
        WorkflowMetric::new(
            "pricing_coverage",
            "tokens in priceable (model-attributed) buckets",
            priced_tokens,
            total_priceable_tokens,
        ),
    ];

    Ok(WorkflowMetrics {
        schema_version: WORKFLOW_METRICS_VERSION,
        from,
        to,
        sessions,
        metrics,
    })
}

fn accumulate_tools(into: &mut crate::model::ToolMetrics, from: &crate::model::ToolMetrics) {
    into.calls += from.calls;
    into.reads += from.reads;
    into.searches += from.searches;
    into.mutations += from.mutations;
    into.commands += from.commands;
    into.other += from.other;
    into.successes += from.successes;
    into.failures += from.failures;
    into.unknown += from.unknown;
    into.mutation_targets += from.mutation_targets;
    into.one_shot_mutations += from.one_shot_mutations;
    into.retry_count += from.retry_count;
    into.duration_ms += from.duration_ms;
}

/// One session's usage in a window (issue #47's `sessions` command).
#[derive(Debug, Clone, Serialize)]
pub struct SessionUsage {
    pub session_key: String,
    pub harness: String,
    pub tokens: TokenTotals,
    pub cost: Option<f64>,
    pub currency: String,
    /// Models that could not be priced, so a per-session cost is never
    /// quietly a floor presented as a total.
    pub unpriced_models: Vec<String>,
}

/// A per-session listing over a window.
#[derive(Debug, Clone, Serialize)]
pub struct SessionReport {
    pub schema_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Sessions with usage in the window, ordered by total tokens
    /// descending — the order a person scanning for "what cost the most"
    /// actually wants.
    pub sessions: Vec<SessionUsage>,
    /// How many sessions the listing was truncated to, when `limit` cut it.
    /// `None` when everything is shown, so a truncated list can never be
    /// mistaken for a complete one.
    pub truncated_to: Option<usize>,
}

pub const SESSION_REPORT_SCHEMA_VERSION: u32 = 1;

/// Per-session usage over a window, heaviest first.
///
/// `limit` bounds the listing. A corpus of thousands of sessions makes an
/// unbounded list unusable in a terminal, but silently truncating one is
/// worse than a long list — so `truncated_to` records when it happened.
pub fn session_report(
    store: &HistoryStore,
    rates: &RateCard,
    harness_for: impl Fn(&str) -> String,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    limit: Option<usize>,
    now: DateTime<Utc>,
) -> Result<SessionReport> {
    let keys = store.session_keys()?;
    let totals = store
        .range_totals_multi(&keys, &[(from, to)])?
        .into_iter()
        .next()
        .unwrap_or_default();

    let mut sessions = Vec::new();
    for key in &keys {
        let Some(range) = totals.get(key) else {
            continue;
        };
        if range.tokens.total_tokens == 0 && range.buckets.is_empty() {
            continue;
        }
        let harness = harness_for(key);
        let (cost, unpriced_models) =
            price_buckets(rates, &harness, &range.buckets, RateTable::Plan, now);
        let currency = rates
            .currencies
            .get(&harness)
            .cloned()
            .unwrap_or_else(|| rates.currency.clone());
        sessions.push(SessionUsage {
            session_key: key.clone(),
            harness,
            tokens: range.tokens.clone(),
            cost,
            currency,
            unpriced_models,
        });
    }

    // Ties broken by key so the order is deterministic across runs — a
    // listing that reshuffles equal rows is unusable for diffing.
    sessions.sort_by(|a, b| {
        b.tokens
            .total_tokens
            .cmp(&a.tokens.total_tokens)
            .then_with(|| a.session_key.cmp(&b.session_key))
    });

    let truncated_to = limit.filter(|limit| *limit < sessions.len());
    if let Some(limit) = truncated_to {
        sessions.truncate(limit);
    }

    Ok(SessionReport {
        schema_version: SESSION_REPORT_SCHEMA_VERSION,
        from,
        to,
        sessions,
        truncated_to,
    })
}

// ---------------------------------------------------------------------------
// Activity heatmap (issue #48)
// ---------------------------------------------------------------------------

/// Usage in one hour of one local day, for a heatmap grid.
#[derive(Debug, Clone, Serialize)]
pub struct ActivityCell {
    /// Local date, `YYYY-MM-DD`.
    pub date: String,
    /// Local hour, 0-23.
    pub hour: u32,
    pub total_tokens: u64,
    pub sessions: u64,
}

/// A heatmap of when work happened (issue #48).
#[derive(Debug, Clone, Serialize)]
pub struct ActivityHeatmap {
    pub schema_version: u32,
    /// Fixed offset from UTC in seconds that the cells are expressed in.
    /// Recorded because a heatmap read in the wrong zone is not slightly
    /// wrong, it is shifted — "I work late" and "I work early" are the same
    /// data eight hours apart.
    pub utc_offset_seconds: i32,
    pub cells: Vec<ActivityCell>,
    /// Busiest cell, or `None` when there is no activity at all. Callers
    /// scale a colour ramp by this; scaling by a fabricated 0 would paint an
    /// empty grid as uniformly maximal.
    pub peak_total_tokens: Option<u64>,
}

pub const ACTIVITY_HEATMAP_SCHEMA_VERSION: u32 = 1;

/// Builds an hour-of-day heatmap over a window.
///
/// Bucketing happens in the caller's local zone, not UTC: a heatmap exists
/// to answer "when do I work", and hour-of-day in UTC answers a question
/// nobody asked. The offset used is recorded on the result so a consumer can
/// tell which zone it is looking at.
pub fn activity_heatmap(
    store: &HistoryStore,
    offset: chrono::FixedOffset,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<ActivityHeatmap> {
    const HOUR_MS: i64 = 3_600_000;
    let from_bucket = from.map(|at| at.timestamp_millis().div_euclid(HOUR_MS));
    let to_bucket = to.map(|at| at.timestamp_millis().div_euclid(HOUR_MS));

    let rows = store.activity_by_hour(from_bucket, to_bucket)?;
    let mut cells = Vec::with_capacity(rows.len());
    let mut peak = 0u64;
    for row in rows {
        let Some(at) = Utc.timestamp_millis_opt(row.hour_bucket * HOUR_MS).single() else {
            // A bucket that is not a representable instant is corrupt rather
            // than merely odd; skipping it beats rendering a cell at an
            // invented time.
            continue;
        };
        let local = at.with_timezone(&offset);
        peak = peak.max(row.total_tokens);
        cells.push(ActivityCell {
            date: local.format("%Y-%m-%d").to_string(),
            hour: local.hour(),
            total_tokens: row.total_tokens,
            sessions: row.sessions,
        });
    }

    Ok(ActivityHeatmap {
        schema_version: ACTIVITY_HEATMAP_SCHEMA_VERSION,
        utc_offset_seconds: offset.local_minus_utc(),
        peak_total_tokens: (peak > 0).then_some(peak),
        cells,
    })
}

// ---------------------------------------------------------------------------
// Display-currency conversion (issue #42)
// ---------------------------------------------------------------------------

/// One total restated in the user's display currency (issue #42).
///
/// Carries the rate, when it was taken, and where it came from, because a
/// converted figure without those is not a number anyone can check — and
/// Odometer never fetches a rate, so `source` is always the user's own
/// account of it.
#[derive(Debug, Clone, Serialize)]
pub struct ConvertedTotal {
    /// The original currency this was converted *from*. Kept so the
    /// original total stays the authoritative one and the conversion reads
    /// as a restatement rather than a replacement.
    pub from_currency: String,
    pub target_currency: String,
    pub amount: f64,
    pub rate: f64,
    pub as_of: DateTime<Utc>,
    pub source: String,
}

/// Restates whatever in `totals` can honestly be converted.
///
/// #42: "Currency conversion never combines credits with money or different
/// original currencies." Two rules follow, and both are refusals:
///
/// - Only totals already in the card's own `currency` are converted. The
///   card carries a single rate, defined as multiplying "an amount already
///   in the card's original currency", so applying it to anything else
///   would be arithmetic on unrelated units.
/// - Plan credits are never converted. Credits are an entitlement, not
///   money at an exchange rate, and turning them into euros would invent a
///   price the provider never charged.
///
/// Each converted amount stays keyed to the currency it came from, so
/// nothing is ever summed across originals.
pub fn convert_totals(rates: &RateCard, totals: &BTreeMap<String, f64>) -> Vec<ConvertedTotal> {
    let Some(conversion) = rates.display_currency.as_ref() else {
        return Vec::new();
    };
    totals
        .iter()
        .filter(|(currency, _)| {
            // Same currency in and out is not a conversion, it is noise.
            currency.as_str() == rates.currency
                && !currency.eq_ignore_ascii_case(&conversion.target_currency)
        })
        .filter_map(|(currency, amount)| {
            conversion.convert(*amount).map(|converted| ConvertedTotal {
                from_currency: currency.clone(),
                target_currency: conversion.target_currency.clone(),
                amount: converted,
                rate: conversion.rate,
                as_of: conversion.as_of,
                source: conversion.source.clone(),
            })
        })
        .collect()
}
