use crate::model::{
    CategoryMetric, Harness, OptimizationFinding, TaskCategory, ToolKind, ToolMetrics,
    ToolObservation, ToolOutcome, TurnClassification,
};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

pub const CLASSIFIER_VERSION: u32 = 1;
pub const ANALYZER_VERSION: u32 = 3;
pub const REPEATED_READ_THRESHOLD: usize = 3;
pub const CORRECTIVE_MUTATION_THRESHOLD: usize = 2;
pub const REPEATED_FAILURE_THRESHOLD: usize = 2;
pub const HIGH_TOOL_CHURN_THRESHOLD: usize = 40;
pub const EXCESSIVE_OUTPUT_BYTES_THRESHOLD: u64 = 1024 * 1024;

pub fn classify_tool(name: &str) -> ToolKind {
    let value = name.to_ascii_lowercase();
    if ["read", "get", "open", "view"]
        .iter()
        .any(|part| value.contains(part))
    {
        ToolKind::Read
    } else if ["search", "find", "grep", "query"]
        .iter()
        .any(|part| value.contains(part))
    {
        ToolKind::Search
    } else if ["edit", "write", "patch", "create", "delete", "move"]
        .iter()
        .any(|part| value.contains(part))
    {
        ToolKind::Mutation
    } else if ["shell", "exec", "command", "bash", "powershell"]
        .iter()
        .any(|part| value.contains(part))
    {
        ToolKind::Command
    } else {
        ToolKind::Other
    }
}

fn stable_hash(value: &str) -> String {
    // FNV-1a is intentionally simple and stable across processes. This is an
    // identity key, not a security primitive; raw arguments are never stored.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", hash)
}

fn resource_candidate(arguments: &Value) -> Option<&str> {
    [
        "path",
        "file_path",
        "target",
        "workdir",
        "cwd",
        "uri",
        "ref_id",
        "document_id",
        "spreadsheet_id",
        "command",
        "query",
    ]
    .iter()
    .find_map(|key| arguments.get(*key))
    .and_then(|value| value.as_str())
    .or_else(|| arguments.as_str())
    .filter(|candidate| !candidate.is_empty())
}

pub fn normalized_resource(arguments: &Value) -> Option<String> {
    resource_candidate(arguments).map(stable_hash)
}

pub fn normalized_target(name: &str, arguments: &Value) -> Option<String> {
    let kind = classify_tool(name);
    let resource = normalized_resource(arguments)?;
    let identity = if matches!(kind, ToolKind::Read | ToolKind::Search) {
        // A read's line/page/range selectors are part of the request identity.
        // The separate resource_id still lets a mutation invalidate every read
        // of the underlying private resource.
        let request = serde_json::to_string(arguments).unwrap_or_default();
        stable_hash(&format!("{name}\0{request}"))
    } else {
        resource
    };
    Some(format!("{}:{identity}", kind.as_str()))
}

fn collect_mcp_providers(text: &str, providers: &mut Vec<String>) {
    let mut remaining = text;
    while let Some(start) = remaining.find("mcp__") {
        let candidate = &remaining[start + 5..];
        let Some(end) = candidate.find("__") else {
            break;
        };
        let provider = &candidate[..end];
        if !provider.is_empty()
            && provider.len() <= 64
            && provider
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            let provider = provider.to_ascii_lowercase();
            if !providers.contains(&provider) && providers.len() < 16 {
                providers.push(provider);
            }
        }
        remaining = &candidate[end + 2..];
    }
}

/// Add a bounded, normalized tool identifier without retaining arguments or
/// orchestration source.
fn push_tool_identifier(value: &str, tools: &mut Vec<String>) {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return;
    }
    let value = value.to_ascii_lowercase();
    if !tools.contains(&value) && tools.len() < 32 {
        tools.push(value);
    }
}

fn collect_orchestrated_tools(source: &str, tools: &mut Vec<String>) {
    let mut remaining = source;
    while let Some(start) = remaining.find("tools.") {
        let candidate = &remaining[start + 6..];
        let end = candidate
            .bytes()
            .position(|byte| !(byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')))
            .unwrap_or(candidate.len());
        push_tool_identifier(&candidate[..end], tools);
        remaining = &candidate[end..];
    }
}

/// Returns bounded effective tool identifiers only. For orchestration wrappers,
/// the nested tool names replace the generic wrapper name.
pub fn effective_tools(name: &str, arguments: &Value) -> Vec<String> {
    let mut tools = Vec::new();
    if let Some(source) = arguments
        .get("input")
        .and_then(Value::as_str)
        .or_else(|| arguments.as_str())
    {
        collect_orchestrated_tools(source, &mut tools);
    }
    if tools.is_empty() {
        push_tool_identifier(name, &mut tools);
    }
    tools
}

/// Returns bounded MCP server identifiers only. Raw orchestration source,
/// arguments, paths, and tool output are never retained.
pub fn tool_providers(name: &str, arguments: &Value) -> Vec<String> {
    let mut providers = Vec::new();
    collect_mcp_providers(name, &mut providers);
    if let Some(source) = arguments
        .get("input")
        .and_then(Value::as_str)
        .or_else(|| arguments.as_str())
    {
        collect_mcp_providers(source, &mut providers);
    }
    providers
}

pub struct ToolCallInput<'a> {
    pub call_id: String,
    pub turn_id: Option<String>,
    pub harness: Harness,
    pub model: Option<String>,
    pub timestamp: DateTime<Utc>,
    pub name: String,
    pub arguments: &'a Value,
}

pub fn observe_call(observations: &mut Vec<ToolObservation>, input: ToolCallInput<'_>) {
    // Parsers own call-id deduplication so this hot path stays O(1). Both
    // harness formats repeat records during streaming/resume, and scanning
    // the full observation vector for every call made large sessions O(n²).
    if input.call_id.is_empty() {
        return;
    }
    let target = normalized_target(&input.name, input.arguments);
    let providers = tool_providers(&input.name, input.arguments);
    let effective_tools = effective_tools(&input.name, input.arguments);
    let resource_id = normalized_resource(input.arguments);
    observations.push(ToolObservation {
        call_id: input.call_id,
        turn_id: input.turn_id,
        harness: input.harness,
        model: input.model,
        timestamp: input.timestamp,
        kind: classify_tool(&input.name),
        name: input.name,
        providers,
        effective_tools,
        target,
        resource_id,
        outcome: ToolOutcome::Pending,
        duration_ms: None,
        output_bytes: 0,
    });
}

pub fn observe_result(
    observations: &mut [ToolObservation],
    call_id: &str,
    outcome: ToolOutcome,
    duration_ms: Option<u64>,
    completed_at: Option<DateTime<Utc>>,
    output_bytes: u64,
) {
    if let Some(item) = observations
        .iter_mut()
        .rev()
        .find(|item| item.call_id == call_id)
    {
        item.outcome = outcome;
        let elapsed_ms = completed_at.and_then(|completed| {
            (completed - item.timestamp)
                .num_milliseconds()
                .try_into()
                .ok()
        });
        if duration_ms.is_some() || (item.duration_ms.is_none() && elapsed_ms.is_some()) {
            item.duration_ms = duration_ms.or(elapsed_ms);
        }
        item.output_bytes = item.output_bytes.max(output_bytes);
    }
}

#[derive(Default)]
struct MetricsAccumulator<'a> {
    out: ToolMetrics,
    mutations: HashMap<(Option<&'a str>, Option<&'a str>), u64>,
}

impl<'a> MetricsAccumulator<'a> {
    fn push(&mut self, item: &'a ToolObservation) {
        self.out.calls += 1;
        self.out.output_bytes += item.output_bytes;
        self.out.duration_ms += item.duration_ms.unwrap_or(0);
        match item.kind {
            ToolKind::Read => self.out.reads += 1,
            ToolKind::Search => self.out.searches += 1,
            ToolKind::Mutation => {
                self.out.mutations += 1;
                *self
                    .mutations
                    .entry((item.turn_id.as_deref(), item.target.as_deref()))
                    .or_default() += 1;
            }
            ToolKind::Command => self.out.commands += 1,
            ToolKind::Other => self.out.other += 1,
        }
        match item.outcome {
            ToolOutcome::Success => self.out.successes += 1,
            ToolOutcome::Failure => self.out.failures += 1,
            ToolOutcome::Pending | ToolOutcome::Unknown => self.out.unknown += 1,
        }
    }

    fn finish(mut self) -> ToolMetrics {
        self.out.mutation_targets = self.mutations.len() as u64;
        self.out.one_shot_mutations =
            self.mutations.values().filter(|count| **count == 1).count() as u64;
        self.out.retry_count = self
            .mutations
            .values()
            .map(|count| count.saturating_sub(1))
            .sum();
        self.out
    }
}

pub fn metrics<'a>(observations: impl Iterator<Item = &'a ToolObservation>) -> ToolMetrics {
    let mut accumulator = MetricsAccumulator::default();
    for item in observations {
        accumulator.push(item);
    }
    accumulator.finish()
}

pub fn metrics_with_models<'a>(
    observations: impl Iterator<Item = &'a ToolObservation>,
) -> (ToolMetrics, BTreeMap<String, ToolMetrics>) {
    let mut all = MetricsAccumulator::default();
    let mut by_model: BTreeMap<&'a str, MetricsAccumulator<'a>> = BTreeMap::new();
    for item in observations {
        all.push(item);
        if let Some(model) = item.model.as_deref() {
            by_model.entry(model).or_default().push(item);
        }
    }
    (
        all.finish(),
        by_model
            .into_iter()
            .map(|(model, accumulator)| (model.to_owned(), accumulator.finish()))
            .collect(),
    )
}

fn contains_any(text: &str, terms: &[&str]) -> bool {
    terms.iter().any(|term| text.contains(term))
}

pub fn classify_turn(text: &str, tool: &ToolMetrics) -> TurnClassification {
    let value = text.to_ascii_lowercase();
    let (category, confidence, signals) =
        if contains_any(&value, &["test", "clippy", "lint", "validate", "verify"])
            || (tool.commands > 0 && tool.failures == 0 && contains_any(&value, &["pass", "check"]))
        {
            (TaskCategory::Testing, 0.9, vec!["testing-keyword"])
        } else if contains_any(&value, &["debug", "bug", "error", "fail", "diagnos", "fix"])
            || tool.failures > 0
        {
            (TaskCategory::Debugging, 0.85, vec!["debug-signal"])
        } else if contains_any(&value, &["review", "audit", "inspect", "assess"]) {
            (TaskCategory::Review, 0.85, vec!["review-keyword"])
        } else if contains_any(&value, &["plan", "design", "architect", "approach"]) {
            (TaskCategory::Planning, 0.8, vec!["planning-keyword"])
        } else if tool.mutations > 0
            || contains_any(&value, &["implement", "add", "build", "refactor", "code"])
        {
            (TaskCategory::Coding, 0.8, vec!["mutation-or-coding-signal"])
        } else if tool.reads + tool.searches > 0
            || contains_any(&value, &["explore", "find", "research", "understand"])
        {
            (
                TaskCategory::Exploration,
                0.75,
                vec!["read-or-search-signal"],
            )
        } else {
            (TaskCategory::Other, 0.3, vec!["no-strong-signal"])
        };
    TurnClassification {
        version: CLASSIFIER_VERSION,
        category,
        confidence,
        signals: signals.into_iter().map(str::to_owned).collect(),
    }
}

#[derive(Default)]
struct ReadStreak {
    current: usize,
    max: usize,
    timestamp: Option<DateTime<Utc>>,
    model: Option<String>,
    tool_name: String,
}

pub fn findings<'a>(
    observations: impl IntoIterator<Item = &'a ToolObservation>,
) -> Vec<OptimizationFinding> {
    let mut groups: BTreeMap<(String, String, String), Vec<&ToolObservation>> = BTreeMap::new();
    let mut target_activity: BTreeMap<(String, String), Vec<&ToolObservation>> = BTreeMap::new();
    let mut turn_calls: BTreeMap<String, Vec<&ToolObservation>> = BTreeMap::new();
    for item in observations {
        let turn = item.turn_id.clone().unwrap_or_else(|| "session".into());
        turn_calls.entry(turn.clone()).or_default().push(item);
        if let Some(target) = &item.target {
            let resource = item
                .resource_id
                .clone()
                .or_else(|| target.rsplit_once(':').map(|(_, hash)| hash.to_owned()))
                .unwrap_or_else(|| target.clone());
            target_activity
                .entry((turn.clone(), resource))
                .or_default()
                .push(item);
            groups
                .entry((turn, item.kind.as_str().into(), target.clone()))
                .or_default()
                .push(item);
        }
    }
    let mut out = Vec::new();

    // Re-reads are actionable only when the exact same successful request
    // occurs in one uninterrupted segment. Different line/page selectors are
    // different requests. A mutation of the private resource resets every
    // request streak; reads in later turns are evaluated independently.
    for ((turn, resource), mut items) in target_activity {
        items.sort_by_key(|item| item.timestamp);
        let mut streaks: BTreeMap<String, ReadStreak> = BTreeMap::new();
        for item in &items {
            match item.kind {
                ToolKind::Read
                    if item.outcome == ToolOutcome::Success && !is_volatile_read(&item.name) =>
                {
                    if let Some(target) = &item.target {
                        let entry = streaks.entry(target.clone()).or_insert_with(|| ReadStreak {
                            tool_name: item.name.clone(),
                            ..Default::default()
                        });
                        entry.current += 1;
                        if entry.current > entry.max {
                            entry.max = entry.current;
                            entry.timestamp = Some(item.timestamp);
                            entry.model = item.model.clone();
                            entry.tool_name = item.name.clone();
                        }
                    }
                }
                ToolKind::Mutation => {
                    for streak in streaks.values_mut() {
                        streak.current = 0;
                    }
                }
                _ => {}
            }
        }
        for (_, streak) in streaks {
            if streak.max >= REPEATED_READ_THRESHOLD {
                out.push(OptimizationFinding {
                    version: ANALYZER_VERSION,
                    rule_id: "repeated-read".into(),
                    severity: "info".into(),
                    confidence: "high".into(),
                    turn_id: (turn != "session").then_some(turn.clone()),
                    model: streak.model,
                    timestamp: streak.timestamp,
                    evidence: format!(
                        "{} identical successful {} requests to private target #{}",
                        streak.max,
                        streak.tool_name,
                        short_private_id(&resource)
                    ),
                    remediation:
                        "Reuse the prior result until the resource changes, or request the needed ranges together"
                            .into(),
                    occurrences: streak.max as u64,
                    avoidable_calls: streak.max.saturating_sub(1) as u64,
                });
            }
        }
    }

    for ((turn, kind, target), items) in groups {
        let failures = items
            .iter()
            .filter(|item| item.outcome == ToolOutcome::Failure)
            .count();
        let rule = if kind == "mutation"
            && items.len() >= CORRECTIVE_MUTATION_THRESHOLD
            && failures > 0
        {
            Some((
                "corrective-mutation",
                "warning",
                "Inspect the first mutation error and validate the intended change before retrying",
            ))
        } else if failures >= REPEATED_FAILURE_THRESHOLD {
            Some((
                "repeated-failure",
                "warning",
                "Stop retrying unchanged input; inspect the first error, adjust the tool request, then retry once",
                ))
        } else {
            None
        };
        if let Some((rule_id, severity, message)) = rule {
            let resource = items
                .last()
                .and_then(|item| item.resource_id.as_deref())
                .unwrap_or_else(|| {
                    target
                        .rsplit_once(':')
                        .map_or(target.as_str(), |(_, id)| id)
                });
            out.push(OptimizationFinding {
                version: ANALYZER_VERSION,
                rule_id: rule_id.into(),
                severity: severity.into(),
                confidence: "high".into(),
                turn_id: (turn != "session").then_some(turn),
                model: items.last().and_then(|item| item.model.clone()),
                timestamp: items.last().map(|item| item.timestamp),
                evidence: format!(
                    "{} {} attempts to private target #{}; {failures} failed",
                    items.len(),
                    items.last().map_or("tool", |item| item.name.as_str()),
                    short_private_id(resource)
                ),
                remediation: message.into(),
                occurrences: items.len() as u64,
                avoidable_calls: failures.saturating_sub(1) as u64,
            });
        }
    }
    for (turn, items) in turn_calls {
        let metrics = metrics(items.iter().copied());
        if items.len() > HIGH_TOOL_CHURN_THRESHOLD
            && (metrics.retry_count >= 4 || metrics.failures >= 4)
        {
            out.push(OptimizationFinding {
                version: ANALYZER_VERSION,
                rule_id: "high-tool-churn".into(),
                severity: "info".into(),
                confidence: "medium".into(),
                turn_id: (turn != "session").then_some(turn.clone()),
                model: items.last().and_then(|item| item.model.clone()),
                timestamp: items.last().map(|item| item.timestamp),
                evidence: format!(
                    "{} tool calls with {} retries and {} failures",
                    items.len(),
                    metrics.retry_count,
                    metrics.failures
                ),
                remediation:
                    "Review the failing or retried path first, then batch compatible independent calls"
                        .into(),
                occurrences: items.len() as u64,
                // This turn-wide signal overlaps target-specific retry findings;
                // keep it out of the avoidable-call total to prevent double counting.
                avoidable_calls: 0,
            });
        }
        let command_output_bytes: u64 = items
            .iter()
            .filter(|item| item.kind == ToolKind::Command)
            .map(|item| item.output_bytes)
            .sum();
        let command_calls = items
            .iter()
            .filter(|item| item.kind == ToolKind::Command)
            .count();
        if command_output_bytes > EXCESSIVE_OUTPUT_BYTES_THRESHOLD {
            out.push(OptimizationFinding {
                version: ANALYZER_VERSION,
                rule_id: "excessive-command-output".into(),
                severity: "warning".into(),
                confidence: "high".into(),
                turn_id: (turn != "session").then_some(turn.clone()),
                model: items.last().and_then(|item| item.model.clone()),
                timestamp: items.last().map(|item| item.timestamp),
                evidence: format!(
                    "{:.1} MiB captured across {command_calls} command calls",
                    command_output_bytes as f64 / (1024.0 * 1024.0)
                ),
                remediation: "Filter, paginate, or summarize command output at the source".into(),
                occurrences: command_calls as u64,
                avoidable_calls: 0,
            });
        }
    }
    out.sort_by(|a, b| {
        (b.severity == "warning")
            .cmp(&(a.severity == "warning"))
            .then_with(|| b.avoidable_calls.cmp(&a.avoidable_calls))
            .then_with(|| a.timestamp.cmp(&b.timestamp))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    out
}

fn is_volatile_read(name: &str) -> bool {
    let value = name.to_ascii_lowercase();
    [
        "status",
        "wait",
        "poll",
        "terminal",
        "get_goal",
        "list_agents",
        "read_thread",
    ]
    .iter()
    .any(|term| value.contains(term))
}

fn short_private_id(value: &str) -> String {
    let tail: String = value.chars().rev().take(8).collect();
    tail.chars().rev().collect()
}

pub fn category_totals(turns: &[crate::model::TurnInfo]) -> BTreeMap<TaskCategory, CategoryMetric> {
    let mut out = BTreeMap::new();
    for turn in turns {
        let category = turn.classification.category;
        let entry: &mut CategoryMetric = out.entry(category).or_default();
        entry.turns += 1;
        entry.tokens.input_tokens += turn.tokens.input_tokens;
        entry.tokens.cached_input_tokens += turn.tokens.cached_input_tokens;
        entry.tokens.output_tokens += turn.tokens.output_tokens;
        entry.tokens.reasoning_output_tokens += turn.tokens.reasoning_output_tokens;
        entry.tokens.total_tokens += turn.tokens.total_tokens;
        entry.tool_calls += turn.tool_metrics.calls;
        if let Some(model) = &turn.model {
            let bucket = entry
                .buckets
                .iter_mut()
                .find(|bucket| bucket.model == *model && bucket.service_tier == turn.service_tier);
            match bucket {
                Some(bucket) => {
                    bucket.tokens.input_tokens += turn.tokens.input_tokens;
                    bucket.tokens.cached_input_tokens += turn.tokens.cached_input_tokens;
                    bucket.tokens.output_tokens += turn.tokens.output_tokens;
                    bucket.tokens.reasoning_output_tokens += turn.tokens.reasoning_output_tokens;
                    bucket.tokens.total_tokens += turn.tokens.total_tokens;
                }
                None => entry.buckets.push(crate::model::TierBucket {
                    model: model.clone(),
                    service_tier: turn.service_tier.clone(),
                    tokens: turn.tokens.clone(),
                }),
            }
        }
    }
    for metric in out.values_mut() {
        metric.buckets.sort_by(|a, b| {
            a.model
                .cmp(&b.model)
                .then_with(|| a.service_tier.cmp(&b.service_tier))
        });
    }
    out
}

pub fn refresh_session(session: &mut crate::model::Session) {
    let mut all = MetricsAccumulator::default();
    let mut by_model: BTreeMap<&str, MetricsAccumulator<'_>> = BTreeMap::new();
    let mut by_turn: HashMap<&str, MetricsAccumulator<'_>> = HashMap::new();
    for observation in &session.tool_observations {
        all.push(observation);
        if let Some(model) = observation.model.as_deref() {
            by_model.entry(model).or_default().push(observation);
        }
        if let Some(turn_id) = observation.turn_id.as_deref() {
            by_turn.entry(turn_id).or_default().push(observation);
        }
    }
    session.tool_metrics = all.finish();
    session.tool_metrics_by_model.clear();
    for (model, accumulator) in by_model {
        session
            .tool_metrics_by_model
            .insert(model.to_owned(), accumulator.finish());
    }
    for turn in &mut session.turns {
        turn.tool_metrics = by_turn
            .remove(turn.turn_id.as_str())
            .map(MetricsAccumulator::finish)
            .unwrap_or_default();
        let text = format!(
            "{} {}",
            turn.user_message.as_deref().unwrap_or(""),
            turn.last_agent_message.as_deref().unwrap_or("")
        );
        turn.classification = classify_turn(&text, &turn.tool_metrics);
    }
    session.category_totals = category_totals(&session.turns);
    session.optimization_findings = findings(&session.tool_observations);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::codex_provider_id;

    #[test]
    fn target_identity_is_stable_and_does_not_retain_arguments() {
        let args = serde_json::json!({"path":"C:/private/example.rs","content":"secret"});
        let target = normalized_target("edit_file", &args).unwrap();
        assert_eq!(target, normalized_target("edit_file", &args).unwrap());
        assert!(!target.contains("private"));
        assert!(!target.contains("secret"));
        assert_eq!(normalized_resource(&args), normalized_resource(&args));
    }

    #[test]
    fn read_ranges_have_distinct_request_ids_but_share_a_resource() {
        let first = serde_json::json!({"path":"C:/private/example.rs","offset":1,"limit":50});
        let second = serde_json::json!({"path":"C:/private/example.rs","offset":51,"limit":50});
        assert_ne!(
            normalized_target("read_file", &first),
            normalized_target("read_file", &second)
        );
        assert_eq!(normalized_resource(&first), normalized_resource(&second));
    }

    #[test]
    fn mutation_metrics_define_one_shot_and_retry_counts() {
        let ts = Utc::now();
        let make = |id: &str, target: &str| ToolObservation {
            call_id: id.into(),
            turn_id: Some("t".into()),
            harness: codex_provider_id(),
            model: Some("m".into()),
            timestamp: ts,
            kind: ToolKind::Mutation,
            name: "edit".into(),
            providers: Vec::new(),
            effective_tools: vec!["edit".into()],
            target: Some(target.into()),
            resource_id: Some(target.into()),
            outcome: ToolOutcome::Success,
            duration_ms: None,
            output_bytes: 0,
        };
        let items = [make("1", "a"), make("2", "b"), make("3", "b")];
        let result = metrics(items.iter());
        assert_eq!(result.mutation_targets, 2);
        assert_eq!(result.one_shot_mutations, 1);
        assert_eq!(result.retry_count, 1);
    }

    #[test]
    fn analyzer_suppresses_duplicate_findings_per_rule_target_turn() {
        let ts = Utc::now();
        let items: Vec<_> = (0..4)
            .map(|i| ToolObservation {
                call_id: i.to_string(),
                turn_id: Some("t".into()),
                harness: codex_provider_id(),
                model: None,
                timestamp: ts,
                kind: ToolKind::Read,
                name: "read".into(),
                providers: Vec::new(),
                effective_tools: vec!["read".into()],
                target: Some("read:abc".into()),
                resource_id: Some("abc".into()),
                outcome: ToolOutcome::Success,
                duration_ms: None,
                output_bytes: 0,
            })
            .collect();
        let result = findings(&items);
        assert_eq!(
            result
                .iter()
                .filter(|f| f.rule_id == "repeated-read")
                .count(),
            1
        );
    }

    #[test]
    fn classifier_is_ordered_deterministic_and_handles_ambiguous_text() {
        let none = ToolMetrics::default();
        assert_eq!(
            classify_turn("write tests and fix a bug", &none).category,
            TaskCategory::Testing
        );
        assert_eq!(
            classify_turn("fix a bug after review", &none).category,
            TaskCategory::Debugging
        );
        assert_eq!(
            classify_turn("review the architecture plan", &none).category,
            TaskCategory::Review
        );
        assert_eq!(
            classify_turn("design an approach", &none).category,
            TaskCategory::Planning
        );
        assert_eq!(
            classify_turn("implement the feature", &none).category,
            TaskCategory::Coding
        );
        assert_eq!(
            classify_turn("research the current flow", &none).category,
            TaskCategory::Exploration
        );
        let unknown = classify_turn("hello", &none);
        assert_eq!(unknown.category, TaskCategory::Other);
        assert_eq!(unknown.version, CLASSIFIER_VERSION);
        assert_eq!(unknown.signals, vec!["no-strong-signal"]);

        let mut tools = ToolMetrics {
            mutations: 1,
            ..Default::default()
        };
        assert_eq!(
            classify_turn("continue", &tools).category,
            TaskCategory::Coding
        );
        tools.failures = 1;
        assert_eq!(
            classify_turn("continue", &tools).category,
            TaskCategory::Debugging
        );
    }

    fn observation(
        id: usize,
        kind: ToolKind,
        target: Option<&str>,
        outcome: ToolOutcome,
        output_bytes: u64,
    ) -> ToolObservation {
        ToolObservation {
            call_id: id.to_string(),
            turn_id: Some("t".into()),
            harness: codex_provider_id(),
            model: Some("m".into()),
            timestamp: DateTime::from_timestamp(id as i64, 0).unwrap(),
            kind,
            name: "synthetic".into(),
            providers: Vec::new(),
            effective_tools: vec!["synthetic".into()],
            target: target.map(str::to_owned),
            resource_id: target.map(|target| {
                target
                    .rsplit_once(':')
                    .map_or(target, |(_, resource)| resource)
                    .to_owned()
            }),
            outcome,
            duration_ms: None,
            output_bytes,
        }
    }

    #[test]
    fn analyzer_resets_repeated_reads_after_a_mutation() {
        let items = vec![
            observation(1, ToolKind::Read, Some("read:a"), ToolOutcome::Success, 0),
            observation(2, ToolKind::Read, Some("read:a"), ToolOutcome::Success, 0),
            observation(
                3,
                ToolKind::Mutation,
                Some("read:a"),
                ToolOutcome::Success,
                0,
            ),
            observation(4, ToolKind::Read, Some("read:a"), ToolOutcome::Success, 0),
            observation(5, ToolKind::Read, Some("read:a"), ToolOutcome::Success, 0),
        ];
        assert!(!findings(&items)
            .iter()
            .any(|item| item.rule_id == "repeated-read"));
    }

    #[test]
    fn analyzer_emits_each_initial_rule_with_bounded_evidence() {
        let mut items = vec![
            observation(
                1,
                ToolKind::Mutation,
                Some("mutation:a"),
                ToolOutcome::Success,
                0,
            ),
            observation(
                2,
                ToolKind::Mutation,
                Some("mutation:a"),
                ToolOutcome::Failure,
                0,
            ),
            observation(
                3,
                ToolKind::Command,
                Some("command:b"),
                ToolOutcome::Failure,
                700_000,
            ),
            observation(
                4,
                ToolKind::Command,
                Some("command:b"),
                ToolOutcome::Failure,
                700_000,
            ),
        ];
        for id in 5..=9 {
            items.push(observation(
                id,
                ToolKind::Other,
                None,
                ToolOutcome::Success,
                0,
            ));
        }
        let result = findings(&items);
        for rule in [
            "corrective-mutation",
            "repeated-failure",
            "excessive-command-output",
        ] {
            assert!(
                result.iter().any(|item| item.rule_id == rule),
                "missing {rule}"
            );
        }
        assert!(result.iter().all(|item| item.evidence.len() < 160));
    }

    #[test]
    fn classifies_tools_and_hashes_targets() {
        assert_eq!(classify_tool("Read"), ToolKind::Read);
    }

    #[test]
    fn extracts_providers_and_effective_tools_from_direct_and_orchestrated_calls() {
        let direct_name = "mcp__alpha_server__code_search";
        assert_eq!(
            tool_providers(direct_name, &Value::Null),
            vec!["alpha_server"]
        );
        assert_eq!(
            effective_tools(direct_name, &Value::Null),
            vec!["mcp__alpha_server__code_search"]
        );

        let arguments = serde_json::json!({
            "input": "await tools.mcp__alpha_server__code_search({}); await tools.exec_command({}); await tools.mcp__beta__lookup({});"
        });
        assert_eq!(
            tool_providers("exec", &arguments),
            vec!["alpha_server", "beta"]
        );
        assert_eq!(
            effective_tools("exec", &arguments),
            vec![
                "mcp__alpha_server__code_search",
                "exec_command",
                "mcp__beta__lookup"
            ]
        );
    }

    #[test]
    fn analyzer_does_not_treat_successful_iterative_edits_as_corrections() {
        let items = vec![
            observation(
                1,
                ToolKind::Mutation,
                Some("mutation:a"),
                ToolOutcome::Success,
                0,
            ),
            observation(
                2,
                ToolKind::Mutation,
                Some("mutation:a"),
                ToolOutcome::Success,
                0,
            ),
        ];
        assert!(!findings(&items)
            .iter()
            .any(|item| item.rule_id == "corrective-mutation"));
    }

    #[test]
    fn analyzer_excludes_volatile_reads_and_neutral_ratios() {
        let volatile: Vec<_> = (1..=8)
            .map(|id| {
                let mut item = observation(
                    id,
                    ToolKind::Read,
                    Some("read:status"),
                    ToolOutcome::Success,
                    0,
                );
                item.name = "read_thread_terminal".into();
                item
            })
            .collect();
        assert!(findings(&volatile).is_empty());
    }
}
