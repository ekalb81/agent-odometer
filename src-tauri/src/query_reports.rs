//! Headless reports assembled from the same durable facts and pricing helpers
//! as the desktop. No transcript discovery, network work, or pricing formulas.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::history_store::HistoryStore;
use crate::model::{
    CategoryMetric, OptimizationSummary, TaskCategory, TierBucket, TokenTotals,
    ToolDimensionMetrics, ToolMetrics,
};
use crate::provider::{ProviderId, ProviderRegistry};
use crate::query::{price_buckets_detailed_controlled, price_tokens, RangePricing, RateTable};
use crate::rates::{PricingBasis, RateCard};

pub const HEADLESS_REPORT_SCHEMA_VERSION: u32 = 1;

fn registered_provider(name: &str) -> Option<ProviderId> {
    let provider = ProviderId::new(name).ok()?;
    ProviderRegistry::builtin().adapter(&provider)?;
    Some(provider)
}

/// Durable keys are provider-namespaced. An absent, invalid, or unregistered
/// prefix cannot establish identity and must never imply Codex pricing.
pub fn provider_for_key(key: &str) -> Option<ProviderId> {
    let (prefix, suffix) = key.split_once(':')?;
    if suffix.is_empty() {
        return None;
    }
    registered_provider(prefix)
}

pub(crate) fn least_confident_basis(left: PricingBasis, right: PricingBasis) -> PricingBasis {
    fn rank(value: PricingBasis) -> u8 {
        match value {
            PricingBasis::Direct | PricingBasis::FreeLocal | PricingBasis::Subscription => 0,
            PricingBasis::Aliased | PricingBasis::FloatingAlias => 1,
            PricingBasis::Estimated => 2,
            PricingBasis::Fallback | PricingBasis::Stale => 3,
            PricingBasis::Unavailable => 4,
        }
    }
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

fn validate_window(from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>) -> Result<()> {
    if from.zip(to).is_some_and(|(from, to)| from > to) {
        bail!("from must not be later than to");
    }
    Ok(())
}

fn controlled_surfaces(
    store: &HistoryStore,
    buckets: &[TierBucket],
    harness: &str,
    rates: &RateCard,
    now: DateTime<Utc>,
) -> Result<RangePricing> {
    Ok(RangePricing {
        plan: price_buckets_detailed_controlled(
            rates,
            harness,
            buckets,
            RateTable::Plan,
            now,
            || store.check_query(),
        )?
        .expect("plan table always applies"),
        api: price_buckets_detailed_controlled(
            rates,
            harness,
            buckets,
            RateTable::Api,
            now,
            || store.check_query(),
        )?,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryUsage {
    /// Null preserves raw usage whose provider cannot be established.
    pub harness: Option<ProviderId>,
    pub category: TaskCategory,
    pub sessions: usize,
    pub turns: u64,
    pub tokens: TokenTotals,
    pub tool_calls: u64,
    pub pricing: Option<RangePricing>,
    pub currency: Option<String>,
    pub unattributed_tokens: TokenTotals,
    pub pricing_complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryReport {
    pub schema_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    /// Category totals are cumulative turn classifications, not event facts.
    pub scope: &'static str,
    pub sessions: usize,
    pub categories: Vec<CategoryUsage>,
}

#[derive(Default)]
struct CategoryAccumulator {
    sessions: usize,
    metric: CategoryMetric,
    buckets: BTreeMap<(String, Option<String>), TokenTotals>,
}

pub fn category_report(
    store: &HistoryStore,
    rates: &RateCard,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<CategoryReport> {
    validate_window(from, to)?;
    let mut grouped: BTreeMap<(Option<ProviderId>, TaskCategory), CategoryAccumulator> =
        BTreeMap::new();
    let mut sessions = 0;
    let mut bucket_rows = 0;
    store.stream_category_snapshots(|snapshot| {
        store.check_query()?;
        if from.is_some_and(|bound| snapshot.last_event_at < bound)
            || to.is_some_and(|bound| snapshot.started_at > bound)
        {
            return Ok(());
        }
        sessions += 1;
        let provider = snapshot
            .harness
            .as_ref()
            .and_then(|id| registered_provider(id.as_str()));
        for (category, metric) in snapshot.category_totals {
            let entry = grouped.entry((provider.clone(), category)).or_default();
            entry.sessions += 1;
            entry.metric.turns += metric.turns;
            entry.metric.tokens += &metric.tokens;
            entry.metric.tool_calls += metric.tool_calls;
            for bucket in metric.buckets {
                let key = (bucket.model, bucket.service_tier);
                bucket_rows += usize::from(!entry.buckets.contains_key(&key));
                *entry.buckets.entry(key).or_default() += &bucket.tokens;
                store.check_query_rows(bucket_rows)?;
            }
        }
        store.check_query_rows(grouped.len() + bucket_rows)?;
        Ok(())
    })?;
    let mut categories = Vec::with_capacity(grouped.len());
    for ((harness, category), accumulated) in grouped {
        store.check_query()?;
        let CategoryAccumulator {
            sessions,
            metric,
            buckets,
        } = accumulated;
        let buckets: Vec<_> = buckets
            .into_iter()
            .map(|((model, service_tier), tokens)| TierBucket {
                model,
                service_tier,
                tokens,
            })
            .collect();
        let pricing = harness
            .as_ref()
            .map(|provider| controlled_surfaces(store, &buckets, provider.as_str(), rates, now))
            .transpose()?;
        let currency = harness.as_ref().map(|provider| currency(rates, provider));
        let (unattributed_tokens, pricing_complete) =
            pricing_coverage(store, &metric.tokens, &buckets, pricing.as_ref())?;
        categories.push(CategoryUsage {
            harness,
            category,
            sessions,
            turns: metric.turns,
            tokens: metric.tokens,
            tool_calls: metric.tool_calls,
            pricing,
            currency,
            unattributed_tokens,
            pricing_complete,
        });
    }
    Ok(CategoryReport {
        schema_version: HEADLESS_REPORT_SCHEMA_VERSION,
        from,
        to,
        scope: "whole_sessions_overlapping_window",
        sessions,
        categories,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct DimensionUsage {
    pub available: bool,
    pub reason: Option<&'static str>,
    pub values: BTreeMap<String, ToolDimensionMetrics>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderToolUsage {
    pub harness: Option<ProviderId>,
    pub sessions: usize,
    pub metrics: ToolMetrics,
    pub by_model: BTreeMap<String, ToolMetrics>,
    pub dimensions: BTreeMap<String, DimensionUsage>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolsReport {
    pub schema_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub providers: Vec<ProviderToolUsage>,
}

fn dimension_available(provider: Option<&ProviderId>, dimension: &str) -> bool {
    let Some(adapter) = provider.and_then(|id| ProviderRegistry::builtin().adapter(id)) else {
        return false;
    };
    let capabilities = adapter.descriptor().capabilities;
    match dimension {
        "mcp_server" => capabilities.mcp_dimension,
        "shell_family" => capabilities.shell_dimension,
        "language" => capabilities.language_dimension,
        "context_source" => capabilities.context_dimension,
        _ => false,
    }
}

pub fn tools_report(
    store: &HistoryStore,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<ToolsReport> {
    validate_window(from, to)?;
    let keys = store.session_keys()?;
    let ranges = store
        .range_totals_multi(&keys, &[(from, to)])?
        .pop()
        .unwrap_or_default();
    let mut providers: BTreeMap<Option<ProviderId>, ProviderToolUsage> = BTreeMap::new();
    for (key, range) in ranges {
        store.check_query()?;
        let harness = provider_for_key(&key);
        let row = providers
            .entry(harness.clone())
            .or_insert_with(|| ProviderToolUsage {
                harness: harness.clone(),
                sessions: 0,
                metrics: ToolMetrics::default(),
                by_model: BTreeMap::new(),
                dimensions: ["mcp_server", "shell_family", "language", "context_source"]
                    .into_iter()
                    .map(|kind| {
                        let available = dimension_available(harness.as_ref(), kind);
                        (
                            kind.to_owned(),
                            DimensionUsage {
                                available,
                                reason: (!available).then_some("provider_dimension_unavailable"),
                                values: BTreeMap::new(),
                            },
                        )
                    })
                    .collect(),
            });
        row.sessions += 1;
        row.metrics.add_assign(&range.tool_metrics);
        for (model, metrics) in range.tool_metrics_by_model {
            row.by_model.entry(model).or_default().add_assign(&metrics);
        }
        for (kind, values) in range.tool_dimensions {
            let dimension = row
                .dimensions
                .entry(kind)
                .or_insert_with(|| DimensionUsage {
                    available: false,
                    reason: Some("dimension_unavailable"),
                    values: BTreeMap::new(),
                });
            // Retain any observed raw values even when capability is not
            // corroborated; availability remains explicit alongside them.
            for (name, metrics) in values {
                dimension
                    .values
                    .entry(name)
                    .or_default()
                    .add_assign(&metrics);
            }
        }
        store.check_query_rows(
            providers
                .values()
                .map(|row| {
                    row.by_model.len()
                        + row
                            .dimensions
                            .values()
                            .map(|dimension| dimension.values.len())
                            .sum::<usize>()
                })
                .sum(),
        )?;
    }
    Ok(ToolsReport {
        schema_version: HEADLESS_REPORT_SCHEMA_VERSION,
        from,
        to,
        providers: providers.into_values().collect(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderContextUsage {
    pub harness: Option<ProviderId>,
    pub sessions: usize,
    pub context: DimensionUsage,
}

#[derive(Debug, Clone, Serialize)]
pub struct ContextReport {
    pub schema_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub providers: Vec<ProviderContextUsage>,
}

pub fn context_report(
    store: &HistoryStore,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<ContextReport> {
    let tools = tools_report(store, from, to)?;
    Ok(ContextReport {
        schema_version: HEADLESS_REPORT_SCHEMA_VERSION,
        from,
        to,
        providers: tools
            .providers
            .into_iter()
            .map(|mut row| ProviderContextUsage {
                harness: row.harness,
                sessions: row.sessions,
                context: row
                    .dimensions
                    .remove("context_source")
                    .expect("standard dimension"),
            })
            .collect(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderFindings {
    pub harness: Option<ProviderId>,
    pub sessions: usize,
    pub summary: OptimizationSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct FindingsReport {
    pub schema_version: u32,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub scope: &'static str,
    pub summary: OptimizationSummary,
    pub providers: Vec<ProviderFindings>,
}

fn add_findings(into: &mut OptimizationSummary, value: &OptimizationSummary) {
    into.findings += value.findings;
    into.warnings += value.warnings;
    into.likely_avoidable_calls += value.likely_avoidable_calls;
    for (rule, count) in &value.by_rule {
        *into.by_rule.entry(rule.clone()).or_default() += count;
    }
}

pub fn findings_report(
    store: &HistoryStore,
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
) -> Result<FindingsReport> {
    validate_window(from, to)?;
    let keys = store.session_keys()?;
    let ranges = store
        .range_totals_multi(&keys, &[(from, to)])?
        .pop()
        .unwrap_or_default();
    let mut providers: BTreeMap<Option<ProviderId>, ProviderFindings> = BTreeMap::new();
    let mut summary = OptimizationSummary::default();
    for (key, range) in ranges {
        store.check_query()?;
        let harness = provider_for_key(&key);
        let row = providers
            .entry(harness.clone())
            .or_insert_with(|| ProviderFindings {
                harness,
                sessions: 0,
                summary: OptimizationSummary::default(),
            });
        row.sessions += 1;
        add_findings(&mut row.summary, &range.optimization_summary);
        add_findings(&mut summary, &range.optimization_summary);
        store.check_query_rows(summary.by_rule.len())?;
    }
    Ok(FindingsReport {
        schema_version: HEADLESS_REPORT_SCHEMA_VERSION,
        from,
        to,
        scope: if from.is_none() && to.is_none() {
            "all_findings_including_undated"
        } else {
            "timestamped_findings_in_window"
        },
        summary,
        providers: providers.into_values().collect(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticSource {
    pub kind: &'static str,
    pub path: Option<String>,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticLedger {
    pub durable_sessions: u64,
    pub available_sessions: u64,
    pub collision_sessions: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiagnosticModel {
    pub model: String,
    pub basis: PricingBasis,
    pub resolved_model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeadlessProviderDiagnostic {
    pub provider: String,
    pub registered: bool,
    pub roots: Vec<DiagnosticSource>,
    pub ledger: Option<DiagnosticLedger>,
    pub models: Vec<DiagnosticModel>,
    pub quota_status: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct HeadlessDiagnosticsReport {
    pub schema_version: u32,
    pub generated_at: DateTime<Utc>,
    pub source_configuration_valid: bool,
    pub ledger_available: bool,
    pub scan_status: &'static str,
    pub cache_status: &'static str,
    pub rates_fetched_at: Option<String>,
    pub providers: Vec<HeadlessProviderDiagnostic>,
}

/// Reports only facts available to a read-only headless process. In-memory
/// scan/cache state and live quota are unavailable, not fabricated successes.
pub fn diagnostics_report(
    store: Option<&HistoryStore>,
    config: &Config,
    rates: &RateCard,
    now: DateTime<Utc>,
    include_paths: bool,
) -> Result<HeadlessDiagnosticsReport> {
    let config = config.clone().normalized();
    let stats = store
        .map(HistoryStore::provider_stats)
        .transpose()?
        .unwrap_or_default();
    let mut stats: BTreeMap<_, _> = stats
        .into_iter()
        .map(|row| (row.provider.clone(), row))
        .collect();
    let mut ids: BTreeSet<String> = ProviderRegistry::builtin()
        .descriptors()
        .map(|row| row.id.as_str().to_owned())
        .collect();
    ids.extend(config.providers.keys().map(|id| id.as_str().to_owned()));
    ids.extend(stats.keys().cloned());
    let mut providers = Vec::new();
    for name in ids {
        if let Some(store) = store {
            store.check_query()?;
        }
        let provider = registered_provider(&name);
        let mut roots = Vec::new();
        if let Some(source) = config.providers.get(name.as_str()) {
            for (kind, paths) in [
                ("live", &source.live_roots),
                ("archive", &source.archive_roots),
            ] {
                roots.extend(paths.iter().map(|path| DiagnosticSource {
                    kind,
                    path: include_paths.then(|| path.to_string_lossy().into_owned()),
                    exists: path.exists(),
                }));
            }
            if let Some(path) = &source.session_index_path {
                roots.push(DiagnosticSource {
                    kind: "session_index",
                    path: include_paths.then(|| path.to_string_lossy().into_owned()),
                    exists: path.exists(),
                });
            }
        }
        let row = stats.remove(&name);
        let models = row
            .as_ref()
            .map(|row| {
                row.models
                    .iter()
                    .map(|model| {
                        let priced = provider.as_ref().map(|provider| {
                            price_tokens(
                                rates,
                                provider.as_str(),
                                model,
                                None,
                                &TokenTotals::default(),
                                RateTable::Plan,
                                now,
                            )
                        });
                        DiagnosticModel {
                            model: model.clone(),
                            basis: priced
                                .as_ref()
                                .map_or(PricingBasis::Unavailable, |value| value.basis),
                            resolved_model: priced.map(|value| value.resolved_model),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let ledger = store.map(|_| DiagnosticLedger {
            durable_sessions: row.as_ref().map_or(0, |row| row.durable_sessions),
            available_sessions: row.as_ref().map_or(0, |row| row.available_sessions),
            collision_sessions: row.as_ref().map_or(0, |row| row.collision_sessions),
        });
        providers.push(HeadlessProviderDiagnostic {
            provider: name,
            registered: provider.is_some(),
            roots,
            ledger,
            models,
            quota_status: "not_queried_use_quota_report",
        });
        if let Some(store) = store {
            store.check_query_rows(
                providers
                    .iter()
                    .map(|row| 1 + row.models.len() + row.roots.len())
                    .sum(),
            )?;
        }
    }
    Ok(HeadlessDiagnosticsReport {
        schema_version: HEADLESS_REPORT_SCHEMA_VERSION,
        generated_at: now,
        source_configuration_valid: config.provider_sources().is_ok(),
        ledger_available: store.is_some(),
        scan_status: "unavailable_in_headless_query",
        cache_status: "unavailable_in_headless_query",
        rates_fetched_at: rates.fetched_at.clone(),
        providers,
    })
}

fn currency(rates: &RateCard, provider: &ProviderId) -> String {
    rates
        .currencies
        .get(provider.as_str())
        .cloned()
        .unwrap_or_else(|| rates.currency.clone())
}

#[derive(Debug, Clone, Serialize)]
pub struct StatuslineProvider {
    pub harness: Option<ProviderId>,
    pub tokens: TokenTotals,
    pub pricing: Option<RangePricing>,
    pub currency: Option<String>,
    /// Usage without an attributable model is retained in the token totals,
    /// but cannot silently appear as a complete priced zero.
    pub unattributed_tokens: TokenTotals,
    pub pricing_complete: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct StatuslineReport {
    pub schema_version: u32,
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub tokens: TokenTotals,
    pub cost_by_currency: BTreeMap<String, f64>,
    pub providers: Vec<StatuslineProvider>,
    pub pricing_complete: bool,
}

fn pricing_coverage(
    store: &HistoryStore,
    tokens: &TokenTotals,
    buckets: &[TierBucket],
    pricing: Option<&RangePricing>,
) -> Result<(TokenTotals, bool)> {
    let mut attributed = TokenTotals::default();
    let priced_models: BTreeSet<&str> = pricing
        .into_iter()
        .flat_map(|pricing| pricing.plan.by_model.iter())
        .filter(|model| !model.unpriced && model.basis != PricingBasis::Unavailable)
        .map(|model| model.model.as_str())
        .collect();
    let mut all_models_priced = pricing.is_some();
    for bucket in buckets {
        store.check_query()?;
        attributed += &bucket.tokens;
        all_models_priced &= priced_models.contains(bucket.model.as_str());
    }
    let unattributed = TokenTotals {
        input_tokens: tokens.input_tokens.saturating_sub(attributed.input_tokens),
        cached_input_tokens: tokens
            .cached_input_tokens
            .saturating_sub(attributed.cached_input_tokens),
        cache_creation_input_tokens: tokens
            .cache_creation_input_tokens
            .saturating_sub(attributed.cache_creation_input_tokens),
        output_tokens: tokens
            .output_tokens
            .saturating_sub(attributed.output_tokens),
        reasoning_output_tokens: tokens
            .reasoning_output_tokens
            .saturating_sub(attributed.reasoning_output_tokens),
        total_tokens: tokens.total_tokens.saturating_sub(attributed.total_tokens),
    };
    let complete = unattributed == TokenTotals::default() && all_models_priced;
    Ok((unattributed, complete))
}

/// Today's counters use one token-only aggregate ledger read, independent
/// of session count or snapshot size. Caller supplies its local UTC offset.
pub fn statusline_report(
    store: &HistoryStore,
    rates: &RateCard,
    now: DateTime<Utc>,
    offset: FixedOffset,
) -> Result<StatuslineReport> {
    let midnight = now
        .with_timezone(&offset)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight exists");
    let from = offset
        .from_local_datetime(&midnight)
        .single()
        .expect("fixed offset")
        .with_timezone(&Utc);
    let totals = store.token_totals_by_provider(Some(from), Some(now))?;
    let mut tokens = TokenTotals::default();
    let mut cost_by_currency = BTreeMap::new();
    let mut providers = Vec::new();
    for (name, range) in totals {
        store.check_query()?;
        tokens += &range.tokens;
        let harness = registered_provider(&name);
        let pricing = harness
            .as_ref()
            .map(|id| controlled_surfaces(store, &range.buckets, id.as_str(), rates, now))
            .transpose()?;
        let currency = harness.as_ref().map(|id| currency(rates, id));
        let (unattributed_tokens, pricing_complete) =
            pricing_coverage(store, &range.tokens, &range.buckets, pricing.as_ref())?;
        if let (Some(pricing), Some(currency)) = (&pricing, &currency) {
            *cost_by_currency.entry(currency.clone()).or_default() += pricing.plan.total;
        }
        providers.push(StatuslineProvider {
            harness,
            tokens: range.tokens,
            pricing,
            currency,
            unattributed_tokens,
            pricing_complete,
        });
        store.check_query_rows(
            providers
                .iter()
                .map(|row| {
                    1 + row
                        .pricing
                        .as_ref()
                        .map_or(0, |pricing| pricing.plan.by_model.len())
                })
                .sum(),
        )?;
    }
    let pricing_complete = providers.iter().all(|row| row.pricing_complete);
    Ok(StatuslineReport {
        schema_version: HEADLESS_REPORT_SCHEMA_VERSION,
        from,
        to: now,
        tokens,
        cost_by_currency,
        providers,
        pricing_complete,
    })
}
