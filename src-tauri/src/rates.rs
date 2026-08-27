use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelRate {
    pub input: f64,
    pub cached_input: f64,
    /// Cache-creation ("cache write") rate — a normalized dimension distinct
    /// from both `input` and `cached_input`. Historically Claude cache
    /// writes were folded into `input`, which missed Anthropic's ~1.25x
    /// write premium.
    ///
    /// Deliberately nullable, and the null/absent case is not "free": it
    /// means the publisher has not stated a cache-write premium for this
    /// model, and cache-creation tokens must be priced at the ordinary
    /// `input` rate (exactly today's pre-#42 accounting) rather than at
    /// zero. `Some(0.0)` is a real, deliberate "this is free" claim,
    /// distinct from "unknown". See `cache_creation_rate()` — every pricing
    /// call site must resolve through it rather than reading this field
    /// directly, or the absent/free distinction gets silently lost again.
    /// Absent for rate-card entries written before this field existed;
    /// `merge_older_override` backfills it from the bundled card for models
    /// a user has otherwise customized (see there).
    #[serde(default)]
    pub cache_creation_input: Option<f64>,
    pub output: f64,
    /// Typically the same as output for reasoning models.
    pub reasoning: f64,
}

impl ModelRate {
    /// Whether every dimension of this rate is a number that can be priced
    /// with: finite and non-negative (issue #42).
    ///
    /// Parsing is not validation. `-5.0` and `NaN` are both valid JSON
    /// numbers, so a corrupt or hostile card deserializes cleanly and then
    /// produces negative costs or — worse — NaN totals that propagate
    /// silently through every sum downstream.
    pub fn is_usable(&self) -> bool {
        let dimensions = [
            Some(self.input),
            Some(self.cached_input),
            Some(self.output),
            Some(self.reasoning),
            self.cache_creation_input,
        ];
        dimensions
            .into_iter()
            .flatten()
            .all(|value| value.is_finite() && value >= 0.0)
    }

    /// Resolves the effective cache-creation rate: the published premium
    /// when stated, otherwise the ordinary `input` rate. Never zero by
    /// default — see `cache_creation_input`'s docs. Every cache-creation
    /// pricing call site must go through this rather than reading
    /// `cache_creation_input` directly.
    pub fn cache_creation_rate(&self) -> f64 {
        self.cache_creation_input.unwrap_or(self.input)
    }

    /// True when `cache_creation_rate()` is falling back to the ordinary
    /// input rate rather than pricing at a directly published cache-write
    /// premium. Callers use this to keep a fallback cache-write charge from
    /// rendering as an authoritative direct price.
    pub fn cache_creation_rate_is_fallback(&self) -> bool {
        self.cache_creation_input.is_none()
    }
}

/// The billing surface a catalog rule applies to.  A model can carry different
/// rules for a subscription plan and for a provider's API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingSurface {
    CodexPlanCredits,
    OpenaiApiUsd,
    AnthropicApiUsd,
}

/// Source information retained with each catalog rule so scenario estimates do
/// not imply a billing policy that has not been published for that surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingProvenance {
    pub evidence: String,
    pub source_url: String,
    pub verified_at: DateTime<Utc>,
    #[serde(default)]
    pub note: Option<String>,
}

/// An effective-dated base rate. `to: None` denotes an open-ended interval;
/// otherwise periods use the half-open interval `[from, to)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectiveRatePeriod {
    /// Stable catalog identity used to reconcile rate rules across updates.
    /// Missing IDs deserialize for old user overrides but do not validate.
    #[serde(default)]
    pub id: String,
    pub surface: PricingSurface,
    pub model: String,
    pub from: DateTime<Utc>,
    #[serde(default)]
    pub to: Option<DateTime<Utc>>,
    pub rate: ModelRate,
    /// Cache-write multiplier over uncached input. Parsers do not currently
    /// expose cache-write tokens, so this is catalog metadata only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_input_multiplier: Option<f64>,
    pub provenance: PricingProvenance,
    pub label: String,
}

/// A condition evaluated against a single provider request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PricingCondition {
    /// Applies only when the request's complete input exceeds this count.
    RequestInputTokenThreshold { greater_than: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RateMultipliers {
    pub input: f64,
    pub output: f64,
}

/// An effective-dated conditional pricing rule.  Cache-write token categories
/// are deliberately not represented by `RateMultipliers`: parsers currently
/// retain cache reads but do not distinguish cache writes from uncached input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionalRateModifier {
    /// Stable catalog identity used to reconcile conditional rules across updates.
    /// Missing IDs deserialize for old user overrides but do not validate.
    #[serde(default)]
    pub id: String,
    pub surface: PricingSurface,
    pub model: String,
    pub from: DateTime<Utc>,
    #[serde(default)]
    pub to: Option<DateTime<Utc>>,
    pub condition: PricingCondition,
    pub multipliers: RateMultipliers,
    pub provenance: PricingProvenance,
    pub label: String,
}

/// Versioned, auditable scenario pricing data.  The legacy `models` maps stay
/// authoritative for existing views and user overrides; this catalog adds
/// time/surface-aware alternatives without changing those calculations.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PricingCatalog {
    #[serde(default)]
    pub rate_periods: Vec<EffectiveRatePeriod>,
    #[serde(default)]
    pub conditional_modifiers: Vec<ConditionalRateModifier>,
    #[serde(default)]
    pub notes: Vec<String>,
}

impl PricingCatalog {
    pub fn is_empty(&self) -> bool {
        self.rate_periods.is_empty()
            && self.conditional_modifiers.is_empty()
            && self.notes.is_empty()
    }

    /// Selects the base rate whose half-open effective interval contains `at`.
    pub fn rate_at(
        &self,
        surface: PricingSurface,
        model: &str,
        at: DateTime<Utc>,
    ) -> Option<&EffectiveRatePeriod> {
        self.rate_periods.iter().find(|period| {
            period.surface == surface
                && period.model == model
                && interval_contains(period.from, period.to, at)
        })
    }

    /// Returns every modifier whose interval and request condition apply.
    pub fn modifiers_for_request(
        &self,
        surface: PricingSurface,
        model: &str,
        at: DateTime<Utc>,
        request_input_tokens: u64,
    ) -> Vec<&ConditionalRateModifier> {
        self.conditional_modifiers
            .iter()
            .filter(|modifier| {
                modifier.surface == surface
                    && modifier.model == model
                    && interval_contains(modifier.from, modifier.to, at)
                    && matches!(
                        modifier.condition,
                        PricingCondition::RequestInputTokenThreshold { greater_than }
                            if request_input_tokens > greater_than
                    )
            })
            .collect()
    }

    /// Ensures catalog intervals are well-formed and that base-rate periods do
    /// not overlap for one `(surface, model)` pair.
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut ids = std::collections::HashSet::new();
        for period in &self.rate_periods {
            validate_rule_id(&period.id, &mut ids)?;
            validate_rule_scope(
                &period.model,
                period.from,
                period.to,
                &period.label,
                &period.provenance,
            )?;
            if period
                .cache_write_input_multiplier
                .is_some_and(|multiplier| multiplier <= 0.0)
            {
                anyhow::bail!("cache-write input multiplier must be greater than zero");
            }
        }
        for modifier in &self.conditional_modifiers {
            validate_rule_id(&modifier.id, &mut ids)?;
            validate_rule_scope(
                &modifier.model,
                modifier.from,
                modifier.to,
                &modifier.label,
                &modifier.provenance,
            )?;
            match modifier.condition {
                PricingCondition::RequestInputTokenThreshold { greater_than }
                    if greater_than > 0 => {}
                PricingCondition::RequestInputTokenThreshold { .. } => {
                    anyhow::bail!("request input token threshold must be greater than zero")
                }
            }
            if modifier.multipliers.input <= 0.0 || modifier.multipliers.output <= 0.0 {
                anyhow::bail!("pricing modifier multipliers must be greater than zero");
            }
        }

        let mut grouped: HashMap<(PricingSurface, &str), Vec<&EffectiveRatePeriod>> =
            HashMap::new();
        for period in &self.rate_periods {
            grouped
                .entry((period.surface, period.model.as_str()))
                .or_default()
                .push(period);
        }
        for ((surface, model), periods) in &mut grouped {
            periods.sort_by_key(|period| period.from);
            for pair in periods.windows(2) {
                let current = pair[0];
                let next = pair[1];
                if intervals_overlap(current.from, current.to, next.from, next.to) {
                    anyhow::bail!(
                        "overlapping pricing periods for {surface:?}/{model}: {} and {}",
                        current.label,
                        next.label
                    );
                }
            }
        }
        for (index, modifier) in self.conditional_modifiers.iter().enumerate() {
            for other in self.conditional_modifiers.iter().skip(index + 1) {
                if modifier.surface == other.surface
                    && modifier.model == other.model
                    && modifier.condition == other.condition
                    && intervals_overlap(modifier.from, modifier.to, other.from, other.to)
                {
                    anyhow::bail!(
                        "overlapping conditional pricing modifiers for {:?}/{}: {} and {}",
                        modifier.surface,
                        modifier.model,
                        modifier.label,
                        other.label
                    );
                }
            }
        }
        Ok(())
    }
}

fn validate_rule_id(id: &str, ids: &mut std::collections::HashSet<String>) -> anyhow::Result<()> {
    if id.trim().is_empty() {
        anyhow::bail!("pricing catalog rules require a non-empty ID");
    }
    if !ids.insert(id.to_owned()) {
        anyhow::bail!("duplicate pricing catalog rule ID: {id}");
    }
    Ok(())
}

fn interval_contains(from: DateTime<Utc>, to: Option<DateTime<Utc>>, at: DateTime<Utc>) -> bool {
    at >= from && to.is_none_or(|end| at < end)
}

fn intervals_overlap(
    first_from: DateTime<Utc>,
    first_to: Option<DateTime<Utc>>,
    second_from: DateTime<Utc>,
    second_to: Option<DateTime<Utc>>,
) -> bool {
    first_to.is_none_or(|end| second_from < end) && second_to.is_none_or(|end| first_from < end)
}

fn validate_rule_scope(
    model: &str,
    from: DateTime<Utc>,
    to: Option<DateTime<Utc>>,
    label: &str,
    provenance: &PricingProvenance,
) -> anyhow::Result<()> {
    if model.trim().is_empty() || label.trim().is_empty() {
        anyhow::bail!("pricing catalog rules require a model and label");
    }
    if provenance.evidence.trim().is_empty() || provenance.source_url.trim().is_empty() {
        anyhow::bail!("pricing catalog rules require evidence and a source URL");
    }
    if to.is_some_and(|end| end <= from) {
        anyhow::bail!("pricing catalog interval end must be after its start");
    }
    Ok(())
}

/// Provenance recorded for every priced amount, so the UI can render direct,
/// approximate, and unpriced results as visually and structurally distinct
/// states rather than collapsing them into one number (issue #42).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingBasis {
    /// The exact requested model id had a rate in the resolved table.
    Direct,
    /// The requested model id resolved to a different key via `model_aliases`
    /// before a rate was found.
    Aliased,
    /// The requested model id resolved via `floating_model_aliases` — a
    /// mapping the provider itself documents as temporary and repoints as
    /// new models ship (issue #177).
    ///
    /// Correct as of the card's fetch, and priced from a real published rate
    /// for the model the alias pointed at *then*. Distinct from `Aliased`
    /// because it carries a known expiry: past `FloatingAlias::expires_at`
    /// the mapping is not trusted and resolution falls through to
    /// `Fallback`, which is what raises the UI's warning. Surface it as a
    /// soft note, not a warning — the price is right today.
    FloatingAlias,
    /// Neither the model nor an alias resolved; the harness's (or card's)
    /// configured fallback model rate was used instead.
    Fallback,
    /// A rate was found but is marked as a non-authoritative estimate (not
    /// produced by this build; reserved for a future estimation source, e.g.
    /// interpolating an unpublished tier from a published one).
    Estimated,
    /// The model is explicitly configured as free or locally hosted — zero
    /// cost by design, distinct from `Unavailable` (no pricing information
    /// at all). See `RateCard::free_local_models`.
    FreeLocal,
    /// Priced against a user-declared subscription/custom plan rather than a
    /// metered per-token rate. See `RateCard::subscription_plans`.
    Subscription,
    /// A rate was found, but the card's refresh bookkeeping considers it
    /// older than `RateRefreshState::max_cache_age_secs`.
    Stale,
    /// No rate could be resolved for this model by any path, including the
    /// fallback. Distinct from `unpriced_models`, whose members are known
    /// models with a confirmed absence of published pricing.
    Unavailable,
}

/// The resolved pricing-table key and provenance for one raw model id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PricedModelResolution {
    /// The table key actually used to find a rate — equal to the requested
    /// model for `Direct`/`FreeLocal`/`Unavailable`, the alias target for
    /// `Aliased`, or the configured fallback model for `Fallback`.
    pub resolved_model: String,
    pub basis: PricingBasis,
}

/// Resolves a raw model id through `model_aliases` before any fallback path.
/// Alias chains may be several hops deep, but a chain can never take more
/// steps than the table has entries — if it does, a cycle exists. Cycle
/// detection tracks visited keys and stops instead of looping forever,
/// returning the last resolved key so the caller can still attempt a plain
/// lookup (which will typically fail, correctly falling through to
/// `Fallback`/`Unavailable`).
fn resolve_alias<'a>(model: &'a str, aliases: &'a HashMap<String, String>) -> (&'a str, bool) {
    let mut current = model;
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut hopped = false;
    for _ in 0..=aliases.len() {
        let Some(next) = aliases.get(current) else {
            break;
        };
        if !seen.insert(current) {
            break; // Cycle: `current` was already visited this resolution.
        }
        current = next.as_str();
        hopped = true;
    }
    (current, hopped)
}

/// A user-declared subscription or custom plan for one harness. Odometer
/// records exactly what the user tells it here — it never infers or claims a
/// plan-equivalent token allowance, because no harness publishes one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionPlan {
    pub name: String,
    #[serde(default)]
    pub monthly_price: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    /// Free-text notes, e.g. plan tier or seat count.
    #[serde(default)]
    pub notes: Option<String>,
    /// User-declared estimated monthly savings versus a metered API-equivalent
    /// cost, from running a local model or a caching proxy instead of paying
    /// per token. This is a value the user supplies or measures themselves,
    /// never a figure Odometer derives from token counts.
    #[serde(default)]
    pub local_baseline_savings: Option<f64>,
}

/// A user-supplied display-currency conversion. Odometer performs no FX
/// fetch: `rate` and `as_of` are exactly what the user entered (or absent).
/// The original-currency amount and the `RateCard.currency`/harness currency
/// it was computed in are always retained separately so a converted total
/// never silently replaces or is combined with money in a different currency
/// or with harness credits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CurrencyConversion {
    /// ISO 4217 code to convert into, e.g. "EUR".
    pub target_currency: String,
    /// Multiply an amount already in the card's original currency by this to
    /// get `target_currency`.
    pub rate: f64,
    pub as_of: DateTime<Utc>,
    /// Free-text provenance for the rate, e.g. "user-entered" or a cited
    /// source name. Never a claim that Odometer fetched it.
    pub source: String,
}

impl CurrencyConversion {
    /// Converts one original-currency amount. Returns `None` for a
    /// non-finite or non-positive rate rather than producing a nonsensical
    /// total.
    pub fn convert(&self, amount_in_original_currency: f64) -> Option<f64> {
        if !self.rate.is_finite() || self.rate <= 0.0 {
            return None;
        }
        Some(amount_in_original_currency * self.rate)
    }
}

/// Coarse freshness classification for `RateRefreshState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RateFreshness {
    /// A refresh has succeeded within `max_cache_age_secs`.
    Fresh,
    /// The last successful refresh is older than `max_cache_age_secs`.
    Stale,
    /// No successful refresh has ever been recorded (e.g. a fresh bundled
    /// card that has never gone through `apply_refresh_candidate`).
    Unknown,
}

fn default_max_cache_age_secs() -> i64 {
    7 * 24 * 60 * 60 // one week
}

/// Bounded-cache-age bookkeeping for the price-refresh/rollback flow. This is
/// local bookkeeping about *when this build last validated a candidate card*,
/// deliberately separate from `RateCard.fetched_at`, which is
/// catalog-supplied provenance for the price data itself.
///
/// SEAM (deliberately unimplemented in this PR — see the PR description for
/// #42's scope): nothing in this codebase currently produces a
/// `RateRefreshState` from a network or updater-channel fetch. `AGENTS.md`
/// forbids adding outbound network access without an explicit requirement
/// and a security review, and no such review has happened yet. A future,
/// reviewed price source would call `apply_refresh_candidate` after fetching
/// and deserializing a candidate `RateCard`, exactly as the offline test
/// coverage in this module exercises.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RateRefreshState {
    #[serde(default)]
    pub last_success_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_attempt_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_failure_reason: Option<String>,
    /// How long a successful refresh remains authoritative before
    /// `freshness` reports `Stale`. Purely informational: nothing in this
    /// build automatically re-fetches when a card goes stale.
    #[serde(default = "default_max_cache_age_secs")]
    pub max_cache_age_secs: i64,
}

impl Default for RateRefreshState {
    /// A hand-written `Default` (rather than `#[derive(Default)]`) so both
    /// this and a `refresh` field entirely missing from an older on-disk
    /// card converge on the same one-week bound instead of the derive's
    /// `i64::default()` of zero, which would make every dated card look
    /// immediately stale.
    fn default() -> Self {
        Self {
            last_success_at: None,
            last_attempt_at: None,
            last_failure_reason: None,
            max_cache_age_secs: default_max_cache_age_secs(),
        }
    }
}

impl RateRefreshState {
    pub fn freshness(&self, now: DateTime<Utc>) -> RateFreshness {
        match self.last_success_at {
            None => RateFreshness::Unknown,
            Some(at) => {
                let age_secs = now.signed_duration_since(at).num_seconds();
                if age_secs <= self.max_cache_age_secs {
                    RateFreshness::Fresh
                } else {
                    RateFreshness::Stale
                }
            }
        }
    }
}

/// Validates a refresh candidate and fails closed to `previous` (the last
/// valid or bundled card) on any structural or partial-data problem, rather
/// than ever partially applying an invalid candidate. See the SEAM note on
/// `RateRefreshState` for why nothing calls this with a network-fetched
/// candidate yet — today's only caller is the offline test suite and, when a
/// reviewed transport exists, that transport.
pub fn apply_refresh_candidate(
    previous: &RateCard,
    candidate: RateCard,
    now: DateTime<Utc>,
) -> RateCard {
    let mut refreshed = candidate;
    let structurally_valid = refreshed.pricing_catalog.validate().is_ok();
    // A candidate that "successfully" parses to an empty or truncated price
    // table is not a valid refresh — it would silently zero out coverage the
    // previous card had. Refuse it exactly like a validation failure.
    let dropped_models = !previous.models.is_empty() && refreshed.models.is_empty();
    let dropped_api_models = !previous.api_models.is_empty() && refreshed.api_models.is_empty();
    // Every rate must be a finite, non-negative number (issue #42's
    // "invalid/partial remote price data fails closed"). Parsing is not
    // validation: `-5.0` and `NaN` are both valid JSON numbers, and both
    // used to be accepted here.
    //
    // NaN is the dangerous one. It propagates silently through every sum
    // that touches it, so a single poisoned rate turns whole-corpus totals
    // into NaN with no error anywhere — and unlike a wrong number, it cannot
    // even be spotted as implausible in a table.
    let unusable_rate = refreshed
        .models
        .values()
        .chain(refreshed.api_models.values())
        .any(|rate| !rate.is_usable());
    if structurally_valid && !dropped_models && !dropped_api_models && !unusable_rate {
        refreshed.refresh.last_success_at = Some(now);
        refreshed.refresh.last_attempt_at = Some(now);
        refreshed.refresh.last_failure_reason = None;
        refreshed
    } else {
        let mut rolled_back = previous.clone();
        rolled_back.refresh.last_attempt_at = Some(now);
        rolled_back.refresh.last_failure_reason = Some(if !structurally_valid {
            "candidate pricing catalog failed validation".to_owned()
        } else if unusable_rate {
            "candidate contained a negative or non-finite rate".to_owned()
        } else {
            "candidate price tables were empty or partial".to_owned()
        });
        rolled_back
    }
}

/// A provider-declared *floating* model alias: a name that resolves to
/// whichever model the provider currently considers current, and which the
/// provider repoints without renaming (issue #177).
///
/// OpenAI's `daybreak-blue-latest` / `daybreak-red-latest` are the motivating
/// case — documented as pointing at a specific model "currently", repointed
/// as new frontier models ship, with pricing following the underlying model.
///
/// A static entry in `model_aliases` would be right on the day it is written
/// and then silently wrong in the worst direction: `Aliased` provenance
/// raises no warning, so the app would price a newer model at an older
/// model's rate while presenting it as an exact match. Recording the expiry
/// makes that failure loud instead of silent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FloatingAlias {
    /// Rate-table key this alias pointed at when the card was fetched.
    pub target: String,
    /// Last date (inclusive, UTC) on which `target` is trusted. After this,
    /// resolution ignores the mapping entirely and falls through to the
    /// harness fallback, so the UI's existing warning fires rather than the
    /// app quietly pricing against a stale target.
    pub expires_at: NaiveDate,
    /// Where the mapping was read from, so a human re-checking it does not
    /// have to guess. Same role as `PricingProvenance::source_url`.
    #[serde(default)]
    pub source_url: String,
}

/// Rate card shipped with the binary or customized by the user.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RateCard {
    pub version: u32,
    /// Three-letter currency code, e.g. "USD".
    pub currency: String,
    /// Token unit denomination, e.g. "per_1m_tokens".
    pub unit: String,
    /// Canonical URL for the live rate schedule.
    pub source_url: String,
    /// ISO8601 timestamp of the last successful fetch; null when using placeholder values.
    pub fetched_at: Option<String>,
    pub models: HashMap<String, ModelRate>,
    /// Model key to use when a session's model is not found in `models`.
    pub fallback_model: String,
    /// Per-harness currency labels (e.g. codex -> "credits", claude_code -> "USD").
    /// Falls back to `currency` when a harness is absent.
    #[serde(default)]
    pub currencies: HashMap<String, String>,
    /// Per-harness fallback models, so an unknown Claude model doesn't fall
    /// back to a Codex credit rate. Falls back to `fallback_model` when absent.
    #[serde(default)]
    pub fallback_models: HashMap<String, String>,
    /// OpenAI API USD rates for Codex models — powers the est.-cost column
    /// alongside the plan-credit rates in `models`.
    #[serde(default)]
    pub api_models: HashMap<String, ModelRate>,
    /// Models known to have no published price. Their usage is excluded from
    /// estimates instead of being priced with an unrelated fallback model.
    #[serde(default)]
    pub unpriced_models: Vec<String>,
    /// Time-aware rates and modifiers for explicitly labeled billing scenarios.
    /// Optional so rate-card overrides written by earlier releases still load.
    #[serde(default)]
    pub pricing_catalog: PricingCatalog,
    /// Raw provider model id -> canonical rate-table key, resolved before any
    /// fallback lookup. Chains are supported; cycles terminate (see
    /// `resolve_alias`) instead of resolving or hanging.
    #[serde(default)]
    pub model_aliases: HashMap<String, String>,
    /// Raw provider model id -> a mapping the provider documents as
    /// temporary, with the date it stops being trusted (issue #177). Checked
    /// before `model_aliases`; see [`FloatingAlias`].
    #[serde(default)]
    pub floating_model_aliases: HashMap<String, FloatingAlias>,
    /// Models that are explicitly zero-cost (free tier, local/self-hosted),
    /// as distinct from `unpriced_models` (known models with no published
    /// price) and from an ordinary missing-rate `Unavailable` resolution.
    #[serde(default)]
    pub free_local_models: Vec<String>,
    /// Per-harness user-declared subscription/custom plan configuration. See
    /// `SubscriptionPlan` — this never claims a plan-equivalent token
    /// allowance, only what the user records.
    #[serde(default)]
    pub subscription_plans: HashMap<String, SubscriptionPlan>,
    /// User-supplied display-currency conversion. `None` means the UI must
    /// show the original currency; Odometer never invents or fetches a rate.
    #[serde(default)]
    pub display_currency: Option<CurrencyConversion>,
    /// Bounded-cache-age bookkeeping for the (currently unimplemented)
    /// refresh flow. See `RateRefreshState` and `apply_refresh_candidate`.
    #[serde(default)]
    pub refresh: RateRefreshState,
}

fn rates_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("agent-odometer").join("rates.json"))
}

impl RateCard {
    /// Loads the bundled rates.json. Unknown fields (e.g. _note) are silently ignored by serde.
    pub fn load_bundled() -> anyhow::Result<Self> {
        let raw = include_str!("../rates.json");
        let card: Self = serde_json::from_str(raw)?;
        card.pricing_catalog.validate()?;
        Ok(card)
    }

    /// Loads rates from <config_dir>/agent-odometer/rates.json.
    /// If the file is missing, returns load_bundled (and does NOT seed the disk file —
    /// users can edit the editor to materialize their own copy).
    /// If the file is present but malformed, logs a warn and returns load_bundled.
    pub fn load_from_disk() -> anyhow::Result<Self> {
        let Some(path) = rates_path() else {
            return Self::load_bundled();
        };
        if !path.exists() {
            return Self::load_bundled();
        }
        match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<Self>(&raw) {
                Ok(card) => {
                    let bundled = Self::load_bundled()?;
                    let merged = merge_older_override(card, bundled.clone());
                    if let Err(e) = merged.pricing_catalog.validate() {
                        tracing::warn!(
                            "rates.json at {:?} has an invalid pricing catalog ({}); falling back to bundled",
                            path,
                            e
                        );
                        Ok(bundled)
                    } else {
                        Ok(merged)
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        "rates.json at {:?} is malformed ({}); falling back to bundled",
                        path,
                        e
                    );
                    Self::load_bundled()
                }
            },
            Err(e) => {
                tracing::warn!(
                    "could not read rates.json at {:?} ({}); falling back to bundled",
                    path,
                    e
                );
                Self::load_bundled()
            }
        }
    }

    /// Atomic-ish write to <config_dir>/agent-odometer/rates.json.
    pub fn save(&self) -> anyhow::Result<()> {
        self.pricing_catalog.validate()?;
        let path = rates_path().ok_or_else(|| anyhow::anyhow!("could not determine config dir"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Write to a temp file alongside the target, then rename for atomicity.
        let tmp = path.with_extension("json.tmp");
        let serialized = serde_json::to_string_pretty(self)?;
        std::fs::write(&tmp, &serialized)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Resolves a raw model id against `table` (the legacy plan-credit
    /// `models` map, `api_models`, or any other rate table sharing this
    /// card's aliases/fallbacks) and records why the returned key was
    /// chosen. This is the one place every surface should call instead of
    /// re-implementing direct/alias/fallback lookup — see the module-level
    /// "one rate-card contract" requirement in AGENTS.md.
    ///
    /// Order of resolution: explicit free/local declaration, explicit
    /// known-unpriced declaration, direct match, alias match, harness (or
    /// card-wide) fallback, else `Unavailable`. A resolution that would
    /// otherwise be `Direct`/`Aliased`/`Fallback` downgrades to `Stale` when
    /// `RateRefreshState::freshness` reports the card itself is stale as of
    /// `now` — staleness is a property of the whole card's last refresh, not
    /// of any one model.
    pub fn resolve_model_pricing(
        &self,
        model: &str,
        harness: &str,
        table: &HashMap<String, ModelRate>,
        now: DateTime<Utc>,
    ) -> PricedModelResolution {
        let basis = |direct_basis: PricingBasis| -> PricingBasis {
            if self.refresh.freshness(now) == RateFreshness::Stale {
                PricingBasis::Stale
            } else {
                direct_basis
            }
        };
        if self.free_local_models.iter().any(|m| m == model) {
            return PricedModelResolution {
                resolved_model: model.to_owned(),
                basis: PricingBasis::FreeLocal,
            };
        }
        if self.unpriced_models.iter().any(|m| m == model) {
            return PricedModelResolution {
                resolved_model: model.to_owned(),
                basis: PricingBasis::Unavailable,
            };
        }
        if table.contains_key(model) {
            return PricedModelResolution {
                resolved_model: model.to_owned(),
                basis: basis(PricingBasis::Direct),
            };
        }
        // Checked before `model_aliases`, and only while unexpired: past its
        // date the mapping is deliberately ignored so resolution reaches the
        // fallback below and the UI's warning fires. Silently pricing a
        // repointed alias against its old target is the failure this exists
        // to prevent (issue #177).
        if let Some(floating) = self.floating_model_aliases.get(model) {
            if now.date_naive() <= floating.expires_at
                && table.contains_key(floating.target.as_str())
            {
                return PricedModelResolution {
                    resolved_model: floating.target.clone(),
                    basis: basis(PricingBasis::FloatingAlias),
                };
            }
        }
        let (resolved, hopped) = resolve_alias(model, &self.model_aliases);
        if hopped && table.contains_key(resolved) {
            return PricedModelResolution {
                resolved_model: resolved.to_owned(),
                basis: basis(PricingBasis::Aliased),
            };
        }
        let fallback_model = self
            .fallback_models
            .get(harness)
            .unwrap_or(&self.fallback_model);
        if table.contains_key(fallback_model.as_str()) {
            return PricedModelResolution {
                resolved_model: fallback_model.clone(),
                basis: basis(PricingBasis::Fallback),
            };
        }
        PricedModelResolution {
            resolved_model: model.to_owned(),
            basis: PricingBasis::Unavailable,
        }
    }
}

/// A `Direct`/`Aliased` model resolution is only as authoritative as its
/// least-authoritative dimension. When the priced usage actually carries
/// cache-creation tokens and the resolved rate has no published cache-write
/// premium (`ModelRate::cache_creation_rate_is_fallback`), the resulting
/// charge is partly a fallback-priced estimate, not a direct price —
/// downgrade so the UI doesn't render it as one. Other bases (`Fallback`,
/// `Unavailable`, `FreeLocal`, `Stale`, ...) already carry their own, more
/// specific provenance and are left untouched. Mirrors
/// `downgradeForCacheCreationFallback` in credits.ts — keep both in sync.
pub fn downgrade_for_cache_creation_fallback(
    basis: PricingBasis,
    rate: &ModelRate,
    cache_creation_tokens: u64,
) -> PricingBasis {
    if matches!(
        basis,
        PricingBasis::Direct | PricingBasis::Aliased | PricingBasis::FloatingAlias
    ) && cache_creation_tokens > 0
        && rate.cache_creation_rate_is_fallback()
    {
        PricingBasis::Estimated
    } else {
        basis
    }
}

/// Add models introduced by a newer bundled card without overwriting any
/// user-edited model, currency, unit, or fallback choices.
fn merge_older_override(mut disk: RateCard, bundled: RateCard) -> RateCard {
    if disk.version >= bundled.version {
        return disk;
    }
    // Backfill the newly split cache-creation dimension onto models the user
    // already customized, before bundled entries below fill in models the
    // user never touched. A disk entry's `cache_creation_input` being
    // `None` is unambiguous now (Option, not a 0.0 default that could also
    // mean "explicitly free"): it means a rate-card override written before
    // this field existed, or before the bundled card published a premium
    // for that model. Absent already prices correctly (falls back to the
    // ordinary input rate — see `ModelRate::cache_creation_rate`), so this
    // is purely an upgrade to a more precise, directly published rate when
    // one becomes available; it never touches a user's own `Some(_)` value,
    // including an explicit `Some(0.0)` "this is free" claim.
    for (model, bundled_rate) in &bundled.models {
        if let Some(existing) = disk.models.get_mut(model) {
            if existing.cache_creation_input.is_none()
                && bundled_rate.cache_creation_input.is_some()
            {
                existing.cache_creation_input = bundled_rate.cache_creation_input;
            }
        }
    }
    for (model, bundled_rate) in &bundled.api_models {
        if let Some(existing) = disk.api_models.get_mut(model) {
            if existing.cache_creation_input.is_none()
                && bundled_rate.cache_creation_input.is_some()
            {
                existing.cache_creation_input = bundled_rate.cache_creation_input;
            }
        }
    }
    for (model, rate) in bundled.models {
        disk.models.entry(model).or_insert(rate);
    }
    for (harness, currency) in bundled.currencies {
        disk.currencies.entry(harness).or_insert(currency);
    }
    for (harness, model) in bundled.fallback_models {
        disk.fallback_models.entry(harness).or_insert(model);
    }
    for (model, rate) in bundled.api_models {
        disk.api_models.entry(model).or_insert(rate);
    }
    for model in bundled.unpriced_models {
        let user_supplied_rate =
            disk.models.contains_key(&model) || disk.api_models.contains_key(&model);
        if !user_supplied_rate && !disk.unpriced_models.contains(&model) {
            disk.unpriced_models.push(model);
        }
    }
    // New models an earlier release couldn't have known about must inherit
    // exactly as they do for `models`/`api_models`/`unpriced_models` above:
    // a bundled alias, free/local declaration, or plan is added only when
    // the user hasn't already recorded their own entry for that key.
    // A key the bundled card now treats as *floating* must stop being a
    // static alias on disk, even though every other merge here is
    // additive-only (issue #177).
    //
    // The daybreak names shipped as plain `model_aliases` entries in v9, so
    // an upgrading install carries both. Static aliases are checked after
    // floating ones, so the pair behaves correctly right up until the expiry
    // — and then quietly resolves `Aliased` off the stale static entry,
    // which is exactly the silent mispricing the expiry exists to prevent.
    //
    // Only a static entry pointing at the same target the bundled card
    // declares is removed, so this discards the copy an earlier bundled card
    // put there and never a mapping the user chose for themselves.
    for (raw_id, floating) in bundled.floating_model_aliases {
        let superseded_bundled_entry = disk
            .model_aliases
            .get(&raw_id)
            .is_some_and(|existing| *existing == floating.target);
        if superseded_bundled_entry {
            disk.model_aliases.remove(&raw_id);
        }
        disk.floating_model_aliases
            .entry(raw_id)
            .or_insert(floating);
    }
    for (raw_id, canonical) in bundled.model_aliases {
        disk.model_aliases.entry(raw_id).or_insert(canonical);
    }
    for model in bundled.free_local_models {
        if !disk.free_local_models.contains(&model) {
            disk.free_local_models.push(model);
        }
    }
    for (harness, plan) in bundled.subscription_plans {
        disk.subscription_plans.entry(harness).or_insert(plan);
    }
    // Reconcile managed catalog rules by durable ID. This refreshes corrected
    // bundled periods/modifiers and adds newly published rules while retaining
    // a user's separately identified custom rules and notes.
    disk.pricing_catalog = merge_older_catalog(disk.pricing_catalog, bundled.pricing_catalog);
    disk.version = bundled.version;
    disk.source_url = bundled.source_url;
    disk.fetched_at = bundled.fetched_at;
    // `refresh` and `display_currency` are local/user bookkeeping, not
    // catalog content — a newer bundled card never overwrites them.
    disk
}

fn merge_older_catalog(mut disk: PricingCatalog, bundled: PricingCatalog) -> PricingCatalog {
    for bundled_period in bundled.rate_periods {
        match disk
            .rate_periods
            .iter_mut()
            .find(|period| period.id == bundled_period.id)
        {
            Some(existing) => *existing = bundled_period,
            None => disk.rate_periods.push(bundled_period),
        }
    }
    for bundled_modifier in bundled.conditional_modifiers {
        match disk
            .conditional_modifiers
            .iter_mut()
            .find(|modifier| modifier.id == bundled_modifier.id)
        {
            Some(existing) => *existing = bundled_modifier,
            None => disk.conditional_modifiers.push(bundled_modifier),
        }
    }
    for bundled_note in bundled.notes {
        if !disk.notes.contains(&bundled_note) {
            disk.notes.push(bundled_note);
        }
    }
    disk
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instant(value: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(value)
            .expect("valid RFC3339 instant")
            .with_timezone(&Utc)
    }

    fn rate(value: f64) -> ModelRate {
        ModelRate {
            input: value,
            cached_input: value,
            cache_creation_input: Some(value),
            output: value,
            reasoning: value,
        }
    }

    #[test]
    fn older_override_inherits_new_models_but_preserves_edits() {
        let disk = RateCard {
            version: 2,
            currency: "custom".into(),
            unit: "per_1m_tokens".into(),
            source_url: "old".into(),
            fetched_at: Some("old".into()),
            models: HashMap::from([("gpt-old".into(), rate(99.0))]),
            fallback_model: "gpt-old".into(),
            currencies: HashMap::new(),
            fallback_models: HashMap::new(),
            api_models: HashMap::from([("preview-covered".into(), rate(7.0))]),
            unpriced_models: vec!["preview-old".into()],
            pricing_catalog: PricingCatalog::default(),
            model_aliases: HashMap::from([("custom-alias".into(), "gpt-old".into())]),
            ..Default::default()
        };
        let bundled = RateCard {
            version: 3,
            currency: "credits".into(),
            unit: "per_1m_tokens".into(),
            source_url: "current".into(),
            fetched_at: Some("current".into()),
            models: HashMap::from([("gpt-old".into(), rate(1.0)), ("gpt-new".into(), rate(2.0))]),
            fallback_model: "gpt-new".into(),
            currencies: HashMap::from([("claude_code".into(), "USD".into())]),
            fallback_models: HashMap::from([("claude_code".into(), "claude-new".into())]),
            api_models: HashMap::from([("gpt-old".into(), rate(0.04))]),
            unpriced_models: vec!["preview-new".into(), "preview-covered".into()],
            pricing_catalog: PricingCatalog {
                notes: vec!["new scenario metadata".into()],
                ..PricingCatalog::default()
            },
            model_aliases: HashMap::from([
                ("custom-alias".into(), "gpt-new".into()),
                ("gpt-preview".into(), "gpt-new".into()),
            ]),
            free_local_models: vec!["local-llama".into()],
            subscription_plans: HashMap::from([(
                "codex".into(),
                SubscriptionPlan {
                    name: "Bundled Default".into(),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };

        let merged = merge_older_override(disk, bundled);
        assert_eq!(merged.version, 3);
        assert_eq!(merged.currency, "custom");
        assert_eq!(merged.fallback_model, "gpt-old");
        assert_eq!(merged.models["gpt-old"].input, 99.0);
        assert_eq!(merged.models["gpt-new"].input, 2.0);
        assert_eq!(merged.source_url, "current");
        // Per-harness maps introduced by a newer bundled card are inherited.
        assert_eq!(merged.currencies["claude_code"], "USD");
        assert_eq!(merged.fallback_models["claude_code"], "claude-new");
        assert_eq!(merged.api_models["gpt-old"].input, 0.04);
        assert_eq!(merged.unpriced_models, ["preview-old", "preview-new"]);
        assert_eq!(merged.api_models["preview-covered"].input, 7.0);
        assert_eq!(merged.pricing_catalog.notes, ["new scenario metadata"]);
        // A user-edited alias key survives even though the bundled card
        // redefines the same key; a brand-new bundled alias is inherited.
        assert_eq!(merged.model_aliases["custom-alias"], "gpt-old");
        assert_eq!(merged.model_aliases["gpt-preview"], "gpt-new");
        assert_eq!(merged.free_local_models, ["local-llama"]);
        assert_eq!(merged.subscription_plans["codex"].name, "Bundled Default");
    }

    #[test]
    fn older_override_backfills_cache_creation_rate_for_customized_models() {
        // A model the user customized before `cache_creation_input` existed
        // deserializes with the field absent (`None`; see `ModelRate`).
        // Merging in a newer bundled card must upgrade that `None` to the
        // bundled cache-write rate, while never touching an already-edited
        // `Some(_)` value.
        let mut user_priced_without_premium = rate(50.0);
        user_priced_without_premium.cache_creation_input = None;
        let mut user_priced_api_without_premium = rate(60.0);
        user_priced_api_without_premium.cache_creation_input = None;

        let disk = RateCard {
            version: 1,
            models: HashMap::from([("legacy-model".into(), user_priced_without_premium)]),
            api_models: HashMap::from([(
                "legacy-api-model".into(),
                user_priced_api_without_premium,
            )]),
            ..Default::default()
        };
        let mut bundled_legacy = rate(50.0);
        bundled_legacy.cache_creation_input = Some(62.5);
        let mut bundled_legacy_api = rate(60.0);
        bundled_legacy_api.cache_creation_input = Some(75.0);
        let bundled = RateCard {
            version: 2,
            models: HashMap::from([("legacy-model".into(), bundled_legacy)]),
            api_models: HashMap::from([("legacy-api-model".into(), bundled_legacy_api)]),
            ..Default::default()
        };

        let merged = merge_older_override(disk, bundled);
        assert_eq!(merged.models["legacy-model"].input, 50.0);
        assert_eq!(
            merged.models["legacy-model"].cache_creation_input,
            Some(62.5)
        );
        assert_eq!(
            merged.api_models["legacy-api-model"].cache_creation_input,
            Some(75.0)
        );
    }

    #[test]
    fn older_override_never_overwrites_an_explicit_free_cache_creation_claim() {
        // Some(0.0) is a deliberate "this model's cache writes are free"
        // claim, not an unset field — it must survive a merge exactly like
        // any other user-edited rate, never being "corrected" back to the
        // bundled card's premium.
        let mut user_explicitly_free = rate(50.0);
        user_explicitly_free.cache_creation_input = Some(0.0);
        let disk = RateCard {
            version: 1,
            models: HashMap::from([("legacy-model".into(), user_explicitly_free)]),
            ..Default::default()
        };
        let mut bundled_legacy = rate(50.0);
        bundled_legacy.cache_creation_input = Some(62.5);
        let bundled = RateCard {
            version: 2,
            models: HashMap::from([("legacy-model".into(), bundled_legacy)]),
            ..Default::default()
        };

        let merged = merge_older_override(disk, bundled);
        assert_eq!(
            merged.models["legacy-model"].cache_creation_input,
            Some(0.0)
        );
    }

    #[test]
    fn older_catalog_refreshes_bundled_ids_but_preserves_custom_ids() {
        let provenance = PricingProvenance {
            evidence: "published".into(),
            source_url: "https://example.test/rates".into(),
            verified_at: instant("2026-01-01T00:00:00Z"),
            note: None,
        };
        let managed_disk_period = EffectiveRatePeriod {
            id: "provider/model/api/base".into(),
            surface: PricingSurface::OpenaiApiUsd,
            model: "managed-model".into(),
            from: instant("2026-01-01T00:00:00Z"),
            to: None,
            rate: rate(99.0),
            cache_write_input_multiplier: None,
            provenance: provenance.clone(),
            label: "stale bundled rate".into(),
        };
        let custom_period = EffectiveRatePeriod {
            id: "user/custom-model/api/base".into(),
            model: "custom-model".into(),
            rate: rate(77.0),
            label: "custom rate".into(),
            ..managed_disk_period.clone()
        };
        let bundled_period = EffectiveRatePeriod {
            rate: rate(2.0),
            label: "refreshed bundled rate".into(),
            ..managed_disk_period.clone()
        };
        let bundled_modifier = ConditionalRateModifier {
            id: "provider/model/api/high-context".into(),
            surface: PricingSurface::OpenaiApiUsd,
            model: "managed-model".into(),
            from: instant("2026-01-01T00:00:00Z"),
            to: None,
            condition: PricingCondition::RequestInputTokenThreshold { greater_than: 1 },
            multipliers: RateMultipliers {
                input: 2.0,
                output: 1.5,
            },
            provenance,
            label: "new bundled modifier".into(),
        };

        let merged = merge_older_catalog(
            PricingCatalog {
                rate_periods: vec![managed_disk_period, custom_period],
                conditional_modifiers: Vec::new(),
                notes: vec!["custom note".into()],
            },
            PricingCatalog {
                rate_periods: vec![bundled_period],
                conditional_modifiers: vec![bundled_modifier],
                notes: vec!["bundled note".into()],
            },
        );

        assert_eq!(merged.rate_periods.len(), 2);
        assert_eq!(
            merged
                .rate_periods
                .iter()
                .find(|period| period.id == "provider/model/api/base")
                .expect("managed period")
                .rate
                .input,
            2.0
        );
        assert_eq!(
            merged
                .rate_periods
                .iter()
                .find(|period| period.id == "user/custom-model/api/base")
                .expect("custom period")
                .rate
                .input,
            77.0
        );
        assert_eq!(merged.conditional_modifiers.len(), 1);
        assert_eq!(merged.notes, ["custom note", "bundled note"]);
    }

    #[test]
    fn bundled_card_prices_current_claude_ids_and_excludes_unpriced_preview() {
        let card = RateCard::load_bundled().expect("bundled rate card should parse");

        let opus = &card.models["claude-opus-5"];
        assert_eq!(opus.input, 5.0);
        assert_eq!(opus.cached_input, 0.5);
        assert_eq!(opus.output, 25.0);
        assert_eq!(opus.reasoning, 25.0);

        let dated_haiku = &card.models["claude-haiku-4-5-20251001"];
        assert_eq!(dated_haiku.input, 1.0);
        assert_eq!(dated_haiku.cached_input, 0.1);
        assert_eq!(dated_haiku.output, 5.0);
        assert_eq!(dated_haiku.reasoning, 5.0);

        assert!(card
            .models
            .contains_key(&card.fallback_models["claude_code"]));
        assert!(card
            .unpriced_models
            .contains(&"gpt-5.3-codex-spark".to_string()));
        assert!(!card.models.contains_key("gpt-5.3-codex-spark"));
        assert!(!card.api_models.contains_key("gpt-5.3-codex-spark"));
    }

    /// The Daybreak aliases are floating: OpenAI repoints them as new
    /// The daybreak names are *floating* aliases: OpenAI repoints them as new
    /// frontier models ship, and pricing follows whatever they point at. That
    /// makes the mapping correct only as of the card's `fetched_at`, so this
    /// pins four things that must hold together — the alias resolves, it
    /// resolves to a model the card actually prices, resolving it is
    /// `FloatingAlias` rather than `Fallback`, and it is dated.
    ///
    /// `Fallback` is what raises "Fallback rate used for:" in the UI, so a
    /// mapping that resolved to an unpriced id would silently keep doing
    /// that while looking fixed here. `FloatingAlias` rather than `Aliased`
    /// is the other half (issue #177): `Aliased` claims an exact match and
    /// raises nothing, which is precisely the wrong thing to say about a
    /// mapping the provider will repoint without notice.
    #[test]
    fn daybreak_blue_alias_resolves_to_a_priced_model_without_falling_back() {
        let card = RateCard::load_bundled().expect("bundled rate card should parse");

        for raw in ["gpt-daybreak-blue-latest", "daybreak-blue-latest"] {
            let floating = card
                .floating_model_aliases
                .get(raw)
                .unwrap_or_else(|| panic!("{raw} should be a floating alias"));
            assert_eq!(floating.target, "gpt-5.6-sol");
            assert!(
                !card.model_aliases.contains_key(raw),
                "{raw} must not also be a static alias, which would outlive its expiry"
            );
            assert!(
                card.models.contains_key(&floating.target)
                    && card.api_models.contains_key(&floating.target),
                "{raw} points at {}, which the card must price in both currencies",
                floating.target
            );

            let before_expiry = floating
                .expires_at
                .and_hms_opt(0, 0, 0)
                .expect("midnight")
                .and_utc();
            for table in [&card.models, &card.api_models] {
                let resolved = card.resolve_model_pricing(raw, "codex", table, before_expiry);
                assert_eq!(
                    resolved.resolved_model, floating.target,
                    "{raw} must resolve to {}",
                    floating.target
                );
                assert_eq!(
                    resolved.basis,
                    PricingBasis::FloatingAlias,
                    "{raw} must resolve as FloatingAlias; Fallback surfaces the UI warning,                      and Aliased would claim an exact match that expires without saying so"
                );
            }
        }
    }

    /// The defect this issue is about: once the provider repoints the alias,
    /// continuing to price against the old target is wrong *and* silent,
    /// because alias provenance raises no warning. Past its date the mapping
    /// must be ignored entirely, so resolution reaches the fallback and the
    /// UI's existing warning fires.
    #[test]
    fn an_expired_floating_alias_falls_back_loudly_instead_of_pricing_silently() {
        let card = RateCard::load_bundled().expect("bundled rate card should parse");
        let floating = &card.floating_model_aliases["daybreak-blue-latest"];

        let day_after = floating
            .expires_at
            .succ_opt()
            .expect("a day after the expiry")
            .and_hms_opt(0, 0, 0)
            .expect("midnight")
            .and_utc();
        let resolved =
            card.resolve_model_pricing("daybreak-blue-latest", "codex", &card.models, day_after);

        assert_eq!(
            resolved.basis,
            PricingBasis::Fallback,
            "expiry must reach the fallback path, which is what warns the user"
        );
        // The resolved *model* is deliberately not asserted to change: the
        // codex fallback happens to be `gpt-5.6-sol` today, the same id this
        // alias targets. So expiry changes the provenance, not the number —
        // which is the whole point. The app keeps charging the same rate and
        // starts saying it is no longer sure, instead of silently claiming an
        // exact match for a mapping that may have been repointed.
        assert_eq!(
            resolved.resolved_model,
            *card
                .fallback_models
                .get("codex")
                .unwrap_or(&card.fallback_model),
            "an expired alias must be priced from the configured fallback, not from its own target"
        );
    }

    /// The expiry is only meaningful if it is actually in the future when the
    /// card ships. A card whose alias expired before release would raise the
    /// fallback warning on day one — the noisy failure #178 removed.
    #[test]
    fn bundled_floating_aliases_expire_after_the_card_was_fetched() {
        let card = RateCard::load_bundled().expect("bundled rate card should parse");
        let fetched_at = card
            .fetched_at
            .as_deref()
            .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
            .expect("bundled card records the date it was fetched");

        assert!(
            !card.floating_model_aliases.is_empty(),
            "this test is vacuous if the card declares no floating aliases"
        );
        for (raw, floating) in &card.floating_model_aliases {
            assert!(
                floating.expires_at > fetched_at,
                "{raw} expires {} which is not after the card's fetch ({fetched_at})",
                floating.expires_at
            );
            assert!(
                !floating.source_url.trim().is_empty(),
                "{raw} must record where its mapping was read from"
            );
        }
    }

    /// The v9 -> v10 upgrade path for #177. v9 shipped the daybreak names as
    /// plain `model_aliases` entries, so an upgrading install has them on
    /// disk. Floating aliases are checked first, so a stale static duplicate
    /// looks harmless — right up until the expiry, when resolution would fall
    /// through to it and report `Aliased`, silently reinstating exactly the
    /// mispricing the expiry exists to prevent.
    #[test]
    fn upgrading_replaces_a_superseded_static_alias_with_its_floating_form() {
        let bundled = RateCard::load_bundled().expect("bundled rate card should parse");
        let mut disk = bundled.clone();
        disk.version = bundled.version - 1;
        disk.floating_model_aliases.clear();
        // What v9 actually wrote to disk.
        disk.model_aliases
            .insert("daybreak-blue-latest".into(), "gpt-5.6-sol".into());
        // A mapping the *user* chose, which must survive untouched.
        disk.model_aliases
            .insert("my-own-alias".into(), "gpt-5.6-sol".into());

        let merged = merge_older_override(disk, bundled);

        assert!(
            !merged.model_aliases.contains_key("daybreak-blue-latest"),
            "the superseded static alias must not outlive its floating replacement's expiry"
        );
        assert_eq!(
            merged.floating_model_aliases["daybreak-blue-latest"].target,
            "gpt-5.6-sol"
        );
        assert_eq!(
            merged.model_aliases.get("my-own-alias").map(String::as_str),
            Some("gpt-5.6-sol"),
            "a user's own alias must survive, even pointing at the same target"
        );

        let expired = merged.floating_model_aliases["daybreak-blue-latest"]
            .expires_at
            .succ_opt()
            .expect("a day after the expiry")
            .and_hms_opt(0, 0, 0)
            .expect("midnight")
            .and_utc();
        assert_eq!(
            merged
                .resolve_model_pricing("daybreak-blue-latest", "codex", &merged.models, expired)
                .basis,
            PricingBasis::Fallback,
            "after upgrading, an expired floating alias must still fail loudly"
        );
    }

    /// The bundled card only reaches an install whose on-disk copy is older
    /// (`merge_older_override` returns early otherwise), so shipping an alias
    /// without bumping `version` would leave every existing user exactly as
    /// broken as before while every test here passed.
    #[test]
    fn bundled_card_version_is_ahead_of_the_last_shipped_card() {
        let card = RateCard::load_bundled().expect("bundled rate card should parse");
        assert!(
            card.version >= 10,
            "adding models or aliases requires a version bump to propagate; got {}",
            card.version
        );
    }

    #[test]
    fn catalog_uses_half_open_period_boundaries_for_sonnet_five() {
        let card = RateCard::load_bundled().expect("bundled rate card should parse");
        let introductory = card
            .pricing_catalog
            .rate_at(
                PricingSurface::AnthropicApiUsd,
                "claude-sonnet-5",
                instant("2026-08-31T23:59:59Z"),
            )
            .expect("introductory period");
        assert_eq!(introductory.rate.input, 2.0);
        assert_eq!(introductory.cache_write_input_multiplier, Some(1.25));

        let standard = card
            .pricing_catalog
            .rate_at(
                PricingSurface::AnthropicApiUsd,
                "claude-sonnet-5",
                instant("2026-09-01T00:00:00Z"),
            )
            .expect("standard period at its inclusive start");
        assert_eq!(standard.rate.input, 3.0);
        assert_eq!(standard.rate.cached_input, 0.3);
        assert_eq!(standard.rate.output, 15.0);
        assert_eq!(standard.rate.reasoning, 15.0);
        assert_eq!(standard.cache_write_input_multiplier, Some(1.25));
    }

    #[test]
    fn gpt_sol_high_context_rule_is_api_only_and_strictly_above_threshold() {
        let card = RateCard::load_bundled().expect("bundled rate card should parse");
        let at = instant("2026-07-27T00:00:00Z");

        assert!(card
            .pricing_catalog
            .modifiers_for_request(PricingSurface::CodexPlanCredits, "gpt-5.6-sol", at, 272_001,)
            .is_empty());
        assert!(card
            .pricing_catalog
            .modifiers_for_request(PricingSurface::OpenaiApiUsd, "gpt-5.6-sol", at, 272_000,)
            .is_empty());

        let modifiers = card.pricing_catalog.modifiers_for_request(
            PricingSurface::OpenaiApiUsd,
            "gpt-5.6-sol",
            at,
            272_001,
        );
        assert_eq!(modifiers.len(), 1);
        assert_eq!(modifiers[0].multipliers.input, 2.0);
        assert_eq!(modifiers[0].multipliers.output, 1.5);

        let base_period = card
            .pricing_catalog
            .rate_at(PricingSurface::OpenaiApiUsd, "gpt-5.6-sol", at)
            .expect("Sol API base period");
        assert_eq!(base_period.cache_write_input_multiplier, Some(1.25));
        let serialized = serde_json::to_value(base_period).expect("period serializes");
        assert_eq!(serialized["cache_write_input_multiplier"], 1.25);
    }

    #[test]
    fn old_rate_cards_without_catalog_fields_remain_deserializable() {
        let card: RateCard = serde_json::from_value(serde_json::json!({
            "version": 1,
            "currency": "credits",
            "unit": "per_1m_tokens",
            "source_url": "https://example.test/rates",
            "fetched_at": null,
            "models": {},
            "fallback_model": "fallback"
        }))
        .expect("pre-catalog rate card should load");
        assert!(card.pricing_catalog.is_empty());
        // New #42 fields must also default cleanly for a card written before
        // they existed, including the hand-written RateRefreshState default
        // (see its Default impl) rather than a derived zero-length window.
        assert!(card.model_aliases.is_empty());
        assert!(card.free_local_models.is_empty());
        assert!(card.subscription_plans.is_empty());
        assert!(card.display_currency.is_none());
        assert_eq!(
            card.refresh.max_cache_age_secs,
            default_max_cache_age_secs()
        );
    }

    #[test]
    fn pricing_catalog_rejects_invalid_and_overlapping_periods() {
        let provenance = PricingProvenance {
            evidence: "published".into(),
            source_url: "https://example.test/rates".into(),
            verified_at: instant("2026-01-01T00:00:00Z"),
            note: None,
        };
        let first = EffectiveRatePeriod {
            id: "test/model/api/first".into(),
            surface: PricingSurface::OpenaiApiUsd,
            model: "model".into(),
            from: instant("2026-01-01T00:00:00Z"),
            to: Some(instant("2026-02-01T00:00:00Z")),
            rate: rate(1.0),
            cache_write_input_multiplier: None,
            provenance: provenance.clone(),
            label: "first".into(),
        };
        let overlapping = EffectiveRatePeriod {
            from: instant("2026-01-15T00:00:00Z"),
            to: None,
            label: "overlapping".into(),
            ..first.clone()
        };
        assert!(PricingCatalog {
            rate_periods: vec![first.clone(), overlapping],
            ..PricingCatalog::default()
        }
        .validate()
        .is_err());

        let duplicate_id = EffectiveRatePeriod {
            from: instant("2026-02-01T00:00:00Z"),
            to: None,
            label: "duplicate ID".into(),
            ..first.clone()
        };
        assert!(PricingCatalog {
            rate_periods: vec![first.clone(), duplicate_id],
            ..PricingCatalog::default()
        }
        .validate()
        .is_err());

        let missing_id = EffectiveRatePeriod {
            id: String::new(),
            label: "missing ID".into(),
            ..first.clone()
        };
        assert!(PricingCatalog {
            rate_periods: vec![missing_id],
            ..PricingCatalog::default()
        }
        .validate()
        .is_err());

        let encoded_without_cache_write = serde_json::to_value(&first).expect("period serializes");
        assert!(encoded_without_cache_write
            .get("cache_write_input_multiplier")
            .is_none());
        let decoded_without_cache_write: EffectiveRatePeriod =
            serde_json::from_value(encoded_without_cache_write).expect("old period deserializes");
        assert_eq!(
            decoded_without_cache_write.cache_write_input_multiplier,
            None
        );

        let invalid_cache_write = EffectiveRatePeriod {
            cache_write_input_multiplier: Some(0.0),
            label: "invalid cache-write multiplier".into(),
            ..first.clone()
        };
        assert!(PricingCatalog {
            rate_periods: vec![invalid_cache_write],
            ..PricingCatalog::default()
        }
        .validate()
        .is_err());

        let invalid = EffectiveRatePeriod {
            to: Some(first.from),
            label: "invalid".into(),
            ..first
        };
        assert!(PricingCatalog {
            rate_periods: vec![invalid],
            ..PricingCatalog::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn pricing_catalog_allows_adjacent_same_condition_modifiers() {
        let provenance = PricingProvenance {
            evidence: "published".into(),
            source_url: "https://example.test/rates".into(),
            verified_at: instant("2026-01-01T00:00:00Z"),
            note: None,
        };
        let first = ConditionalRateModifier {
            id: "test/model/api/high-context-first".into(),
            surface: PricingSurface::OpenaiApiUsd,
            model: "model".into(),
            from: instant("2026-01-01T00:00:00Z"),
            to: Some(instant("2026-02-01T00:00:00Z")),
            condition: PricingCondition::RequestInputTokenThreshold {
                greater_than: 272_000,
            },
            multipliers: RateMultipliers {
                input: 2.0,
                output: 1.5,
            },
            provenance,
            label: "first high-context rule".into(),
        };
        let adjacent = ConditionalRateModifier {
            id: "test/model/api/high-context-second".into(),
            from: instant("2026-02-01T00:00:00Z"),
            to: None,
            label: "second high-context rule".into(),
            ..first.clone()
        };

        PricingCatalog {
            conditional_modifiers: vec![first, adjacent],
            ..PricingCatalog::default()
        }
        .validate()
        .expect("adjacent half-open modifier periods must not overlap");
    }

    #[test]
    fn pricing_catalog_rejects_overlapping_same_condition_modifiers() {
        let provenance = PricingProvenance {
            evidence: "published".into(),
            source_url: "https://example.test/rates".into(),
            verified_at: instant("2026-01-01T00:00:00Z"),
            note: None,
        };
        let first = ConditionalRateModifier {
            id: "test/model/api/high-context-first".into(),
            surface: PricingSurface::OpenaiApiUsd,
            model: "model".into(),
            from: instant("2026-01-01T00:00:00Z"),
            to: Some(instant("2026-02-01T00:00:00Z")),
            condition: PricingCondition::RequestInputTokenThreshold {
                greater_than: 272_000,
            },
            multipliers: RateMultipliers {
                input: 2.0,
                output: 1.5,
            },
            provenance,
            label: "first high-context rule".into(),
        };
        let overlapping = ConditionalRateModifier {
            id: "test/model/api/high-context-overlapping".into(),
            from: instant("2026-01-15T00:00:00Z"),
            to: None,
            label: "overlapping high-context rule".into(),
            ..first.clone()
        };

        assert!(PricingCatalog {
            conditional_modifiers: vec![first, overlapping],
            ..PricingCatalog::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn cache_creation_rate_falls_back_to_input_when_absent() {
        let mut absent = rate(3.0);
        absent.cache_creation_input = None;
        assert_eq!(absent.cache_creation_rate(), 3.0);
        assert!(absent.cache_creation_rate_is_fallback());
    }

    #[test]
    fn cache_creation_rate_uses_an_explicit_value_including_zero() {
        let mut free = rate(3.0);
        free.cache_creation_input = Some(0.0);
        assert_eq!(free.cache_creation_rate(), 0.0);
        assert!(!free.cache_creation_rate_is_fallback());

        let mut premium = rate(3.0);
        premium.cache_creation_input = Some(3.75);
        assert_eq!(premium.cache_creation_rate(), 3.75);
        assert!(!premium.cache_creation_rate_is_fallback());
    }

    fn card_with(models: HashMap<String, ModelRate>, aliases: HashMap<String, String>) -> RateCard {
        RateCard {
            version: 1,
            models,
            fallback_model: "fallback-model".into(),
            model_aliases: aliases,
            ..Default::default()
        }
    }

    #[test]
    fn downgrade_for_cache_creation_fallback_only_applies_to_authoritative_bases() {
        let mut rate_without_premium = rate(3.0);
        rate_without_premium.cache_creation_input = None;

        // Direct/Aliased with nonzero cache-creation usage and no published
        // premium downgrades to Estimated.
        assert_eq!(
            downgrade_for_cache_creation_fallback(PricingBasis::Direct, &rate_without_premium, 100),
            PricingBasis::Estimated
        );
        assert_eq!(
            downgrade_for_cache_creation_fallback(
                PricingBasis::Aliased,
                &rate_without_premium,
                100
            ),
            PricingBasis::Estimated
        );
        // No cache-creation usage: nothing to downgrade for.
        assert_eq!(
            downgrade_for_cache_creation_fallback(PricingBasis::Direct, &rate_without_premium, 0),
            PricingBasis::Direct
        );
        // A published premium (even Some(0.0), a deliberate "free" claim) is
        // authoritative — no downgrade.
        let mut rate_with_premium = rate(3.0);
        rate_with_premium.cache_creation_input = Some(0.0);
        assert_eq!(
            downgrade_for_cache_creation_fallback(PricingBasis::Direct, &rate_with_premium, 100),
            PricingBasis::Direct
        );
        // Already-non-authoritative bases keep their own provenance.
        assert_eq!(
            downgrade_for_cache_creation_fallback(
                PricingBasis::Fallback,
                &rate_without_premium,
                100
            ),
            PricingBasis::Fallback
        );
        assert_eq!(
            downgrade_for_cache_creation_fallback(
                PricingBasis::Unavailable,
                &rate_without_premium,
                100
            ),
            PricingBasis::Unavailable
        );
    }

    #[test]
    fn resolve_model_pricing_prefers_direct_over_alias_and_fallback() {
        let card = card_with(
            HashMap::from([
                ("claude-sonnet-5".into(), rate(3.0)),
                ("fallback-model".into(), rate(9.0)),
            ]),
            HashMap::from([("claude-sonnet-5-20260815".into(), "claude-sonnet-5".into())]),
        );
        let now = instant("2026-01-01T00:00:00Z");

        let direct =
            card.resolve_model_pricing("claude-sonnet-5", "claude_code", &card.models, now);
        assert_eq!(direct.basis, PricingBasis::Direct);
        assert_eq!(direct.resolved_model, "claude-sonnet-5");
    }

    #[test]
    fn resolve_model_pricing_resolves_alias_before_fallback() {
        let card = card_with(
            HashMap::from([
                ("claude-sonnet-5".into(), rate(3.0)),
                ("fallback-model".into(), rate(9.0)),
            ]),
            HashMap::from([("claude-sonnet-5-20260815".into(), "claude-sonnet-5".into())]),
        );
        let now = instant("2026-01-01T00:00:00Z");

        let aliased = card.resolve_model_pricing(
            "claude-sonnet-5-20260815",
            "claude_code",
            &card.models,
            now,
        );
        assert_eq!(aliased.basis, PricingBasis::Aliased);
        assert_eq!(aliased.resolved_model, "claude-sonnet-5");
    }

    #[test]
    fn resolve_model_pricing_falls_back_when_neither_model_nor_alias_resolve() {
        let card = card_with(
            HashMap::from([("fallback-model".into(), rate(9.0))]),
            HashMap::new(),
        );
        let now = instant("2026-01-01T00:00:00Z");

        let fallback = card.resolve_model_pricing("totally-unknown", "codex", &card.models, now);
        assert_eq!(fallback.basis, PricingBasis::Fallback);
        assert_eq!(fallback.resolved_model, "fallback-model");
    }

    #[test]
    fn resolve_model_pricing_is_unavailable_without_model_alias_or_fallback_rate() {
        let card = card_with(HashMap::new(), HashMap::new());
        let now = instant("2026-01-01T00:00:00Z");

        let unavailable = card.resolve_model_pricing("totally-unknown", "codex", &card.models, now);
        assert_eq!(unavailable.basis, PricingBasis::Unavailable);
    }

    #[test]
    fn resolve_model_pricing_distinguishes_unpriced_and_free_local_from_unavailable() {
        let mut card = card_with(HashMap::new(), HashMap::new());
        card.unpriced_models.push("known-unpriced".into());
        card.free_local_models.push("local-llama".into());
        let now = instant("2026-01-01T00:00:00Z");

        assert_eq!(
            card.resolve_model_pricing("known-unpriced", "codex", &card.models, now)
                .basis,
            PricingBasis::Unavailable
        );
        assert_eq!(
            card.resolve_model_pricing("local-llama", "codex", &card.models, now)
                .basis,
            PricingBasis::FreeLocal
        );
    }

    #[test]
    fn resolve_model_pricing_downgrades_to_stale_when_refresh_is_old() {
        let mut card = card_with(
            HashMap::from([("claude-sonnet-5".into(), rate(3.0))]),
            HashMap::new(),
        );
        card.refresh = RateRefreshState {
            last_success_at: Some(instant("2026-01-01T00:00:00Z")),
            max_cache_age_secs: 60,
            ..Default::default()
        };
        let now = instant("2026-01-01T00:10:00Z"); // 600s later, past the 60s bound

        let resolution =
            card.resolve_model_pricing("claude-sonnet-5", "claude_code", &card.models, now);
        assert_eq!(resolution.basis, PricingBasis::Stale);
    }

    #[test]
    fn resolve_alias_follows_multi_hop_chains() {
        let aliases = HashMap::from([
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "c".to_string()),
        ]);
        let (resolved, hopped) = resolve_alias("a", &aliases);
        assert_eq!(resolved, "c");
        assert!(hopped);
    }

    #[test]
    fn resolve_alias_terminates_on_a_cycle_instead_of_looping_forever() {
        let aliases = HashMap::from([
            ("a".to_string(), "b".to_string()),
            ("b".to_string(), "a".to_string()),
        ]);
        // The real assertion is that this returns at all: a naive
        // implementation without cycle detection would loop forever.
        let (resolved, hopped) = resolve_alias("a", &aliases);
        assert!(hopped);
        assert!(["a", "b"].contains(&resolved));
    }

    #[test]
    fn resolve_alias_self_reference_terminates() {
        let aliases = HashMap::from([("a".to_string(), "a".to_string())]);
        let (resolved, hopped) = resolve_alias("a", &aliases);
        assert_eq!(resolved, "a");
        assert!(hopped);
    }

    #[test]
    fn resolve_model_pricing_treats_cycle_as_unresolved_alias() {
        // Neither "a" nor "b" ever appears in a rate table in this scenario
        // (only aliases point at each other, with no canonical target) — the
        // resolution must still terminate and fall through to Fallback
        // rather than hang or panic.
        let card = card_with(
            HashMap::from([("fallback-model".into(), rate(9.0))]),
            HashMap::from([
                ("a".to_string(), "b".to_string()),
                ("b".to_string(), "a".to_string()),
            ]),
        );
        let now = instant("2026-01-01T00:00:00Z");
        let resolution = card.resolve_model_pricing("a", "codex", &card.models, now);
        assert_eq!(resolution.basis, PricingBasis::Fallback);
        assert_eq!(resolution.resolved_model, "fallback-model");
    }

    /// #42: "Invalid/partial remote price data fails closed to the last
    /// valid or bundled card."
    ///
    /// A negative rate produces negative costs. Before this check, both it
    /// and the NaN case below were accepted, because parsing is not
    /// validation — they are valid JSON numbers.
    #[test]
    fn a_candidate_with_a_negative_rate_is_rolled_back() {
        let previous = card_with(HashMap::from([("m".into(), rate(3.0))]), HashMap::new());
        let mut candidate = previous.clone();
        candidate.models.insert("m".into(), rate(-5.0));

        let applied = apply_refresh_candidate(&previous, candidate, Utc::now());

        assert_eq!(applied.models["m"].input, 3.0, "the old card must stand");
        assert!(applied.refresh.last_success_at.is_none());
        assert!(
            applied
                .refresh
                .last_failure_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("non-finite")),
            "the rollback must say why: {:?}",
            applied.refresh.last_failure_reason
        );
    }

    /// NaN is the dangerous one: it propagates silently through every sum
    /// that touches it, so one poisoned rate turns whole-corpus totals into
    /// NaN with no error raised anywhere — and unlike a merely wrong number,
    /// it cannot be spotted as implausible in a table.
    #[test]
    fn a_candidate_with_a_non_finite_rate_is_rolled_back() {
        let previous = card_with(HashMap::from([("m".into(), rate(3.0))]), HashMap::new());
        for poison in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut candidate = previous.clone();
            candidate.models.insert("m".into(), rate(poison));

            let applied = apply_refresh_candidate(&previous, candidate, Utc::now());

            assert_eq!(
                applied.models["m"].input, 3.0,
                "{poison} must not reach the applied card"
            );
        }
    }

    /// The API table is priced from too, so it gets the same guard — a
    /// candidate whose plan rates are clean and whose API rates are not is
    /// still not safe to apply.
    #[test]
    fn a_candidate_with_a_poisoned_api_rate_is_rolled_back() {
        let mut previous = card_with(HashMap::from([("m".into(), rate(3.0))]), HashMap::new());
        previous.api_models.insert("m".into(), rate(1.0));
        let mut candidate = previous.clone();
        candidate.api_models.insert("m".into(), rate(f64::NAN));

        let applied = apply_refresh_candidate(&previous, candidate, Utc::now());

        assert_eq!(applied.api_models["m"].input, 1.0);
        assert!(applied.refresh.last_success_at.is_none());
    }

    /// A published cache-write premium is priced from just like the others,
    /// so it cannot be exempt from the check.
    #[test]
    fn a_candidate_with_a_poisoned_cache_creation_rate_is_rolled_back() {
        let previous = card_with(HashMap::from([("m".into(), rate(3.0))]), HashMap::new());
        let mut poisoned = rate(3.0);
        poisoned.cache_creation_input = Some(-1.0);
        let mut candidate = previous.clone();
        candidate.models.insert("m".into(), poisoned);

        let applied = apply_refresh_candidate(&previous, candidate, Utc::now());

        assert_eq!(
            applied.models["m"].cache_creation_input, previous.models["m"].cache_creation_input,
            "the previous card's premium must stand, whatever it was"
        );
        assert!(applied.refresh.last_success_at.is_none());
    }

    /// A zero rate is legitimate — a provider can genuinely price a
    /// dimension at nothing — so the guard must reject the unusable without
    /// also rejecting the free.
    #[test]
    fn a_candidate_with_a_zero_rate_is_still_accepted() {
        let previous = card_with(HashMap::from([("m".into(), rate(3.0))]), HashMap::new());
        let mut candidate = previous.clone();
        candidate.models.insert("m".into(), rate(0.0));

        let applied = apply_refresh_candidate(&previous, candidate, Utc::now());

        assert_eq!(applied.models["m"].input, 0.0);
        assert!(applied.refresh.last_success_at.is_some());
    }

    #[test]
    fn apply_refresh_candidate_accepts_a_valid_richer_candidate() {
        let previous = card_with(
            HashMap::from([("claude-sonnet-5".into(), rate(3.0))]),
            HashMap::new(),
        );
        let candidate = card_with(
            HashMap::from([
                ("claude-sonnet-5".into(), rate(3.5)),
                ("claude-haiku-5".into(), rate(1.0)),
            ]),
            HashMap::new(),
        );
        let now = instant("2026-01-01T00:00:00Z");

        let result = apply_refresh_candidate(&previous, candidate, now);
        assert_eq!(result.models["claude-sonnet-5"].input, 3.5);
        assert_eq!(result.refresh.last_success_at, Some(now));
        assert_eq!(result.refresh.last_failure_reason, None);
        assert_eq!(result.refresh.freshness(now), RateFreshness::Fresh);
    }

    #[test]
    fn apply_refresh_candidate_rolls_back_on_invalid_catalog() {
        let previous = card_with(
            HashMap::from([("claude-sonnet-5".into(), rate(3.0))]),
            HashMap::new(),
        );
        let provenance = PricingProvenance {
            evidence: "published".into(),
            source_url: "https://example.test/rates".into(),
            verified_at: instant("2026-01-01T00:00:00Z"),
            note: None,
        };
        let invalid_period = EffectiveRatePeriod {
            id: String::new(), // Empty ID always fails validation.
            surface: PricingSurface::AnthropicApiUsd,
            model: "claude-sonnet-5".into(),
            from: instant("2026-01-01T00:00:00Z"),
            to: None,
            rate: rate(3.0),
            cache_write_input_multiplier: None,
            provenance,
            label: "invalid".into(),
        };
        let mut candidate = card_with(
            HashMap::from([("claude-sonnet-5".into(), rate(99.0))]),
            HashMap::new(),
        );
        candidate.pricing_catalog.rate_periods.push(invalid_period);
        let now = instant("2026-01-01T00:00:00Z");

        let result = apply_refresh_candidate(&previous, candidate, now);
        // Failed closed: the previous card's prices are retained exactly...
        assert_eq!(result.models["claude-sonnet-5"].input, 3.0);
        // ...and the failed attempt is recorded rather than silently dropped.
        assert_eq!(result.refresh.last_attempt_at, Some(now));
        assert!(result.refresh.last_success_at.is_none());
        assert!(result
            .refresh
            .last_failure_reason
            .as_deref()
            .unwrap_or_default()
            .contains("validation"));
    }

    #[test]
    fn apply_refresh_candidate_rolls_back_on_partial_empty_models() {
        let previous = card_with(
            HashMap::from([("claude-sonnet-5".into(), rate(3.0))]),
            HashMap::new(),
        );
        // A candidate that parses but comes back with no models at all (e.g.
        // a truncated or empty payload) must never silently wipe out
        // existing coverage.
        let candidate = card_with(HashMap::new(), HashMap::new());
        let now = instant("2026-01-01T00:00:00Z");

        let result = apply_refresh_candidate(&previous, candidate, now);
        assert_eq!(result.models["claude-sonnet-5"].input, 3.0);
        assert!(result
            .refresh
            .last_failure_reason
            .as_deref()
            .unwrap_or_default()
            .contains("partial"));
    }

    #[test]
    fn rate_refresh_state_freshness_is_unknown_without_a_success() {
        let state = RateRefreshState::default();
        assert_eq!(
            state.freshness(instant("2026-01-01T00:00:00Z")),
            RateFreshness::Unknown
        );
    }

    #[test]
    fn currency_conversion_multiplies_by_the_user_supplied_rate_only() {
        let conversion = CurrencyConversion {
            target_currency: "EUR".into(),
            rate: 0.9,
            as_of: instant("2026-01-01T00:00:00Z"),
            source: "user-entered".into(),
        };
        assert_eq!(conversion.convert(100.0), Some(90.0));
        let invalid = CurrencyConversion {
            rate: 0.0,
            ..conversion
        };
        assert_eq!(invalid.convert(100.0), None);
    }
}
