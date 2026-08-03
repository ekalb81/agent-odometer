use crate::config::Config;
use crate::model::{
    Harness, RateLimitSnapshotPoint, RateLimitWindow, Session, TokenTotals, TurnInfo,
};
use crate::provider::{claude_code_provider_id, codex_provider_id};
use crate::rates::{ModelRate, RateCard};
use anyhow::{anyhow, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_HOOK_INPUT_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Deserialize)]
struct HookInput {
    session_id: String,
    transcript_path: Option<String>,
    hook_event_name: String,
    turn_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookRunRecord {
    pub last_run_at: Option<DateTime<Utc>>,
    pub success: bool,
    pub last_receipt: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HookRunStatusFile {
    codex: HookRunRecord,
    claude_code: HookRunRecord,
}

/// Handles the headless hook entry point before Tauri starts. Returning true
/// means the process was a hook invocation and the GUI must not launch.
pub fn try_run_cli() -> bool {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("hook") {
        return false;
    }
    let harness = match args.next().as_deref() {
        Some("codex") => codex_provider_id(),
        Some("claude") | Some("claude_code") => claude_code_provider_id(),
        _ => {
            print_hook_output(None);
            return true;
        }
    };

    let config = Config::load().unwrap_or_default();
    let provider_enabled = if harness == codex_provider_id() {
        config.turn_receipts_codex
    } else if harness == claude_code_provider_id() {
        config.turn_receipts_claude
    } else {
        // Fail safe: turn receipts have no configuration toggle for a
        // provider outside the two builtins, so treat it as disabled.
        false
    };
    // A stale hook after the feature is disabled must be nearly free: do not
    // read stdin, touch the transcript, or write health data.
    if !config.turn_receipts_enabled || !provider_enabled {
        print_hook_output(None);
        return true;
    }

    let result = run_hook(harness.clone(), &config);
    match result {
        Ok(receipt) => {
            save_run_record(
                harness,
                HookRunRecord {
                    last_run_at: Some(Utc::now()),
                    success: true,
                    last_receipt: Some(receipt.clone()),
                    detail: None,
                },
            );
            print_hook_output(Some(receipt));
        }
        Err(error) => {
            let detail = sanitized_error(&error);
            save_run_record(
                harness,
                HookRunRecord {
                    last_run_at: Some(Utc::now()),
                    success: false,
                    last_receipt: None,
                    detail: Some(detail.clone()),
                },
            );
            print_hook_output(Some(format!(
                "Odometer receipt unavailable · {detail} · check Odometer Settings"
            )));
        }
    }
    true
}

fn print_hook_output(message: Option<String>) {
    // Both Codex and Claude Code accept this common Stop-hook output shape.
    // Serialization is infallible for this fixed structure; keep stdout JSON-only.
    let value = match message {
        Some(message) => serde_json::json!({
            "continue": true,
            "systemMessage": message,
        }),
        None => serde_json::json!({ "continue": true }),
    };
    println!("{value}");
}

fn run_hook(harness: Harness, config: &Config) -> anyhow::Result<String> {
    let mut raw = Vec::new();
    std::io::stdin()
        .take(MAX_HOOK_INPUT_BYTES + 1)
        .read_to_end(&mut raw)
        .context("hook input could not be read")?;
    if raw.len() as u64 > MAX_HOOK_INPUT_BYTES {
        return Err(anyhow!("hook input exceeded the safety limit"));
    }
    let input: HookInput = serde_json::from_slice(&raw).context("hook input was not valid JSON")?;
    if input.hook_event_name != "Stop" {
        return Err(anyhow!("unexpected hook event"));
    }
    if input.session_id.is_empty() || input.session_id.len() > 256 {
        return Err(anyhow!("invalid session identity"));
    }
    let transcript = input
        .transcript_path
        .as_deref()
        .map(expand_home_path)
        .ok_or_else(|| anyhow!("the harness did not provide a transcript"))?;
    validate_transcript_path(&transcript, &harness, config)?;

    let session = if harness == codex_provider_id() {
        crate::parser::parse_file(&transcript, false)
    } else if harness == claude_code_provider_id() {
        crate::claude_parser::parse_file(&transcript)
    } else {
        // Fail safe: parsing dispatch has no route for an unknown provider.
        Err(anyhow!("unsupported provider for turn receipts"))
    }
    .context("the transcript could not be parsed")?
    .ok_or_else(|| anyhow!("the transcript did not contain a session"))?;
    if session.id != input.session_id {
        return Err(anyhow!("the transcript identity did not match the hook"));
    }
    let turn = match input.turn_id.as_deref() {
        Some(id) => session
            .turns
            .iter()
            .find(|turn| turn.turn_id == id)
            .ok_or_else(|| anyhow!("the requested turn was not found"))?,
        None => session
            .turns
            .iter()
            .rev()
            .find(|turn| turn.completed_at.is_some())
            .or_else(|| session.turns.last())
            .ok_or_else(|| anyhow!("the completed turn was not found"))?,
    };
    let rates = RateCard::load_from_disk().context("the rate card could not be loaded")?;
    Ok(build_receipt(&session, turn, &rates))
}

fn expand_home_path(value: &str) -> PathBuf {
    if value == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from(value));
    }
    if let Some(relative) = value
        .strip_prefix("~/")
        .or_else(|| value.strip_prefix("~\\"))
    {
        if let Some(home) = dirs::home_dir() {
            return home.join(relative);
        }
    }
    PathBuf::from(value)
}

fn validate_transcript_path(path: &Path, harness: &Harness, config: &Config) -> anyhow::Result<()> {
    if path.extension().and_then(|value| value.to_str()) != Some("jsonl") {
        return Err(anyhow!("the transcript was not a JSONL file"));
    }
    let canonical = path
        .canonicalize()
        .context("the transcript path was unavailable")?;
    if !canonical.is_file() {
        return Err(anyhow!("the transcript path was not a file"));
    }
    let allowed_roots: Vec<&PathBuf> = if *harness == codex_provider_id() {
        config
            .session_roots
            .iter()
            .chain(config.archive_roots.iter())
            .collect()
    } else if *harness == claude_code_provider_id() {
        config.claude_session_roots.iter().collect()
    } else {
        // Fail safe: no roots are trusted for a provider without dedicated
        // hook wiring, so the path check below rejects the transcript.
        Vec::new()
    };
    let allowed = allowed_roots.iter().any(|root| {
        root.canonicalize()
            .is_ok_and(|root| canonical.starts_with(root))
    });
    if !allowed {
        return Err(anyhow!("the transcript is outside the watched roots"));
    }
    Ok(())
}

pub fn build_receipt(session: &Session, turn: &TurnInfo, rates: &RateCard) -> String {
    let turn_model = turn.model.as_deref().or(session.model.as_deref());
    let turn_plan = price_tokens(
        &turn.tokens,
        turn_model,
        turn.service_tier.as_deref(),
        &session.harness,
        rates,
        false,
    );
    let turn_api = price_tokens(
        &turn.tokens,
        turn_model,
        turn.service_tier.as_deref(),
        &session.harness,
        rates,
        true,
    );
    let session_plan = price_session(session, rates, false);
    let session_api = price_session(session, rates, true);

    let mut first = format!(
        "Odometer · turn {} · {} tokens",
        turn.index,
        format_tokens(turn.tokens.total_tokens)
    );
    if session.harness == codex_provider_id() {
        first.push_str(&format_optional_amount(turn_plan, "credits", false));
        first.push_str(&format_optional_amount(turn_api, "API", true));
    } else if session.harness == claude_code_provider_id() {
        first.push_str(&format_optional_amount(turn_plan, "API", true));
    } else {
        // Neutral fallback: no plan/credits pricing concept exists outside
        // Codex, so treat an unknown provider the same as Claude Code.
        first.push_str(&format_optional_amount(turn_plan, "API", true));
    }

    let mut lines = vec![first];
    if let Some(quota) = quota_receipt(session, turn) {
        lines.push(format!("Quota · {quota}"));
    }
    let session_line = if session.harness == codex_provider_id() {
        format!(
            "Session{}{}",
            format_session_amount(&session_plan, "credits", false),
            format_session_amount(&session_api, "API", true)
        )
    } else if session.harness == claude_code_provider_id() {
        format!(
            "Session{}",
            format_session_amount(&session_plan, "API", true)
        )
    } else {
        // Neutral fallback: same generic "API"-only shape as Claude Code.
        format!(
            "Session{}",
            format_session_amount(&session_plan, "API", true)
        )
    };
    lines.push(session_line);
    lines.join("\n")
}

#[derive(Debug, Default)]
struct SessionPrice {
    amount: Option<f64>,
    omitted_models: Vec<String>,
}

fn price_session(session: &Session, rates: &RateCard, api_table: bool) -> SessionPrice {
    let mut total = 0.0;
    let mut priced = false;
    let mut omitted_models = BTreeSet::new();
    for bucket in session.tier_buckets() {
        if let Some(cost) = price_tokens(
            &bucket.tokens,
            Some(&bucket.model),
            bucket.service_tier.as_deref(),
            &session.harness,
            rates,
            api_table,
        ) {
            total += cost;
            priced = true;
        } else {
            omitted_models.insert(bucket.model);
        }
    }
    SessionPrice {
        amount: priced.then_some(total),
        omitted_models: omitted_models.into_iter().collect(),
    }
}

fn price_tokens(
    tokens: &TokenTotals,
    model: Option<&str>,
    service_tier: Option<&str>,
    harness: &Harness,
    rates: &RateCard,
    api_table: bool,
) -> Option<f64> {
    if model.is_some_and(|model| rates.unpriced_models.iter().any(|value| value == model)) {
        return None;
    }
    let table = if api_table {
        if *harness != codex_provider_id() || rates.api_models.is_empty() {
            return None;
        }
        &rates.api_models
    } else {
        &rates.models
    };
    let fallback = rates
        .fallback_models
        .get(harness.as_str())
        .unwrap_or(&rates.fallback_model);
    let rate = model
        .and_then(|model| table.get(model))
        .or_else(|| table.get(fallback))?;
    Some(token_cost(
        tokens,
        rate,
        service_tier_multiplier(model, service_tier),
    ))
}

fn token_cost(tokens: &TokenTotals, rate: &ModelRate, multiplier: f64) -> f64 {
    let non_cached_input = tokens
        .input_tokens
        .saturating_sub(tokens.cached_input_tokens);
    let non_reasoning_output = tokens
        .output_tokens
        .saturating_sub(tokens.reasoning_output_tokens);
    ((non_cached_input as f64 * rate.input)
        + (tokens.cached_input_tokens as f64 * rate.cached_input)
        + (non_reasoning_output as f64 * rate.output)
        + (tokens.reasoning_output_tokens as f64 * rate.reasoning))
        / 1_000_000.0
        * multiplier
}

fn service_tier_multiplier(model: Option<&str>, service_tier: Option<&str>) -> f64 {
    if service_tier != Some("fast") {
        return 1.0;
    }
    match model {
        Some("gpt-5.5") => 2.5,
        Some("gpt-5.4") => 2.0,
        _ => 1.0,
    }
}

fn quota_receipt(session: &Session, turn: &TurnInfo) -> Option<String> {
    let (after_index, after) = session
        .rate_limits_history
        .iter()
        .enumerate()
        .rev()
        .find(|(_, point)| point.turn_id.as_deref() == Some(turn.turn_id.as_str()))?;
    let before = session.rate_limits_history[..after_index]
        .iter()
        .rev()
        .find(|point| point.turn_id.as_deref() != Some(turn.turn_id.as_str()));

    let mut parts = Vec::new();
    append_quota_window(&mut parts, before, after, true);
    append_quota_window(&mut parts, before, after, false);
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn append_quota_window(
    parts: &mut Vec<String>,
    before: Option<&RateLimitSnapshotPoint>,
    after: &RateLimitSnapshotPoint,
    primary: bool,
) {
    let current = if primary {
        after.primary.as_ref()
    } else {
        after.secondary.as_ref()
    };
    let Some(current) = current else { return };
    let previous = before.and_then(|point| {
        if primary {
            point.primary.as_ref()
        } else {
            point.secondary.as_ref()
        }
    });
    let label = quota_window_label(current, primary);
    let current_percent = format_percent(current.used_percent);
    let identity_is_known = before
        .and_then(|point| point.limit_id.as_deref())
        .zip(after.limit_id.as_deref())
        .is_some();
    let reset_is_known = previous
        .and_then(|previous| previous.resets_at)
        .zip(current.resets_at)
        .is_some();
    let same_identity = before
        .and_then(|point| point.limit_id.as_deref())
        .zip(after.limit_id.as_deref())
        .is_some_and(|(previous, current)| previous == current);
    let same_reset = previous
        .and_then(|previous| previous.resets_at)
        .zip(current.resets_at)
        .is_some_and(|(previous, current)| previous == current);
    let value = match previous {
        Some(previous)
            if same_identity && same_reset && current.used_percent >= previous.used_percent =>
        {
            let delta = current.used_percent - previous.used_percent;
            if delta.abs() < f64::EPSILON {
                format!("{label} no measurable change ({current_percent}% used)")
            } else {
                format!(
                    "{label} +{} pp observed ({current_percent}% used)",
                    format_percent(delta)
                )
            }
        }
        Some(_) if identity_is_known && reset_is_known => {
            format!("{label} {current_percent}% used · window changed")
        }
        Some(_) => format!("{label} {current_percent}% used · turn delta unavailable"),
        None => format!("{label} {current_percent}% used · turn delta unavailable"),
    };
    parts.push(value);
}

fn quota_window_label(window: &RateLimitWindow, primary: bool) -> String {
    match window.window_minutes {
        Some(300) => "5h".into(),
        Some(10_080) => "7d".into(),
        Some(minutes) if minutes % 1_440 == 0 => format!("{}d", minutes / 1_440),
        Some(minutes) if minutes % 60 == 0 => format!("{}h", minutes / 60),
        Some(minutes) => format!("{minutes}m"),
        None if primary => "Primary".into(),
        None => "Secondary".into(),
    }
}

fn format_percent(value: f64) -> String {
    let rendered = if value != 0.0 && value.abs() < 0.005 {
        format!("{value:.4}")
    } else {
        format!("{value:.2}")
    };
    rendered
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_owned()
}

fn format_tokens(value: u64) -> String {
    if value >= 1_000_000 {
        format_compact(value as f64 / 1_000_000.0, "M")
    } else if value >= 1_000 {
        format_compact(value as f64 / 1_000.0, "k")
    } else {
        value.to_string()
    }
}

fn format_compact(value: f64, suffix: &str) -> String {
    if value >= 100.0 || value.fract().abs() < f64::EPSILON {
        format!("{value:.0}{suffix}")
    } else {
        format!("{value:.1}{suffix}")
    }
}

fn format_optional_amount(amount: Option<f64>, label: &str, usd: bool) -> String {
    match amount {
        Some(value) => {
            let amount = if value != 0.0 && value.abs() < 0.005 {
                format!("{value:.4}")
            } else {
                format!("{value:.2}")
            };
            if usd {
                format!(" · ≈${amount} {label}")
            } else {
                format!(" · ≈{amount} {label}")
            }
        }
        None => format!(" · {label} unpriced"),
    }
}

fn format_session_amount(price: &SessionPrice, label: &str, usd: bool) -> String {
    let omitted = price.omitted_models.join(", ");
    match (price.amount, price.omitted_models.is_empty()) {
        (Some(amount), true) => format_optional_amount(Some(amount), label, usd),
        (Some(amount), false) => format!(
            "{} (partial; unpriced: {omitted})",
            format_optional_amount(Some(amount), label, usd)
        ),
        (None, false) => format!(" · {label} unpriced ({omitted})"),
        (None, true) => format_optional_amount(None, label, usd),
    }
}

fn sanitized_error(error: &anyhow::Error) -> String {
    let text = error.to_string();
    const ALLOWED: &[&str] = &[
        "hook input could not be read",
        "hook input exceeded the safety limit",
        "hook input was not valid JSON",
        "unexpected hook event",
        "invalid session identity",
        "the harness did not provide a transcript",
        "the transcript was not a JSONL file",
        "the transcript path was unavailable",
        "the transcript path was not a file",
        "the transcript is outside the watched roots",
        "the transcript could not be parsed",
        "the transcript did not contain a session",
        "the transcript identity did not match the hook",
        "the requested turn was not found",
        "the completed turn was not found",
        "the rate card could not be loaded",
    ];
    ALLOWED
        .iter()
        .find(|message| text.starts_with(**message))
        .copied()
        .unwrap_or("unexpected local error")
        .to_owned()
}

fn run_status_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("agent-odometer"))
}

fn run_status_path(dir: &Path, harness: &Harness) -> PathBuf {
    if *harness == codex_provider_id() {
        dir.join("turn-receipt-status-codex.json")
    } else if *harness == claude_code_provider_id() {
        dir.join("turn-receipt-status-claude-code.json")
    } else {
        // Neutral fallback: derive a per-provider file name from its id so a
        // future provider gets an independent, collision-free status file.
        dir.join(format!("turn-receipt-status-{}.json", harness.as_str()))
    }
}

fn save_run_record(harness: Harness, record: HookRunRecord) {
    let Some(dir) = run_status_dir() else {
        return;
    };
    save_run_record_in_dir(&dir, harness, &record);
}

fn save_run_record_in_dir(dir: &Path, harness: Harness, record: &HookRunRecord) {
    let path = run_status_path(dir, &harness);
    let Ok(bytes) = serde_json::to_vec_pretty(record) else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    // Each harness owns a separate file, so simultaneous Codex and Claude
    // hooks cannot overwrite each other's read-modify-write snapshot. A
    // process-specific temporary also prevents concurrent same-harness writes
    // from sharing intermediate bytes.
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(&tmp, &path).is_err() {
        let _ = std::fs::copy(&tmp, &path);
        let _ = std::fs::remove_file(tmp);
    }
}

pub fn load_run_record(harness: Harness) -> HookRunRecord {
    let Some(dir) = run_status_dir() else {
        return HookRunRecord::default();
    };
    load_run_record_in_dir(&dir, harness)
}

fn load_run_record_in_dir(dir: &Path, harness: Harness) -> HookRunRecord {
    let current = std::fs::read(run_status_path(dir, &harness))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HookRunRecord>(&bytes).ok());
    if let Some(record) = current {
        return record;
    }

    // Preserve the last observation from v0.6.0 development builds that used
    // one shared file. New writes always use the independent files above.
    let legacy = std::fs::read(dir.join("turn-receipt-status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<HookRunStatusFile>(&bytes).ok())
        .unwrap_or_default();
    if harness == codex_provider_id() {
        legacy.codex
    } else if harness == claude_code_provider_id() {
        legacy.claude_code
    } else {
        // Neutral fallback: no legacy (pre-v0.6.0) status file entry existed
        // for a provider beyond the two builtins.
        HookRunRecord::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{RateLimitSnapshotPoint, TurnStatus};
    use std::collections::{BTreeMap, HashMap};

    fn rate(value: f64) -> ModelRate {
        ModelRate {
            input: value,
            cached_input: value,
            output: value,
            reasoning: value,
        }
    }

    fn fixture() -> (Session, RateCard) {
        let started = "2026-07-25T12:00:00Z".parse().unwrap();
        let completed = "2026-07-25T12:01:00Z".parse().unwrap();
        let tokens = TokenTotals {
            input_tokens: 100_000,
            cached_input_tokens: 80_000,
            output_tokens: 20_000,
            reasoning_output_tokens: 10_000,
            total_tokens: 120_000,
        };
        let turn = TurnInfo {
            turn_id: "turn-7".into(),
            index: 7,
            model: Some("gpt-test".into()),
            service_tier: None,
            status: TurnStatus::Completed,
            started_at: Some(started),
            completed_at: Some(completed),
            tokens: tokens.clone(),
            ..Default::default()
        };
        let window = |used_percent| RateLimitWindow {
            used_percent,
            window_minutes: Some(300),
            resets_at: Some("2026-07-25T17:00:00Z".parse().unwrap()),
        };
        let session = Session {
            id: "session".into(),
            storage_id: "codex:thread:session".into(),
            harness: codex_provider_id(),
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: "fixture.jsonl".into(),
            source_availability: Default::default(),
            archived: false,
            started_at: started,
            last_event_at: completed,
            working_directory: None,
            originator: None,
            source: None,
            subagent_id_is_path_fallback: false,
            history_mode: None,
            memory_mode: None,
            cli_version: None,
            model_provider: None,
            model: Some("gpt-test".into()),
            service_tier: None,
            plan_type: Some("plus".into()),
            credits_unlimited: None,
            credits_balance: None,
            context_window: None,
            latest_context_tokens: None,
            total_turns: 7,
            first_user_message: None,
            tokens_total: tokens.clone(),
            tokens_by_model: HashMap::from([("gpt-test".into(), tokens)]),
            tokens_history: Vec::new(),
            rate_limits_history: vec![
                RateLimitSnapshotPoint {
                    timestamp: started,
                    turn_id: Some("turn-6".into()),
                    limit_id: Some("codex".into()),
                    primary: Some(window(38.17)),
                    secondary: None,
                },
                RateLimitSnapshotPoint {
                    timestamp: completed,
                    turn_id: Some("turn-7".into()),
                    limit_id: Some("codex".into()),
                    primary: Some(window(38.21)),
                    secondary: None,
                },
            ],
            turns: vec![turn],
            tool_observations: Vec::new(),
            tool_metrics: Default::default(),
            tool_metrics_by_model: BTreeMap::new(),
            category_totals: BTreeMap::new(),
            optimization_findings: Vec::new(),
        };
        let rates = RateCard {
            version: 1,
            currency: "credits".into(),
            unit: "per_1m_tokens".into(),
            source_url: String::new(),
            fetched_at: None,
            models: HashMap::from([("gpt-test".into(), rate(1.5))]),
            fallback_model: "gpt-test".into(),
            currencies: HashMap::new(),
            fallback_models: HashMap::new(),
            api_models: HashMap::from([("gpt-test".into(), rate(3.5))]),
            unpriced_models: Vec::new(),
            pricing_catalog: Default::default(),
        };
        (session, rates)
    }

    #[test]
    fn receipt_keeps_granular_observed_quota_delta() {
        let (session, rates) = fixture();
        let receipt = build_receipt(&session, &session.turns[0], &rates);
        assert!(receipt.contains("turn 7 · 120k tokens"));
        assert!(receipt.contains("5h +0.04 pp observed (38.21% used)"));
        assert!(receipt.contains("Session"));
    }

    #[test]
    fn unchanged_quota_is_not_reported_as_zero_consumption() {
        let (mut session, rates) = fixture();
        session.rate_limits_history[1]
            .primary
            .as_mut()
            .unwrap()
            .used_percent = 38.17;
        let receipt = build_receipt(&session, &session.turns[0], &rates);
        assert!(receipt.contains("no measurable change"));
        assert!(!receipt.contains("+0 pp"));
    }

    #[test]
    fn sub_hundredth_quota_delta_keeps_useful_precision() {
        let (mut session, rates) = fixture();
        session.rate_limits_history[1]
            .primary
            .as_mut()
            .unwrap()
            .used_percent = 38.1712;
        let receipt = build_receipt(&session, &session.turns[0], &rates);
        assert!(receipt.contains("+0.0012 pp observed"));
    }

    #[test]
    fn missing_quota_identity_never_produces_an_observed_delta() {
        let (mut session, rates) = fixture();
        for point in &mut session.rate_limits_history {
            point.limit_id = None;
            point.primary.as_mut().unwrap().resets_at = None;
        }
        let receipt = build_receipt(&session, &session.turns[0], &rates);
        assert!(receipt.contains("turn delta unavailable"));
        assert!(!receipt.contains("pp observed"));
    }

    #[test]
    fn session_price_labels_omitted_unpriced_models() {
        let (mut session, mut rates) = fixture();
        session.tokens_by_model.insert(
            "preview-unpriced".into(),
            TokenTotals {
                input_tokens: 50_000,
                total_tokens: 50_000,
                ..Default::default()
            },
        );
        rates.unpriced_models.push("preview-unpriced".into());

        let receipt = build_receipt(&session, &session.turns[0], &rates);
        assert!(receipt.contains("partial; unpriced: preview-unpriced"));
    }

    #[test]
    fn fast_tier_multiplier_matches_frontend_accounting() {
        let tokens = TokenTotals {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        let mut rates = fixture().1;
        rates.models.insert("gpt-5.5".into(), rate(1.0));
        assert_eq!(
            price_tokens(
                &tokens,
                Some("gpt-5.5"),
                Some("fast"),
                &codex_provider_id(),
                &rates,
                false,
            ),
            Some(2.5)
        );
    }

    #[test]
    fn harness_health_records_use_independent_files() {
        let dir = tempfile::tempdir().unwrap();
        let codex_record = HookRunRecord {
            last_run_at: Some("2026-07-25T12:00:00Z".parse().unwrap()),
            success: true,
            last_receipt: Some("codex receipt".into()),
            detail: None,
        };
        let claude_record = HookRunRecord {
            last_run_at: Some("2026-07-25T12:00:01Z".parse().unwrap()),
            success: false,
            last_receipt: None,
            detail: Some("claude detail".into()),
        };

        std::thread::scope(|scope| {
            scope.spawn(|| {
                save_run_record_in_dir(dir.path(), codex_provider_id(), &codex_record);
            });
            scope.spawn(|| {
                save_run_record_in_dir(dir.path(), claude_code_provider_id(), &claude_record);
            });
        });

        let codex = load_run_record_in_dir(dir.path(), codex_provider_id());
        let claude = load_run_record_in_dir(dir.path(), claude_code_provider_id());
        assert_eq!(codex.last_receipt.as_deref(), Some("codex receipt"));
        assert_eq!(claude.detail.as_deref(), Some("claude detail"));
        assert_ne!(
            run_status_path(dir.path(), &codex_provider_id()),
            run_status_path(dir.path(), &claude_code_provider_id())
        );
    }

    #[test]
    fn health_record_reader_accepts_legacy_shared_file() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = HookRunStatusFile {
            codex: HookRunRecord {
                success: true,
                last_receipt: Some("legacy codex".into()),
                ..Default::default()
            },
            claude_code: HookRunRecord {
                success: false,
                detail: Some("legacy claude".into()),
                ..Default::default()
            },
        };
        std::fs::write(
            dir.path().join("turn-receipt-status.json"),
            serde_json::to_vec(&legacy).unwrap(),
        )
        .unwrap();

        assert_eq!(
            load_run_record_in_dir(dir.path(), codex_provider_id())
                .last_receipt
                .as_deref(),
            Some("legacy codex")
        );
        assert_eq!(
            load_run_record_in_dir(dir.path(), claude_code_provider_id())
                .detail
                .as_deref(),
            Some("legacy claude")
        );
    }
}
