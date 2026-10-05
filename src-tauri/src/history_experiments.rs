//! Small, frozen offline comparisons. Imported evidence never executes a prompt.
use super::*;
use crate::{
    query,
    rates::{ModelRate, PricingBasis, RateCard},
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_EXPERIMENTS: i64 = 8;
const MAX_MANIFEST: usize = 64 * 1024;
const MAX_RUN: usize = 8 * 1024;
static NEXT: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptVariant {
    pub id: String,
    pub prompt_id: String,
    pub prompt_version: String,
    pub prompt: String,
    pub provider: String,
    pub model: String,
    pub service_tier: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenRate {
    pub configured_version: u32,
    pub currency: String,
    pub table: String,
    pub quoted_at: String,
    pub resolved_model: String,
    pub basis: PricingBasis,
    pub cache_creation_basis: PricingBasis,
    pub effective_per_million: Option<ModelRate>,
    pub applied_tier_multiplier: Option<f64>,
    pub modifier: Option<FrozenModifier>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenModifier {
    pub id: String,
    pub surface: crate::rates::PricingSurface,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub evidence: String,
    pub verified_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenVariant {
    pub conditions: PromptVariant,
    pub prompt_hash: String,
    pub rate: FrozenRate,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseMember {
    pub case_id: i64,
    pub dataset_case_version: i64,
    pub dataset_content_hash: String,
    pub input_hash: String,
    pub expected_outcome: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExperimentManifest {
    pub format_version: u32,
    pub mode: String,
    pub active_replay: String,
    pub name: String,
    pub rubric: String,
    pub dataset_revision: i64,
    pub captured_at: String,
    pub rate_snapshot_hash: String,
    pub variants: Vec<FrozenVariant>,
    pub members: Vec<CaseMember>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FreezeRequest {
    pub dataset_revision: i64,
    pub name: String,
    pub rubric: String,
    pub variants: Vec<PromptVariant>,
    pub redact_phrases: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct FreezePreview {
    pub token: String,
    pub manifest: ExperimentManifest,
    pub inputs: Vec<(i64, CuratedContent)>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportedRun {
    pub case_id: i64,
    pub variant: String,
    pub status: String,
    #[serde(default)]
    pub output: String,
    pub elapsed_ms: Option<u64>,
    pub actual_cost_usd: Option<f64>,
    pub tokens: Option<TokenTotals>,
    pub quality: Option<String>,
    #[serde(default)]
    pub observed_input_hash: String,
    #[serde(default)]
    pub observed_model: String,
    #[serde(default)]
    pub observed_provider: String,
    #[serde(default)]
    pub observed_service_tier: String,
    #[serde(default)]
    pub observed_prompt_id: String,
    #[serde(default)]
    pub observed_prompt_version: String,
    #[serde(default)]
    pub observed_prompt_hash: String,
    #[serde(default)]
    pub observed_rate_hash: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenRun {
    pub record: ImportedRun,
    pub output_truncated: bool,
    pub estimated_api_usd: Option<f64>,
    pub estimate_basis: PricingBasis,
    pub condition_notes: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub experiment_id: i64,
    pub revision: i64,
    pub rows: Vec<ImportedRun>,
    pub redact_phrases: Vec<String>,
}
#[derive(Debug, Clone, Serialize)]
pub struct ImportPreview {
    pub token: String,
    pub experiment_id: i64,
    pub revision: i64,
    pub rows: Vec<FrozenRun>,
}
pub(super) enum PendingBody {
    Freeze(FreezePreview),
    Import(ImportPreview),
}
pub(super) struct Pending {
    token: String,
    created: Instant,
    body: PendingBody,
}
#[derive(Debug, Serialize)]
pub struct ExperimentHeader {
    pub id: i64,
    pub revision: i64,
    pub name: String,
    pub dataset_revision: i64,
    pub expected_cases: usize,
    pub captured_at: String,
}
#[derive(Debug, Default, Serialize)]
pub struct Measure {
    pub count: usize,
    pub mean: Option<f64>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
}
impl Measure {
    fn from(values: Vec<f64>) -> Self {
        if values.is_empty() {
            return Self::default();
        }
        Self {
            count: values.len(),
            mean: Some(values.iter().sum::<f64>() / values.len() as f64),
            minimum: values.iter().copied().reduce(f64::min),
            maximum: values.iter().copied().reduce(f64::max),
        }
    }
}
#[derive(Debug, Default, Serialize)]
pub struct VariantSummary {
    pub variant: String,
    pub expected_cases: usize,
    pub completed: usize,
    pub failed: usize,
    pub missing: usize,
    pub missing_output: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub unresolved: usize,
    pub not_rated: usize,
    pub quality_missing: usize,
    pub conditions_unverified: usize,
    pub elapsed_ms: Measure,
    pub actual_cost_usd: Measure,
    pub estimated_api_usd: Measure,
}
#[derive(Debug, Serialize)]
pub struct ExperimentCase {
    pub member: CaseMember,
    pub input: Option<CuratedContent>,
    pub a: Option<FrozenRun>,
    pub b: Option<FrozenRun>,
}
#[derive(Debug, Serialize)]
pub struct ExperimentReport {
    pub export_digest: String,
    pub id: i64,
    pub revision: i64,
    pub manifest: ExperimentManifest,
    pub cases: Vec<ExperimentCase>,
    pub summaries: Vec<VariantSummary>,
    pub removed_cases: usize,
    pub current_dataset_revision: i64,
    pub current_selected_pricing_differs: bool,
    pub changed_conditions: Vec<String>,
    pub paired_elapsed_delta_ms: Measure,
    pub paired_actual_cost_delta_usd: Measure,
    pub paired_estimated_cost_delta_usd: Measure,
    pub recovery_backup_unrestored: bool,
}

fn hash(value: &impl Serialize) -> Result<String> {
    // All hashable structs here have ordered fields and no HashMaps.
    Ok(stable_hash(&serde_json::to_string(value)?))
}
fn clean(value: &str, phrases: &[String], maximum: usize) -> String {
    let mut text = curated::redact(value.trim(), phrases).0;
    if text.len() > maximum {
        let mut end = maximum;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}
fn phrases_valid(phrases: &[String]) -> Result<()> {
    if phrases.len() > 100
        || phrases
            .iter()
            .any(|p| p.len() > 1024 || p.chars().count() > 256)
    {
        bail!("Use at most 100 exact redaction phrases of 256 characters");
    }
    Ok(())
}
fn identifier(value: &str) -> Result<()> {
    if value.trim() != value
        || value.is_empty()
        || value.len() > 128
        || value.chars().any(char::is_control)
        || curated::redact(value, &[]).0 != value
    {
        bail!("Use nonprivate identifiers up to 128 bytes without credentials, paths or URLs");
    }
    Ok(())
}
fn quote_rate(
    rates: &RateCard,
    variant: &PromptVariant,
    at: chrono::DateTime<chrono::Utc>,
) -> FrozenRate {
    let table = if variant.provider == "codex" {
        query::RateTable::ApiEstimate
    } else {
        query::RateTable::Plan
    };
    let quote = |tokens: TokenTotals| {
        query::price_tokens(
            rates,
            &variant.provider,
            &variant.model,
            Some(&variant.service_tier),
            &tokens,
            table,
            at,
        )
    };
    let base = quote(TokenTotals {
        input_tokens: 1_000_000,
        total_tokens: 1_000_000,
        ..Default::default()
    });
    let cached = quote(TokenTotals {
        input_tokens: 1_000_000,
        cached_input_tokens: 1_000_000,
        total_tokens: 1_000_000,
        ..Default::default()
    });
    let creation = quote(TokenTotals {
        input_tokens: 1_000_000,
        cache_creation_input_tokens: 1_000_000,
        total_tokens: 1_000_000,
        ..Default::default()
    });
    let output = quote(TokenTotals {
        output_tokens: 1_000_000,
        total_tokens: 1_000_000,
        ..Default::default()
    });
    let reasoning = quote(TokenTotals {
        output_tokens: 1_000_000,
        reasoning_output_tokens: 1_000_000,
        total_tokens: 1_000_000,
        ..Default::default()
    });
    let usd = variant.provider == "codex"
        || rates
            .currencies
            .get(&variant.provider)
            .map(|v| v.as_str())
            .unwrap_or(&rates.currency)
            == "USD";
    let effective = match (
        base.amount,
        cached.amount,
        creation.amount,
        output.amount,
        reasoning.amount,
    ) {
        (
            Some(input),
            Some(cached_input),
            Some(cache_creation_input),
            Some(output),
            Some(reasoning),
        ) if usd => Some(ModelRate {
            input,
            cached_input,
            cache_creation_input: Some(cache_creation_input),
            output,
            reasoning,
        }),
        _ => None,
    };
    let resolution = rates.resolve_model_pricing(
        &variant.model,
        &variant.provider,
        if variant.provider == "codex" {
            &rates.api_models
        } else {
            &rates.models
        },
        at,
    );
    let applied_tier_multiplier = query::tier_multiplier(
        rates,
        &variant.provider,
        &resolution.resolved_model,
        resolution.fallback_used,
        Some(&variant.service_tier),
        table,
        at,
    );
    let modifier = if variant.provider == "codex" {
        rates
            .pricing_catalog
            .modifier_for_tier(
                crate::rates::PricingSurface::OpenaiApiUsd,
                &resolution.resolved_model,
                at,
                &variant.service_tier,
            )
            .map(|m| FrozenModifier {
                id: clean(&m.id, &[], 128),
                surface: m.surface,
                effective_from: m.from.to_rfc3339(),
                effective_to: m.to.map(|v| v.to_rfc3339()),
                evidence: clean(&m.provenance.evidence, &[], 128),
                verified_at: m.provenance.verified_at.to_rfc3339(),
            })
    } else {
        None
    };
    FrozenRate {
        configured_version: rates.version,
        currency: "USD".into(),
        table: "frozen_api_base_estimate".into(),
        quoted_at: at.to_rfc3339(),
        resolved_model: clean(&base.resolved_model, &[], 128),
        basis: if usd {
            base.basis
        } else {
            PricingBasis::Unavailable
        },
        cache_creation_basis: if usd {
            creation.basis
        } else {
            PricingBasis::Unavailable
        },
        effective_per_million: effective,
        applied_tier_multiplier,
        modifier,
    }
}
fn estimate(tokens: &TokenTotals, rate: &FrozenRate) -> Option<f64> {
    rate.effective_per_million
        .as_ref()
        .map(|r| query::token_cost(tokens, r, 1.0))
}
fn changed_private(tx: &Transaction<'_>) -> Result<()> {
    tx.execute("INSERT INTO history_meta(key,value) VALUES('organization_revision','1') ON CONFLICT(key) DO UPDATE SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT)",[])?;
    Ok(())
}
fn read_manifest(
    conn: &Connection,
    id: i64,
    expected: Option<i64>,
) -> Result<(i64, ExperimentManifest)> {
    let (revision, json): (i64, String) = conn
        .query_row(
            "SELECT revision,manifest_json FROM offline_experiments WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| anyhow!("Offline comparison unavailable; reload"))?;
    if expected.is_some_and(|v| v != revision) {
        bail!("Offline comparison changed; reload before importing");
    }
    Ok((revision, serde_json::from_str(&json)?))
}
fn project_run(
    mut row: ImportedRun,
    member: &CaseMember,
    manifest: &ExperimentManifest,
    phrases: &[String],
) -> Result<FrozenRun> {
    let variant = manifest
        .variants
        .iter()
        .find(|v| v.conditions.id == row.variant)
        .ok_or_else(|| anyhow!("Use variant a or b"))?;
    if !["completed", "failed", "missing"].contains(&row.status.as_str())
        || row.output.len() > 16 * 1024
        || row.elapsed_ms.is_some_and(|v| v > 604_800_000)
        || row
            .actual_cost_usd
            .is_some_and(|v| !v.is_finite() || !(0.0..=1e9).contains(&v))
    {
        bail!("Use a reported status, output up to 16 KiB, time up to seven days, and nonnegative finite USD cost");
    }
    if row.status == "missing"
        && (!row.output.is_empty()
            || row.elapsed_ms.is_some()
            || row.actual_cost_usd.is_some()
            || row.tokens.is_some()
            || row.quality.is_some())
    {
        bail!("Missing runs cannot carry fabricated outputs or measurements");
    }
    if let Some(tokens) = &row.tokens {
        if tokens.input_tokens > 1_000_000_000_000
            || tokens.output_tokens > 1_000_000_000_000
            || tokens
                .cached_input_tokens
                .checked_add(tokens.cache_creation_input_tokens)
                .is_none_or(|v| v > tokens.input_tokens)
            || tokens.reasoning_output_tokens > tokens.output_tokens
            || tokens.input_tokens.checked_add(tokens.output_tokens) != Some(tokens.total_tokens)
            || (variant.conditions.provider == "codex" && tokens.cache_creation_input_tokens != 0)
        {
            bail!("Imported token subsets and totals do not reconcile");
        }
    }
    if row
        .quality
        .as_ref()
        .is_some_and(|v| !["accepted", "rejected", "unresolved", "not_rated"].contains(&v.as_str()))
        || (row.quality.is_some() && (row.status != "completed" || row.output.trim().is_empty()))
    {
        bail!("Human quality labels require a completed run with an output");
    }
    let truncated = curated::redact(row.output.trim(), phrases).0.len() > 3000;
    row.output = clean(&row.output, phrases, 3000);
    let mut notes = vec![];
    for (label, observed, expected) in [
        ("input", &mut row.observed_input_hash, &member.input_hash),
        ("model", &mut row.observed_model, &variant.conditions.model),
        (
            "provider",
            &mut row.observed_provider,
            &variant.conditions.provider,
        ),
        (
            "service tier",
            &mut row.observed_service_tier,
            &variant.conditions.service_tier,
        ),
        (
            "prompt identifier",
            &mut row.observed_prompt_id,
            &variant.conditions.prompt_id,
        ),
        (
            "prompt version",
            &mut row.observed_prompt_version,
            &variant.conditions.prompt_version,
        ),
        (
            "prompt text",
            &mut row.observed_prompt_hash,
            &variant.prompt_hash,
        ),
        (
            "rates",
            &mut row.observed_rate_hash,
            &manifest.rate_snapshot_hash,
        ),
    ] {
        if observed.len() > 128 {
            bail!("Reported condition identifiers must fit 128 bytes");
        }
        *observed = clean(observed, phrases, 128);
        if observed.is_empty() {
            notes.push(format!("{label} not reported"));
        } else if observed != expected {
            notes.push(format!("{label} differs from frozen conditions"));
        }
    }
    let amount = row.tokens.as_ref().and_then(|t| estimate(t, &variant.rate));
    let basis = if row
        .tokens
        .as_ref()
        .is_some_and(|t| t.cache_creation_input_tokens > 0)
    {
        variant.rate.cache_creation_basis
    } else {
        variant.rate.basis
    };
    Ok(FrozenRun {
        record: row,
        output_truncated: truncated,
        estimated_api_usd: amount,
        estimate_basis: if amount.is_some() {
            basis
        } else {
            PricingBasis::Unavailable
        },
        condition_notes: notes,
    })
}

impl HistoryStore {
    fn remember_experiment(&self, token: String, body: PendingBody) {
        let mut cache = self.experiment_previews.lock().unwrap();
        cache.retain(|p| p.created.elapsed() < Duration::from_secs(300));
        if cache.len() >= 8 {
            cache.remove(0);
        }
        cache.push(Pending {
            token,
            created: Instant::now(),
            body,
        });
    }
    fn take_experiment_preview(&self, token: &str, reviewed: bool) -> Result<PendingBody> {
        if !reviewed {
            bail!("Review the exact minimized preview before storing it locally");
        }
        let mut cache = self.experiment_previews.lock().unwrap();
        let index = cache
            .iter()
            .position(|p| p.token == token && p.created.elapsed() < Duration::from_secs(300))
            .ok_or_else(|| anyhow!("Preview expired or was consumed; build it again"))?;
        Ok(cache.remove(index).body)
    }
    pub fn preview_experiment(
        &self,
        mut request: FreezeRequest,
        rates: &RateCard,
    ) -> Result<FreezePreview> {
        phrases_valid(&request.redact_phrases)?;
        if request.name.trim().is_empty()
            || request.name.len() > 128
            || request.rubric.len() > 1024
            || request.variants.len() != 2
        {
            bail!("Name the comparison and provide exactly two prompt/model variants, a and b");
        }
        let dataset = self.curated_dataset()?;
        if dataset.revision != request.dataset_revision || dataset.cases.is_empty() {
            bail!("Dataset changed or is empty; reload before freezing");
        }
        let captured = chrono::Utc::now();
        let mut variants = Vec::new();
        for (index, mut variant) in request.variants.drain(..).enumerate() {
            if variant.id != ["a", "b"][index]
                || !["codex", "claude_code", "gemini_cli"].contains(&variant.provider.as_str())
                || !["standard", "fast", "ultrafast"].contains(&variant.service_tier.as_str())
                || variant.prompt.trim().is_empty()
                || variant.prompt.len() > 4096
            {
                bail!("Use variants a and b, a supported provider, an explicit tier, and prompt text up to 4096 bytes");
            }
            for value in [&variant.prompt_id, &variant.prompt_version, &variant.model] {
                identifier(value)?;
            }
            variant.prompt = clean(&variant.prompt, &request.redact_phrases, 4096);
            let prompt_hash = hash(&variant.prompt)?;
            variants.push(FrozenVariant {
                rate: quote_rate(rates, &variant, captured),
                conditions: variant,
                prompt_hash,
            });
        }
        let mut members = vec![];
        let mut inputs = vec![];
        for case in dataset.cases {
            let mut content = case.content;
            content.name = clean(&content.name, &request.redact_phrases, 128);
            content.rubric = clean(&content.rubric, &request.redact_phrases, 1024);
            for block in &mut content.blocks {
                let (text, count) = curated::redact(&block.text, &request.redact_phrases);
                content.redactions += count;
                block.truncated |= text.len() > 3000;
                block.text = clean(&text, &[], 3000);
            }
            if serde_json::to_vec(&content)?.len() > 16 * 1024 {
                bail!("Frozen input exceeds 16 KiB after minimization");
            }
            members.push(CaseMember {
                case_id: case.id,
                dataset_case_version: case.version,
                dataset_content_hash: case.content_hash,
                input_hash: hash(&content)?,
                expected_outcome: content.expected_outcome.clone(),
            });
            inputs.push((case.id, content));
        }
        let snapshot_hash = hash(&variants.iter().map(|v| &v.rate).collect::<Vec<_>>())?;
        let manifest = ExperimentManifest {
            format_version: 1,
            mode: "imported_offline".into(),
            active_replay: "no_go".into(),
            name: clean(&request.name, &request.redact_phrases, 128),
            rubric: clean(&request.rubric, &request.redact_phrases, 1024),
            dataset_revision: request.dataset_revision,
            captured_at: captured.to_rfc3339(),
            rate_snapshot_hash: snapshot_hash,
            variants,
            members,
        };
        if serde_json::to_vec(&manifest)?.len() > MAX_MANIFEST {
            bail!("Comparison manifest exceeds its supported size");
        }
        let token = format!("freeze-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let preview = FreezePreview {
            token: token.clone(),
            manifest,
            inputs,
        };
        self.remember_experiment(token, PendingBody::Freeze(preview.clone()));
        Ok(preview)
    }
    pub fn commit_experiment(
        &self,
        token: &str,
        reviewed: bool,
        rates: &RateCard,
    ) -> Result<ExperimentReport> {
        let PendingBody::Freeze(preview) = self.take_experiment_preview(token, reviewed)? else {
            bail!("Use a frozen comparison preview");
        };
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let revision: i64 = tx
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM history_meta WHERE key='dataset_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if revision != preview.manifest.dataset_revision {
            bail!("Dataset changed after preview; freeze it again");
        }
        let count: i64 =
            tx.query_row("SELECT COUNT(*) FROM offline_experiments", [], |r| r.get(0))?;
        if count >= MAX_EXPERIMENTS {
            bail!("At most eight frozen comparisons can be stored; remove one first");
        }
        for member in &preview.manifest.members {
            let current: Option<(i64, String)> = tx
                .query_row(
                    "SELECT version,content_hash FROM curated_cases WHERE id=?1",
                    [member.case_id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if current
                != Some((
                    member.dataset_case_version,
                    member.dataset_content_hash.clone(),
                ))
            {
                bail!("Selected case changed or was removed; rebuild the preview");
            }
        }
        tx.execute(
            "INSERT INTO offline_experiments(revision,manifest_json) VALUES(1,?1)",
            [serde_json::to_string(&preview.manifest)?],
        )?;
        let id = tx.last_insert_rowid();
        for (case_id, input) in preview.inputs {
            tx.execute(
                "INSERT INTO offline_cases(experiment_id,case_id,input_json) VALUES(?1,?2,?3)",
                params![id, case_id, serde_json::to_string(&input)?],
            )?;
        }
        changed_private(&tx)?;
        tx.commit()?;
        drop(conn);
        self.experiment_report(id, rates)
    }
    pub fn experiment_headers(&self) -> Result<Vec<ExperimentHeader>> {
        let conn = self.connection()?;
        let mut query = conn.prepare(
            "SELECT id,revision,manifest_json FROM offline_experiments ORDER BY id LIMIT 9",
        )?;
        let values = query
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let (id, revision, json) = row?;
                let manifest: ExperimentManifest = serde_json::from_str(&json)?;
                Ok(ExperimentHeader {
                    id,
                    revision,
                    name: manifest.name,
                    dataset_revision: manifest.dataset_revision,
                    expected_cases: manifest.members.len(),
                    captured_at: manifest.captured_at,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if values.len() > MAX_EXPERIMENTS as usize {
            bail!("Comparison count exceeds its supported limit");
        }
        Ok(values)
    }
    pub fn preview_experiment_import(&self, request: ImportRequest) -> Result<ImportPreview> {
        phrases_valid(&request.redact_phrases)?;
        if request.rows.is_empty()
            || request.rows.len() > 128
            || serde_json::to_vec(&request.rows)?.len() > 1024 * 1024
        {
            bail!("Import between one and 128 bounded result rows");
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let (_, manifest) = read_manifest(&tx, request.experiment_id, Some(request.revision))?;
        let mut seen = std::collections::HashSet::new();
        let mut rows = vec![];
        for row in request.rows {
            if !seen.insert((row.case_id, row.variant.clone())) {
                bail!("Each case/variant pair can appear only once per import");
            }
            let member = manifest
                .members
                .iter()
                .find(|m| m.case_id == row.case_id)
                .ok_or_else(|| anyhow!("Result case is outside the frozen dataset"))?;
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM offline_cases WHERE experiment_id=?1 AND case_id=?2)",
                params![request.experiment_id, row.case_id],
                |r| r.get(0),
            )?;
            if !exists {
                bail!("Frozen case was removed or purged; its content cannot be restored through import");
            }
            let row = project_run(row, member, &manifest, &request.redact_phrases)?;
            if serde_json::to_vec(&row)?.len() > MAX_RUN {
                bail!("Minimized result exceeds 8 KiB");
            }
            rows.push(row);
        }
        tx.commit()?;
        drop(conn);
        let token = format!("import-{}", NEXT.fetch_add(1, Ordering::Relaxed));
        let preview = ImportPreview {
            token: token.clone(),
            experiment_id: request.experiment_id,
            revision: request.revision,
            rows,
        };
        self.remember_experiment(token, PendingBody::Import(preview.clone()));
        Ok(preview)
    }
    pub fn commit_experiment_import(
        &self,
        token: &str,
        reviewed: bool,
        rates: &RateCard,
    ) -> Result<ExperimentReport> {
        let PendingBody::Import(preview) = self.take_experiment_preview(token, reviewed)? else {
            bail!("Use an imported result preview");
        };
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        read_manifest(&tx, preview.experiment_id, Some(preview.revision))?;
        for row in preview.rows {
            let sql = if row.record.variant == "a" {
                "UPDATE offline_cases SET result_a_json=?1 WHERE experiment_id=?2 AND case_id=?3"
            } else {
                "UPDATE offline_cases SET result_b_json=?1 WHERE experiment_id=?2 AND case_id=?3"
            };
            if tx.execute(
                sql,
                params![
                    serde_json::to_string(&row)?,
                    preview.experiment_id,
                    row.record.case_id
                ],
            )? != 1
            {
                bail!("Frozen case was removed; import cannot restore it");
            }
        }
        tx.execute(
            "UPDATE offline_experiments SET revision=revision+1 WHERE id=?1",
            [preview.experiment_id],
        )?;
        changed_private(&tx)?;
        tx.commit()?;
        drop(conn);
        self.experiment_report(preview.experiment_id, rates)
    }
    pub fn remove_experiment(&self, id: i64, expected_revision: i64) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        read_manifest(&tx, id, Some(expected_revision))?;
        tx.execute("DELETE FROM offline_experiments WHERE id=?1", [id])?;
        changed_private(&tx)?;
        tx.commit()?;
        Ok(())
    }

    pub fn experiment_report(&self, id: i64, rates: &RateCard) -> Result<ExperimentReport> {
        let recovery_backup_unrestored = self.organization_recovery_pending()?;
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let (revision, manifest) = read_manifest(&tx, id, None)?;
        let current_dataset_revision = tx
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM history_meta WHERE key='dataset_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let mut cases = vec![];
        for member in &manifest.members {
            let row:Option<(String,Option<String>,Option<String>)>=tx.query_row("SELECT input_json,result_a_json,result_b_json FROM offline_cases WHERE experiment_id=?1 AND case_id=?2",params![id,member.case_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let (input, a, b) = if let Some((input, a, b)) = row {
                (
                    Some(serde_json::from_str(&input)?),
                    a.map(|s| serde_json::from_str(&s)).transpose()?,
                    b.map(|s| serde_json::from_str(&s)).transpose()?,
                )
            } else {
                (None, None, None)
            };
            cases.push(ExperimentCase {
                member: member.clone(),
                input,
                a,
                b,
            });
        }
        tx.commit()?;
        let captured = chrono::DateTime::parse_from_rfc3339(&manifest.captured_at)?
            .with_timezone(&chrono::Utc);
        let current_rates: Vec<_> = manifest
            .variants
            .iter()
            .map(|v| quote_rate(rates, &v.conditions, captured))
            .collect();
        let current_selected_pricing_differs = hash(&current_rates)? != manifest.rate_snapshot_hash;
        let mut summaries = vec![];
        let mut changed_conditions = std::collections::BTreeSet::new();
        for index in 0..2 {
            let mut summary = VariantSummary {
                variant: ["a", "b"][index].into(),
                expected_cases: manifest.members.len(),
                ..Default::default()
            };
            let mut elapsed = vec![];
            let mut actual = vec![];
            let mut estimated = vec![];
            for case in &cases {
                let run = if index == 0 {
                    case.a.as_ref()
                } else {
                    case.b.as_ref()
                };
                let Some(run) = run else {
                    summary.missing += 1;
                    summary.quality_missing += 1;
                    continue;
                };
                match run.record.status.as_str() {
                    "completed" => summary.completed += 1,
                    "failed" => summary.failed += 1,
                    _ => summary.missing += 1,
                };
                if run.record.status == "completed" && run.record.output.is_empty() {
                    summary.missing_output += 1;
                }
                match run.record.quality.as_deref() {
                    Some("accepted") => summary.accepted += 1,
                    Some("rejected") => summary.rejected += 1,
                    Some("unresolved") => summary.unresolved += 1,
                    Some("not_rated") => summary.not_rated += 1,
                    _ => summary.quality_missing += 1,
                };
                if !run.condition_notes.is_empty() {
                    summary.conditions_unverified += 1;
                }
                changed_conditions.extend(run.condition_notes.iter().cloned());
                elapsed.extend(run.record.elapsed_ms.map(|n| n as f64));
                actual.extend(run.record.actual_cost_usd);
                estimated.extend(run.estimated_api_usd);
            }
            summary.elapsed_ms = Measure::from(elapsed);
            summary.actual_cost_usd = Measure::from(actual);
            summary.estimated_api_usd = Measure::from(estimated);
            summaries.push(summary);
        }
        let mut elapsed = vec![];
        let mut actual = vec![];
        let mut estimated = vec![];
        for case in &cases {
            if let (Some(a), Some(b)) = (&case.a, &case.b) {
                if let (Some(a), Some(b)) = (a.record.elapsed_ms, b.record.elapsed_ms) {
                    elapsed.push(b as f64 - a as f64);
                }
                if let (Some(a), Some(b)) = (a.record.actual_cost_usd, b.record.actual_cost_usd) {
                    actual.push(b - a);
                }
                if let (Some(a), Some(b)) = (a.estimated_api_usd, b.estimated_api_usd) {
                    estimated.push(b - a);
                }
            }
        }
        let mut report = ExperimentReport {
            export_digest: String::new(),
            id,
            revision,
            removed_cases: cases.iter().filter(|c| c.input.is_none()).count(),
            manifest,
            cases,
            summaries,
            current_dataset_revision,
            current_selected_pricing_differs,
            changed_conditions: changed_conditions.into_iter().collect(),
            paired_elapsed_delta_ms: Measure::from(elapsed),
            paired_actual_cost_delta_usd: Measure::from(actual),
            paired_estimated_cost_delta_usd: Measure::from(estimated),
            recovery_backup_unrestored,
        };
        report.export_digest = hash(&report)?;
        Ok(report)
    }

    /// The native picker happens before this call. Keep the immediate transaction
    /// through file publication so purge/import cannot win after revalidation.
    pub fn export_current_experiment(
        &self,
        id: i64,
        revision: i64,
        digest: &str,
        rates: &RateCard,
        publish: impl FnOnce(&str) -> Result<()>,
    ) -> Result<()> {
        let report = self.experiment_report(id, rates)?;
        if report.revision != revision || report.export_digest != digest {
            bail!("Reviewed comparison changed; reload before exporting");
        }
        let content = serde_json::to_string_pretty(&report)? + "\n";
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        read_manifest(&tx, id, Some(revision))?;
        let current: i64 = tx
            .query_row(
                "SELECT CAST(value AS INTEGER) FROM history_meta WHERE key='dataset_revision'",
                [],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if current != report.current_dataset_revision {
            bail!("Dataset changed; reload the comparison before exporting");
        }
        publish(&content)?;
        tx.commit()?;
        Ok(())
    }
}

pub(super) fn install_schema(tx: &Transaction<'_>) -> Result<()> {
    tx.execute_batch("CREATE TABLE offline_experiments(id INTEGER PRIMARY KEY AUTOINCREMENT,revision INTEGER NOT NULL,manifest_json TEXT NOT NULL CHECK(length(CAST(manifest_json AS BLOB))<=65536));
        CREATE TABLE offline_cases(experiment_id INTEGER NOT NULL REFERENCES offline_experiments(id) ON DELETE CASCADE,case_id INTEGER NOT NULL REFERENCES curated_cases(id) ON DELETE CASCADE,input_json TEXT NOT NULL CHECK(length(CAST(input_json AS BLOB))<=16384),result_a_json TEXT CHECK(length(CAST(result_a_json AS BLOB))<=8192),result_b_json TEXT CHECK(length(CAST(result_b_json AS BLOB))<=8192),PRIMARY KEY(experiment_id,case_id));
        CREATE INDEX offline_cases_source_idx ON offline_cases(case_id);
        CREATE TRIGGER offline_case_removed AFTER DELETE ON offline_cases BEGIN
            UPDATE offline_experiments SET revision=revision+1 WHERE id=OLD.experiment_id;
            INSERT INTO history_meta(key,value) VALUES('organization_revision','1') ON CONFLICT(key) DO UPDATE SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT);
        END;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn variant(id: &str, provider: &str, model: &str, tier: &str) -> PromptVariant {
        PromptVariant {
            id: id.into(),
            prompt_id: format!("prompt-{id}"),
            prompt_version: "1".into(),
            prompt: "Synthetic instruction: assess correctness".into(),
            provider: provider.into(),
            model: model.into(),
            service_tier: tier.into(),
        }
    }
    fn request(revision: i64) -> FreezeRequest {
        FreezeRequest {
            dataset_revision: revision,
            name: "Synthetic comparison".into(),
            rubric: "Human checks correctness".into(),
            variants: vec![
                variant("a", "codex", "gpt-5.5", "standard"),
                variant("b", "codex", "gpt-5.5", "fast"),
            ],
            redact_phrases: vec![],
        }
    }
    fn fixture() -> (tempfile::TempDir, HistoryStore, std::path::PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("synthetic.jsonl");
        std::fs::write(&source,"{\"type\":\"session_meta\",\"timestamp\":\"2026-01-01T00:00:00Z\",\"payload\":{\"id\":\"offline-synthetic\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n{\"type\":\"response_item\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"Synthetic source\"}]}}\n").unwrap();
        let session = crate::parser::parse_file(&source, false).unwrap().unwrap();
        let store = HistoryStore::open(&root.path().join("history.sqlite")).unwrap();
        let key = store.observe(&source, &session, 1).unwrap().key;
        let summary = store
            .organization_summaries(std::slice::from_ref(&key))
            .unwrap()
            .remove(0);
        let content = CuratedContent {
            name: "Synthetic frozen input".into(),
            rubric: "Synthetic rubric".into(),
            expected_outcome: "accepted".into(),
            source_provider: "codex".into(),
            source_fingerprint_at_capture: summary.identity.fingerprint.clone(),
            source_records: vec!["synthetic-anchor".into()],
            captured_at: "2026-01-01T00:00:00Z".into(),
            blocks: vec![curated::CuratedBlock {
                role: "user".into(),
                text: "SYNTHETIC_FROZEN_TEXT".into(),
                truncated: false,
            }],
            redactions: 0,
        };
        for _ in 0..3 {
            store.connection().unwrap().execute("INSERT INTO curated_cases(version,session_key,first_event_fingerprint,content_json,content_hash) VALUES(1,?1,?2,?3,?4)",params![key,summary.identity.fingerprint,serde_json::to_string(&content).unwrap(),hash(&content).unwrap()]).unwrap();
        }
        (root, store, source, key)
    }
    fn row(id: i64, variant: &str, status: &str) -> ImportedRun {
        ImportedRun {
            case_id: id,
            variant: variant.into(),
            status: status.into(),
            output: if status == "completed" {
                "Synthetic output token=PRIVATE_SYNTHETIC_SECRET".into()
            } else {
                String::new()
            },
            elapsed_ms: None,
            actual_cost_usd: None,
            tokens: None,
            quality: None,
            observed_input_hash: String::new(),
            observed_model: String::new(),
            observed_provider: String::new(),
            observed_service_tier: String::new(),
            observed_prompt_id: String::new(),
            observed_prompt_version: String::new(),
            observed_prompt_hash: String::new(),
            observed_rate_hash: String::new(),
        }
    }
    #[test]
    fn frozen_rate_adapter_conforms_to_shared_pricing_without_double_counting_subsets() {
        let rates = RateCard::load_bundled().unwrap();
        let at = "2026-08-01T12:00:00Z".parse().unwrap();
        for (provider, model, tier) in [
            ("codex", "gpt-5.5", "standard"),
            ("codex", "gpt-5.5", "fast"),
            ("codex", "missing-synthetic-model", "standard"),
            ("claude_code", "claude-sonnet-4-6", "standard"),
            ("gemini_cli", "gemini-2.5-pro", "standard"),
            ("codex", "gpt-5.5", "ultrafast"),
        ] {
            let variant = variant("a", provider, model, tier);
            let frozen = quote_rate(&rates, &variant, at);
            let tokens = TokenTotals {
                input_tokens: 1500,
                cached_input_tokens: 500,
                cache_creation_input_tokens: if provider == "codex" { 0 } else { 250 },
                output_tokens: 700,
                reasoning_output_tokens: 200,
                total_tokens: 2200,
            };
            let expected = query::price_tokens(
                &rates,
                provider,
                model,
                Some(tier),
                &tokens,
                if provider == "codex" {
                    query::RateTable::ApiEstimate
                } else {
                    query::RateTable::Plan
                },
                at,
            );
            match (estimate(&tokens, &frozen), expected.amount) {
                (Some(a), Some(b)) => assert!(
                    (a - b).abs() < 1e-10,
                    "{provider}/{model}/{tier}: {a} vs {b}"
                ),
                (None, None) => {}
                _ => panic!("Pricing availability disagrees for {provider}/{model}/{tier}"),
            };
            if tokens.cache_creation_input_tokens > 0 {
                assert_eq!(frozen.cache_creation_basis, expected.basis);
            } else {
                assert_eq!(frozen.basis, expected.basis);
            }
        }
        let mut unavailable = rates.clone();
        unavailable.api_models.clear();
        assert!(quote_rate(
            &unavailable,
            &variant("a", "codex", "gpt-5.5", "standard"),
            at
        )
        .effective_per_million
        .is_none());
        let later = quote_rate(
            &rates,
            &variant("a", "codex", "missing-synthetic-model", "standard"),
            "2036-01-01T00:00:00Z".parse().unwrap(),
        );
        assert!(!matches!(
            later.basis,
            PricingBasis::Direct | PricingBasis::Aliased
        ));
    }
    #[test]
    fn frozen_reports_preserve_rates_inputs_missing_failed_and_independent_denominators() {
        let (_root, store, _source, _key) = fixture();
        let rates = RateCard::load_bundled().unwrap();
        let dataset = store.curated_dataset().unwrap();
        let preview = store
            .preview_experiment(request(dataset.revision), &rates)
            .unwrap();
        assert!(store
            .commit_experiment(&preview.token, false, &rates)
            .is_err());
        let report = store
            .commit_experiment(&preview.token, true, &rates)
            .unwrap();
        assert!(store
            .commit_experiment(&preview.token, true, &rates)
            .is_err());
        let mut a = row(1, "a", "completed");
        a.elapsed_ms = Some(100);
        a.actual_cost_usd = Some(0.03);
        a.quality = Some("accepted".into());
        a.tokens = Some(TokenTotals {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            ..Default::default()
        });
        let mut failed = row(2, "a", "failed");
        failed.elapsed_ms = Some(60);
        failed.actual_cost_usd = Some(0.02);
        let mut b = row(1, "b", "completed");
        b.elapsed_ms = Some(150);
        b.quality = Some("rejected".into());
        b.observed_model = "changed-synthetic-model".into();
        let preview = store
            .preview_experiment_import(ImportRequest {
                experiment_id: report.id,
                revision: report.revision,
                rows: vec![a, failed, b],
                redact_phrases: vec![],
            })
            .unwrap();
        assert!(!serde_json::to_string(&preview)
            .unwrap()
            .contains("PRIVATE_SYNTHETIC_SECRET"));
        let report = store
            .commit_experiment_import(&preview.token, true, &rates)
            .unwrap();
        let before = serde_json::to_string(&report.manifest).unwrap();
        let old = report.cases[0].a.as_ref().unwrap().estimated_api_usd;
        let a = &report.summaries[0];
        assert_eq!(
            (
                a.expected_cases,
                a.completed,
                a.failed,
                a.missing,
                a.accepted,
                a.quality_missing
            ),
            (3, 1, 1, 1, 1, 2)
        );
        assert_eq!(
            (
                a.actual_cost_usd.count,
                a.elapsed_ms.count,
                a.estimated_api_usd.count
            ),
            (2, 2, 1)
        );
        assert_eq!(report.paired_elapsed_delta_ms.mean, Some(50.0));
        assert_eq!(report.paired_actual_cost_delta_usd.count, 0);
        assert!(report
            .changed_conditions
            .iter()
            .any(|v| v == "model differs from frozen conditions"));
        let mut changed = rates.clone();
        changed.api_models.get_mut("gpt-5.5").unwrap().input += 10.0;
        store.connection().unwrap().execute("UPDATE curated_cases SET content_json=replace(content_json,'SYNTHETIC_FROZEN_TEXT','NEW_DATASET_TEXT'),version=version+1 WHERE id=1",[]).unwrap();
        let reread = store.experiment_report(report.id, &changed).unwrap();
        assert!(reread.current_selected_pricing_differs);
        assert_ne!(
            reread.current_dataset_revision,
            reread.manifest.dataset_revision
        );
        assert_eq!(serde_json::to_string(&reread.manifest).unwrap(), before);
        assert_eq!(reread.cases[0].a.as_ref().unwrap().estimated_api_usd, old);
        assert_eq!(
            reread.cases[0].input.as_ref().unwrap().blocks[0].text,
            "SYNTHETIC_FROZEN_TEXT"
        );
        let invalid = store
            .preview_experiment_import(ImportRequest {
                experiment_id: report.id,
                revision: report.revision,
                rows: vec![row(1, "b", "missing")],
                redact_phrases: vec![],
            })
            .unwrap();
        store
            .commit_experiment_import(&invalid.token, true, &rates)
            .unwrap();
        let mut bad = row(1, "a", "missing");
        bad.elapsed_ms = Some(0);
        assert!(store
            .preview_experiment_import(ImportRequest {
                experiment_id: report.id,
                revision: 3,
                rows: vec![bad],
                redact_phrases: vec![]
            })
            .is_err());
    }
    #[test]
    fn purge_removes_all_copied_bodies_and_rejects_late_reviewed_imports() {
        let (_root, store, source, _key) = fixture();
        let rates = RateCard::load_bundled().unwrap();
        let dataset = store.curated_dataset().unwrap();
        let pending_freeze = store
            .preview_experiment(request(dataset.revision), &rates)
            .unwrap();
        let preview = store
            .preview_experiment(request(dataset.revision), &rates)
            .unwrap();
        let report = store
            .commit_experiment(&preview.token, true, &rates)
            .unwrap();
        let export_revision = report.revision;
        let export_digest = report.export_digest.clone();
        let pending = store
            .preview_experiment_import(ImportRequest {
                experiment_id: report.id,
                revision: report.revision,
                rows: vec![row(1, "a", "completed")],
                redact_phrases: vec![],
            })
            .unwrap();
        std::fs::remove_file(&source).unwrap();
        store.mark_path_missing(&source).unwrap();
        store
            .set_retention_policy(&RetentionPolicy {
                retained_days: Some(1),
            })
            .unwrap();
        let now = "2026-10-04T12:00:00Z".parse().unwrap();
        let preview = store.preview_purge(now).unwrap();
        assert_eq!(preview.sessions, 1);
        store.purge_retained(&preview, now).unwrap();
        assert!(store
            .commit_experiment_import(&pending.token, true, &rates)
            .is_err());
        assert!(store
            .commit_experiment(&pending_freeze.token, true, &rates)
            .is_err());
        let report = store.experiment_report(report.id, &rates).unwrap();
        assert!(store
            .export_current_experiment(
                report.id,
                export_revision,
                &export_digest,
                &rates,
                |_| panic!("purged bytes must never reach the file publisher")
            )
            .is_err());
        assert!(store
            .export_current_dataset(dataset.revision, &dataset.export_digest, |_| panic!(
                "purged dataset bytes must never reach the file publisher"
            ))
            .is_err());
        assert_eq!(report.removed_cases, 3);
        assert_eq!(report.summaries[0].missing, 3);
        assert_eq!(report.summaries[0].expected_cases, 3);
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("SYNTHETIC_FROZEN_TEXT"));
        assert!(!json.contains("PRIVATE_SYNTHETIC_SECRET"));
        assert!(store
            .preview_experiment_import(ImportRequest {
                experiment_id: report.id,
                revision: report.revision,
                rows: vec![row(1, "a", "completed")],
                redact_phrases: vec![]
            })
            .is_err());
    }
    #[test]
    fn migration_preview_expiry_and_capacity_are_bounded_and_preserve_dataset() {
        let (root, store, _source, _key) = fixture();
        let before = serde_json::to_string(&store.curated_dataset().unwrap()).unwrap();
        let accounting = serde_json::to_string(&store.session_summaries().unwrap()).unwrap();
        store.connection().unwrap().execute_batch("DROP TABLE offline_cases; DROP TABLE offline_experiments; UPDATE history_meta SET value='15' WHERE key='schema_version'; PRAGMA user_version=15;").unwrap();
        drop(store);
        let mut steps = vec![];
        let store = HistoryStore::open_with_progress(&root.path().join("history.sqlite"), |e| {
            if e.elapsed_ms.is_some() {
                steps.push(e.step)
            }
        })
        .unwrap();
        assert_eq!(steps, ["v15_to_v16_offline_comparisons"]);
        assert_eq!(
            serde_json::to_string(&store.curated_dataset().unwrap()).unwrap(),
            before
        );
        assert_eq!(
            serde_json::to_string(&store.session_summaries().unwrap()).unwrap(),
            accounting
        );
        let rates = RateCard::load_bundled().unwrap();
        let revision = store.curated_dataset().unwrap().revision;
        let first = store.preview_experiment(request(revision), &rates).unwrap();
        for _ in 0..8 {
            store.preview_experiment(request(revision), &rates).unwrap();
        }
        assert!(store.commit_experiment(&first.token, true, &rates).is_err());
        let expiry = store.preview_experiment(request(revision), &rates).unwrap();
        store
            .experiment_previews
            .lock()
            .unwrap()
            .last_mut()
            .unwrap()
            .created = Instant::now() - Duration::from_secs(301);
        assert!(store
            .commit_experiment(&expiry.token, true, &rates)
            .is_err());
        for _ in 0..8 {
            let p = store.preview_experiment(request(revision), &rates).unwrap();
            store.commit_experiment(&p.token, true, &rates).unwrap();
        }
        let p = store.preview_experiment(request(revision), &rates).unwrap();
        assert!(store.commit_experiment(&p.token, true, &rates).is_err());
        assert_eq!(store.experiment_headers().unwrap().len(), 8);
    }

    #[test]
    fn reviewed_exports_revalidate_digest_and_serialize_with_removal_through_publication() {
        let (root, store, _source, _key) = fixture();
        let rates = RateCard::load_bundled().unwrap();
        let dataset = store.curated_dataset().unwrap();
        let p = store
            .preview_experiment(request(dataset.revision), &rates)
            .unwrap();
        let report = store.commit_experiment(&p.token, true, &rates).unwrap();
        assert!(store
            .export_current_experiment(
                report.id,
                report.revision,
                "unreviewed-digest",
                &rates,
                |_| panic!("unreviewed bytes must never be published")
            )
            .is_err());
        assert!(store
            .export_current_dataset(dataset.revision, "unreviewed-digest", |_| panic!(
                "unreviewed dataset must never be published"
            ))
            .is_err());
        let file = root.path().join("reviewed.json");
        store.export_current_experiment(report.id,report.revision,&report.export_digest,&rates,|content| {
            let other=Connection::open(root.path().join("history.sqlite"))?;other.busy_timeout(Duration::ZERO)?;other.pragma_update(None,"foreign_keys","ON")?;
            let failure=other.execute("DELETE FROM curated_cases WHERE id=1",[]).unwrap_err();assert!(matches!(failure,rusqlite::Error::SqliteFailure(ref e,_) if e.code==rusqlite::ErrorCode::DatabaseBusy));
            std::fs::write(&file,content)?;Ok(())
        }).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&file).unwrap())
                .unwrap(),
            serde_json::to_value(&report).unwrap()
        );
        store
            .export_current_dataset(dataset.revision, &dataset.export_digest, |content| {
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(content)?,
                    serde_json::to_value(&dataset)?
                );
                Ok(())
            })
            .unwrap();
        store.remove_curated(1, dataset.revision).unwrap();
        let prior = std::fs::read(&file).unwrap();
        assert!(store
            .export_current_experiment(
                report.id,
                report.revision,
                &report.export_digest,
                &rates,
                |content| {
                    std::fs::write(&file, content)?;
                    Ok(())
                }
            )
            .is_err());
        assert_eq!(std::fs::read(&file).unwrap(), prior);
    }
}
