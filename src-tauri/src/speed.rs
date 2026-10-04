//! Turn and response throughput reports from already parsed Codex usage data.

use chrono::{DateTime, Duration, SecondsFormat, Utc};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration as StdDuration, Instant};

use crate::model::{TokenHistoryPoint, TurnInfo, TurnStatus};

const MAX_RANGE: Duration = Duration::days(15);
const SOURCE_WINDOW: Duration = Duration::days(1);
const MAX_LOG_ROWS: usize = 100_000;
const MAX_SAMPLES: usize = 5_000;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const QUERY_TIMEOUT: StdDuration = StdDuration::from_secs(3);
const MAX_TURN_SESSIONS: usize = 500;
const TURN_SESSION_BATCH: usize = 8;
const MAX_TURNS: usize = 10_000;
const MAX_TURN_SAMPLES: usize = 5_000;
const TURN_QUERY_TIMEOUT: StdDuration = StdDuration::from_secs(5);
const MAX_RESPONSE_ID_BYTES: usize = 256;

#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SpeedMeasurement {
    #[default]
    Turn,
    Response,
}

impl SpeedMeasurement {
    fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Response => "response",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SpeedQuery {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub measurement: SpeedMeasurement,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedReport {
    pub status: &'static str,
    pub reason: Option<&'static str>,
    pub measurement: &'static str,
    pub source: &'static str,
    pub generated_at: String,
    pub rows: Vec<SpeedSample>,
    pub excluded_count: u64,
    pub scanned_rows: u64,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SpeedSample {
    pub completed_at: String,
    pub model: String,
    pub reasoning_effort: Option<String>,
    pub time_to_first_token_ms: Option<u64>,
    pub timing_source: &'static str,
    pub mode: &'static str,
    pub output_tokens: u64,
    pub reasoning_tokens: Option<u64>,
    pub duration_ms: f64,
    pub output_tps: f64,
    pub visible_tps: Option<f64>,
}

#[derive(Debug)]
struct LogRow {
    body: String,
    body_too_large: bool,
}

#[derive(Default)]
struct TurnCandidateSelector {
    newest: BinaryHeap<Reverse<(DateTime<Utc>, String)>>,
    discarded: bool,
}

impl TurnCandidateSelector {
    fn push(&mut self, id: String, last_event_at: DateTime<Utc>) {
        self.newest.push(Reverse((last_event_at, id)));
        if self.newest.len() > MAX_TURN_SESSIONS {
            self.newest.pop();
            self.discarded = true;
        }
    }

    fn into_newest_first(self) -> Vec<(String, DateTime<Utc>)> {
        let mut candidates: Vec<_> = self
            .newest
            .into_iter()
            .map(|Reverse((last_event_at, id))| (id, last_event_at))
            .collect();
        candidates.sort_by_key(|(id, last_event_at)| Reverse((*last_event_at, id.clone())));
        candidates
    }
}

pub fn report(query: SpeedQuery, state: &crate::store::AppState) -> SpeedReport {
    let Some((from, to)) = parse_range(&query) else {
        return unavailable(
            query.measurement,
            "Enter a valid date range of 15 days or less.",
        );
    };
    match query.measurement {
        SpeedMeasurement::Turn => report_turns(state, from, to),
        SpeedMeasurement::Response => report_from_path(
            &crate::config::codex_home_dir().join("logs_2.sqlite"),
            from,
            to,
        ),
    }
}

fn report_from_path(path: &Path, from: DateTime<Utc>, to: DateTime<Utc>) -> SpeedReport {
    if !path.is_file() {
        return unavailable(
            SpeedMeasurement::Response,
            "Codex local logs are unavailable.",
        );
    }

    let conn = match Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) {
        Ok(conn) => conn,
        Err(_) => {
            return unavailable(
                SpeedMeasurement::Response,
                "Codex local logs are unavailable.",
            )
        }
    };
    let _ = conn.busy_timeout(StdDuration::from_millis(250));

    if !has_supported_schema(&conn) {
        return unavailable(
            SpeedMeasurement::Response,
            "The Codex local log schema is not supported.",
        );
    }

    let query_started = Instant::now();
    let deadline = query_started + QUERY_TIMEOUT;
    if conn
        .progress_handler(1000, Some(move || Instant::now() >= deadline))
        .is_err()
    {
        return unavailable(
            SpeedMeasurement::Response,
            "The Codex log query limit could not be enforced.",
        );
    }
    let lower = (from - SOURCE_WINDOW).timestamp();
    let upper = (to + SOURCE_WINDOW).timestamp();
    let sql = format!(
        "SELECT CASE WHEN length(CAST(feedback_log_body AS BLOB)) > {} THEN NULL ELSE feedback_log_body END, \
                length(CAST(feedback_log_body AS BLOB)) > {} \
           FROM (SELECT id, ts, feedback_log_body FROM logs \
                 WHERE ts >= ?1 AND ts <= ?2 \
                   AND feedback_log_body LIKE '%websocket event:%' \
                 ORDER BY id DESC LIMIT {}) ORDER BY id ASC",
        MAX_BODY_BYTES,
        MAX_BODY_BYTES,
        MAX_LOG_ROWS
    );
    let mut statement = match conn.prepare(&sql) {
        Ok(statement) => statement,
        Err(_) => {
            return unavailable(
                SpeedMeasurement::Response,
                "The Codex local log schema is not supported.",
            )
        }
    };
    let rows = match statement.query_map([lower, upper], |row| {
        Ok(LogRow {
            body: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
            body_too_large: row.get(1)?,
        })
    }) {
        Ok(rows) => rows,
        Err(_) => {
            return unavailable(
                SpeedMeasurement::Response,
                "Codex local logs could not be read.",
            )
        }
    };
    let mut by_response: HashMap<String, Option<SpeedSample>> = HashMap::new();
    let mut excluded_count = 0u64;
    let mut scanned_rows = 0u64;
    let mut truncated = false;

    for row in rows {
        if Instant::now() >= deadline {
            truncated = true;
            break;
        }
        let row = match row {
            Ok(row) => row,
            Err(_) => {
                return unavailable(
                    SpeedMeasurement::Response,
                    "Codex local logs could not be read.",
                )
            }
        };
        scanned_rows += 1;
        if scanned_rows >= MAX_LOG_ROWS as u64 {
            truncated = true;
        }
        if row.body_too_large {
            truncated = true;
            excluded_count += 1;
            continue;
        }
        let fields = parse_prefix_fields(&row.body);

        let Some(event) = parse_embedded_json(&row.body, "websocket event:") else {
            if row.body.contains("websocket event:") {
                excluded_count += 1;
            }
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("response.completed") {
            continue;
        }
        let response = event.get("response").unwrap_or(&event);
        if response
            .get("status")
            .and_then(Value::as_str)
            .is_some_and(|status| status != "completed")
        {
            excluded_count += 1;
            continue;
        }
        let response_id = response.get("id").and_then(Value::as_str).unwrap_or("");
        if response_id.is_empty() || response_id.len() > MAX_RESPONSE_ID_BYTES {
            excluded_count += 1;
            continue;
        }
        let sample = parse_sample(response, &fields, from, to);
        if sample.is_none() {
            excluded_count += 1;
        }
        // Rows are ordered by source log id, so replacement gives latest-log-wins semantics.
        by_response.insert(response_id.to_owned(), sample);
    }
    let _ = conn.progress_handler(0, None::<fn() -> bool>);

    let mut samples: Vec<_> = by_response.into_values().flatten().collect();
    samples.sort_by(|a, b| a.completed_at.cmp(&b.completed_at));
    let sample_truncated = samples.len() > MAX_SAMPLES;
    if sample_truncated {
        samples.drain(..samples.len() - MAX_SAMPLES);
    }
    SpeedReport {
        status: "ready",
        reason: if samples.is_empty() {
            Some("No eligible response timings were found in retained Codex logs. This Codex version or logging configuration may not record completed-response telemetry.")
        } else {
            None
        },
        measurement: "response",
        source: "codex_local_logs",
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        rows: samples,
        excluded_count,
        scanned_rows,
        truncated: truncated || sample_truncated,
    }
}

fn report_turns(
    state: &crate::store::AppState,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> SpeedReport {
    if state.history_readiness() == crate::store::HistoryReadinessKind::Pending {
        return unavailable(
            SpeedMeasurement::Turn,
            "Codex session history is still preparing.",
        );
    }

    // Session summaries are small. Load full parsed sessions from durable history
    // in bounded batches so the report never holds the whole corpus in memory.
    let started = Instant::now();
    let mut selector = TurnCandidateSelector::default();
    let mut candidate_scan_timed_out = false;
    for entry in state.sessions.iter() {
        if started.elapsed() >= TURN_QUERY_TIMEOUT {
            candidate_scan_timed_out = true;
            break;
        }
        let summary = &entry.summary;
        if summary.harness.as_str() != "codex"
            || summary.started_at > to
            || summary.last_event_at < from
        {
            continue;
        }
        selector.push(entry.key().clone(), summary.last_event_at);
    }
    let truncated_by_session_limit = selector.discarded;
    let candidates = selector.into_newest_first();

    let scan_complete = state.scanned.load(Ordering::Acquire);
    let mut truncated = !scan_complete || truncated_by_session_limit || candidate_scan_timed_out;

    let mut scanned_rows = 0u64;
    let mut excluded_count = 0u64;
    let mut samples = Vec::new();
    'batches: for batch in candidates.chunks(TURN_SESSION_BATCH) {
        if started.elapsed() >= TURN_QUERY_TIMEOUT {
            truncated = true;
            break;
        }
        let ids: Vec<_> = batch.iter().map(|(id, _)| id.clone()).collect();
        let sessions = match state.full_sessions(&ids) {
            Ok(sessions) => sessions,
            Err(_) => {
                return unavailable(
                    SpeedMeasurement::Turn,
                    "Codex session history could not be loaded completely.",
                )
            }
        };
        for session in sessions {
            for turn in &session.turns {
                if started.elapsed() >= TURN_QUERY_TIMEOUT {
                    truncated = true;
                    break 'batches;
                }
                if scanned_rows >= MAX_TURNS as u64 {
                    truncated = true;
                    break 'batches;
                }
                scanned_rows += 1;
                if turn
                    .completed_at
                    .is_some_and(|completed| completed >= from && completed <= to)
                {
                    match turn_sample(turn, &session.tokens_history, from, to) {
                        Some(sample) => samples.push(sample),
                        None => excluded_count += 1,
                    }
                }
            }
        }
    }

    samples.sort_by(|left, right| left.completed_at.cmp(&right.completed_at));
    let sample_truncated = samples.len() > MAX_TURN_SAMPLES;
    if sample_truncated {
        samples.drain(..samples.len() - MAX_TURN_SAMPLES);
    }
    SpeedReport {
        status: "ready",
        reason: if samples.is_empty() && truncated {
            Some("Codex session scanning is still in progress or the report reached a safety limit; turn results may be incomplete.")
        } else if samples.is_empty() {
            Some("No eligible completed Codex turns were found in session history for this period.")
        } else {
            None
        },
        measurement: "turn",
        source: "codex_session_logs",
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        rows: samples,
        excluded_count,
        scanned_rows,
        truncated: truncated || sample_truncated,
    }
}

fn turn_sample(
    turn: &TurnInfo,
    token_history: &[TokenHistoryPoint],
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Option<SpeedSample> {
    if turn.status != TurnStatus::Completed {
        return None;
    }
    let completed_at = turn.completed_at?;
    if completed_at < from || completed_at > to || turn.tokens.output_tokens == 0 {
        return None;
    }

    let (duration_ms, timing_source) = match turn.duration_ms {
        Some(duration) => (duration, "explicit"),
        None => {
            let start = turn.started_at?;
            let duration = completed_at.signed_duration_since(start).num_milliseconds();
            if duration < 0 {
                return None;
            }
            (duration as u64, "timestamps")
        }
    };
    if duration_ms == 0 {
        return None;
    }
    let duration_ms_f64 = duration_ms as f64;
    let output_tps = turn.tokens.output_tokens as f64 / (duration_ms_f64 / 1000.0);
    if !output_tps.is_finite() {
        return None;
    }

    let model = turn_model_label(token_history, turn);
    let mode = turn_mode_label(token_history, turn);
    Some(SpeedSample {
        completed_at: completed_at.to_rfc3339_opts(SecondsFormat::Millis, true),
        model,
        reasoning_effort: turn.reasoning_effort.clone(),
        time_to_first_token_ms: turn
            .time_to_first_token_ms
            .filter(|time_to_first| *time_to_first <= duration_ms),
        timing_source,
        mode,
        output_tokens: turn.tokens.output_tokens,
        // Session history does not prove complete reasoning-token coverage for
        // every retained turn, so visible throughput stays unknown here.
        reasoning_tokens: None,
        duration_ms: duration_ms_f64,
        output_tps,
        visible_tps: None,
    })
}

fn turn_model_label(token_history: &[TokenHistoryPoint], turn: &TurnInfo) -> String {
    let Some((started_at, attribution_end)) = token_history_bounds(turn) else {
        return "Unknown model".to_owned();
    };
    let mut output_by_model = HashMap::<String, u64>::new();
    let mut unattributed = false;
    let mut output_total = 0u64;
    for point in token_history.iter().filter(|point| {
        point.timestamp >= started_at
            && point.timestamp < attribution_end
            && point.delta.output_tokens > 0
    }) {
        output_total = output_total.saturating_add(point.delta.output_tokens);
        match point.model.as_deref().filter(|model| !model.is_empty()) {
            Some(model) => {
                let total = output_by_model.entry(model.to_owned()).or_default();
                *total = total.saturating_add(point.delta.output_tokens);
            }
            None => unattributed = true,
        }
    }
    if unattributed || output_total != turn.tokens.output_tokens || output_by_model.is_empty() {
        return "Unknown model".to_owned();
    }
    if output_by_model.len() > 1 {
        return "Mixed models".to_owned();
    }
    // Prefer token-event evidence over TurnInfo.model, which can be refreshed by
    // a later turn_context while the turn's aggregate token count stays intact.
    output_by_model
        .into_keys()
        .next()
        .unwrap_or_else(|| "Unknown model".to_owned())
}

fn turn_mode_label(token_history: &[TokenHistoryPoint], turn: &TurnInfo) -> &'static str {
    let (Some((started_at, attribution_end)), Some(configured_tier)) =
        (token_history_bounds(turn), turn.service_tier.as_deref())
    else {
        return "unknown";
    };
    let mut observed_tier: Option<&str> = None;
    let mut output_total = 0u64;
    for point in token_history.iter().filter(|point| {
        point.timestamp >= started_at
            && point.timestamp < attribution_end
            && point.delta.output_tokens > 0
    }) {
        output_total = output_total.saturating_add(point.delta.output_tokens);
        let Some(tier) = point.service_tier.as_deref() else {
            return "unknown";
        };
        if observed_tier.is_some_and(|existing| existing != tier) {
            return "unknown";
        }
        observed_tier = Some(tier);
    }
    if output_total != turn.tokens.output_tokens || observed_tier != Some(configured_tier) {
        return "unknown";
    }
    classify_tier(configured_tier).unwrap_or("unknown")
}

fn token_history_bounds(turn: &TurnInfo) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let (started_at, completed_at) = (turn.started_at?, turn.completed_at?);
    let attribution_end = if completed_at.timestamp_subsec_nanos() == 0 {
        completed_at.checked_add_signed(Duration::seconds(1))?
    } else {
        completed_at
    };
    Some((started_at, attribution_end))
}

fn parse_range(query: &SpeedQuery) -> Option<(DateTime<Utc>, DateTime<Utc>)> {
    let from = DateTime::parse_from_rfc3339(&query.from)
        .ok()?
        .with_timezone(&Utc);
    let to = DateTime::parse_from_rfc3339(&query.to)
        .ok()?
        .with_timezone(&Utc);
    if to < from || to - from > MAX_RANGE {
        return None;
    }
    Some((from, to))
}

fn has_supported_schema(conn: &Connection) -> bool {
    let Ok(mut statement) = conn.prepare("PRAGMA table_info(logs)") else {
        return false;
    };
    let Ok(columns) = statement.query_map([], |row| row.get::<_, String>(1)) else {
        return false;
    };
    let names: std::collections::HashSet<String> = columns.filter_map(Result::ok).collect();
    ["id", "ts", "feedback_log_body"]
        .iter()
        .all(|name| names.contains(*name))
}

fn parse_embedded_json(body: &str, marker: &str) -> Option<Value> {
    let start = body.find(marker)? + marker.len();
    let json_start = start + body[start..].find('{')?;
    let mut stream = serde_json::Deserializer::from_str(&body[json_start..]).into_iter::<Value>();
    stream.next()?.ok()
}

fn parse_prefix_fields(body: &str) -> HashMap<String, String> {
    let mut fields = HashMap::new();
    // Only tracing metadata before the JSON can identify a turn or effort.
    // Prompts and replies may contain identical strings inside the payload.
    let end = ["websocket event:", "websocket request:"]
        .iter()
        .filter_map(|marker| body.find(marker))
        .min()
        .unwrap_or(0);
    let prefix = &body[..end];
    for key in ["turn.id", "codex.turn.reasoning_effort"] {
        let marker = format!("{key}=");
        if let Some(start) = prefix.find(&marker).map(|i| i + marker.len()) {
            let value = prefix[start..]
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_start_matches('"')
                .trim_end_matches(|character: char| {
                    !character.is_ascii_alphanumeric()
                        && !matches!(character, '_' | '-' | '.' | '/')
                });
            if let Some(value) = safe_label(value) {
                fields.insert(key.to_owned(), value);
            }
        }
    }
    fields
}

fn safe_label(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
    {
        return None;
    }
    Some(value.to_owned())
}

fn parse_sample(
    response: &Value,
    prefix: &HashMap<String, String>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Option<SpeedSample> {
    let model = safe_label(response.get("model")?.as_str()?)?;
    let usage = response.get("usage")?;
    let output_tokens = usage.get("output_tokens")?.as_u64()?;
    let reasoning_value = usage.pointer("/output_tokens_details/reasoning_tokens");
    let reasoning_tokens = match reasoning_value {
        Some(value) => Some(value.as_u64()?),
        None => None,
    };
    if reasoning_tokens.is_some_and(|reasoning| reasoning > output_tokens) {
        return None;
    }
    if output_tokens < 100
        || has_non_text_usage(usage)
        || has_tool_usage(response)
        || has_non_text_output(response)
    {
        return None;
    }

    let created_at = response.get("created_at")?.as_i64()?;
    let completed_at = response.get("completed_at")?.as_i64()?;
    let duration_seconds = completed_at.checked_sub(created_at)?;
    if duration_seconds < 5 {
        return None;
    }
    let completed = DateTime::from_timestamp(completed_at, 0)?;
    if completed < from || completed > to {
        return None;
    }
    let duration_ms = duration_seconds as f64 * 1000.0;
    let output_tps = output_tokens as f64 / (duration_ms / 1000.0);
    let visible_tps = reasoning_tokens
        .map(|reasoning| output_tokens.saturating_sub(reasoning) as f64 / (duration_ms / 1000.0));
    if !output_tps.is_finite() || visible_tps.is_some_and(|value| !value.is_finite()) {
        return None;
    }

    let served = response.get("service_tier").and_then(Value::as_str);
    let mode = if let Some(served) = served {
        classify_tier(served).unwrap_or("unknown")
    } else {
        "unknown"
    };
    let effort = response
        .pointer("/reasoning/effort")
        .and_then(Value::as_str)
        .and_then(safe_label)
        .or_else(|| prefix.get("codex.turn.reasoning_effort").cloned());

    Some(SpeedSample {
        completed_at: completed.to_rfc3339_opts(SecondsFormat::Millis, true),
        model,
        reasoning_effort: effort,
        time_to_first_token_ms: None,
        timing_source: "response",
        mode,
        output_tokens,
        reasoning_tokens,
        duration_ms,
        output_tps,
        visible_tps,
    })
}

fn classify_tier(tier: &str) -> Option<&'static str> {
    match tier.to_ascii_lowercase().as_str() {
        "fast" | "priority" => Some("fast"),
        "default" | "standard" => Some("standard"),
        "auto" => None,
        _ => None,
    }
}

fn has_non_text_usage(usage: &Value) -> bool {
    [
        "/input_tokens_details/image_tokens",
        "/input_tokens_details/audio_tokens",
        "/output_tokens_details/image_tokens",
        "/output_tokens_details/audio_tokens",
    ]
    .iter()
    .any(|path| usage.pointer(path).and_then(Value::as_u64).unwrap_or(0) > 0)
}

fn has_tool_usage(response: &Value) -> bool {
    fn any_positive(value: &Value) -> bool {
        match value {
            Value::Number(number) => number.as_f64().is_some_and(|n| n > 0.0),
            Value::Array(items) => items.iter().any(any_positive),
            Value::Object(fields) => fields.values().any(any_positive),
            _ => false,
        }
    }
    response.get("tool_usage").is_some_and(any_positive)
}

fn has_non_text_output(response: &Value) -> bool {
    let Some(output) = response.get("output").and_then(Value::as_array) else {
        return true;
    };
    let mut saw_text = false;
    let has_non_text = output.iter().any(|item| {
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        if item_type == "reasoning" {
            return false;
        }
        if item_type != "message" {
            return true;
        }
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            return true;
        };
        parts.iter().any(|part| {
            if !matches!(
                part.get("type").and_then(Value::as_str),
                Some("output_text" | "text" | "refusal")
            ) {
                return true;
            }
            if part
                .get("text")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty())
            {
                saw_text = true;
            }
            false
        })
    });
    has_non_text || !saw_text
}

fn unavailable(measurement: SpeedMeasurement, reason: &'static str) -> SpeedReport {
    SpeedReport {
        status: "unavailable",
        reason: Some(reason),
        measurement: measurement.as_str(),
        source: match measurement {
            SpeedMeasurement::Turn => "codex_session_logs",
            SpeedMeasurement::Response => "codex_local_logs",
        },
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        rows: Vec::new(),
        excluded_count: 0,
        scanned_rows: 0,
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use tempfile::tempdir;

    fn create_logs_db(path: &Path) -> Connection {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE logs (
                id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, ts_nanos INTEGER NOT NULL DEFAULT 0,
                thread_id TEXT, process_uuid TEXT, feedback_log_body TEXT
            );",
        )
        .unwrap();
        conn
    }

    fn insert(conn: &Connection, id: i64, body: &str) {
        conn.execute(
            "INSERT INTO logs(id, ts, thread_id, process_uuid, feedback_log_body) VALUES(?1, 1000, 'thread-a', 'proc-a', ?2)",
            params![id, body],
        )
        .unwrap();
    }

    fn event(id: &str, model: &str, created: i64, completed: i64, extra: &str) -> String {
        let mut event = serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": id,
                "model": model,
                "service_tier": "priority",
                "created_at": created,
                "completed_at": completed,
                "output": [{"type": "message", "content": [{"type": "output_text", "text": "synthetic"}]}],
                "usage": {
                    "output_tokens": 120,
                    "output_tokens_details": {"reasoning_tokens": 20}
                }
            }
        });
        if !extra.is_empty() {
            let extra_fields: Value =
                serde_json::from_str(&format!("{{{}}}", extra.trim_start_matches(','))).unwrap();
            event["response"]
                .as_object_mut()
                .unwrap()
                .extend(extra_fields.as_object().unwrap().clone());
        }
        format!("websocket event: {event}")
    }

    fn bounds() -> (DateTime<Utc>, DateTime<Utc>) {
        (
            DateTime::from_timestamp(900, 0).unwrap(),
            DateTime::from_timestamp(1200, 0).unwrap(),
        )
    }

    #[test]
    fn query_defaults_to_turns_and_keeps_response_measurement_selectable() {
        let default_query: SpeedQuery =
            serde_json::from_str(r#"{"from":"2026-10-01T00:00:00Z","to":"2026-10-02T00:00:00Z"}"#)
                .unwrap();
        assert_eq!(default_query.measurement, SpeedMeasurement::Turn);

        let response_query: SpeedQuery = serde_json::from_str(
            r#"{"from":"2026-10-01T00:00:00Z","to":"2026-10-02T00:00:00Z","measurement":"response"}"#,
        )
        .unwrap();
        assert_eq!(response_query.measurement, SpeedMeasurement::Response);
    }

    #[test]
    fn turn_candidate_selector_keeps_only_newest_sessions_in_deterministic_order() {
        let mut selector = TurnCandidateSelector::default();
        for timestamp in (0..750).rev() {
            selector.push(
                format!("session-{timestamp:04}"),
                DateTime::from_timestamp(timestamp, 0).unwrap(),
            );
            assert!(selector.newest.len() <= MAX_TURN_SESSIONS);
        }

        assert!(selector.discarded);
        let candidates = selector.into_newest_first();
        assert_eq!(candidates.len(), MAX_TURN_SESSIONS);
        assert_eq!(candidates.first().unwrap().0, "session-0749");
        assert_eq!(candidates.last().unwrap().0, "session-0250");

        let same_time = DateTime::from_timestamp(10_000, 0).unwrap();
        let mut tied = TurnCandidateSelector::default();
        tied.push("storage-a".to_owned(), same_time);
        tied.push("storage-z".to_owned(), same_time);
        let tied = tied.into_newest_first();
        assert_eq!(tied[0].0, "storage-z");
        assert_eq!(tied[1].0, "storage-a");
    }

    #[test]
    fn reads_read_only_filters_deduplicates_and_ignores_ambiguous_request_effort() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("logs_2.sqlite");
        let conn = create_logs_db(&path);
        insert(
            &conn,
            1,
            "thread.id=thread-a turn.id=turn-2 websocket request: {\"model\":\"gpt-5.5\",\"service_tier\":\"standard\",\"reasoning\":{\"effort\":\"high\"}}",
        );
        insert(
            &conn,
            2,
            &format!(
                "thread.id=thread-a turn.id=turn-1 codex.turn.reasoning_effort=medium {}",
                event("resp-1", "gpt-5.5", 1000, 1010, "")
            ),
        );
        insert(
            &conn,
            3,
            &format!(
                "thread.id=thread-a turn.id=turn-1 codex.turn.reasoning_effort=medium {}",
                event("resp-1", "gpt-5.5", 1000, 1012, ",\"output\":[{\"type\":\"reasoning\",\"summary\":[]},{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\"}]}]")
            ),
        );
        insert(
            &conn,
            4,
            &format!(
                "thread.id=thread-a turn.id=turn-2 {}",
                event("resp-2", "gpt-5.5", 1000, 1010, ",\"service_tier\":null")
            ),
        );
        insert(
            &conn,
            5,
            &format!(
                "{} thread.id=thread-a turn.id=turn-1",
                event(
                    "resp-tool",
                    "gpt-5.5",
                    1000,
                    1010,
                    ",\"tool_usage\":{\"browser\":{\"num_requests\":1}}"
                )
            ),
        );
        insert(&conn, 6, "websocket event: {broken json");
        insert(
            &conn,
            7,
            &format!(
                "{} thread.id=thread-a turn.id=turn-1",
                event(
                    "resp-image",
                    "gpt-5.5",
                    1000,
                    1010,
                    ",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"image\"}]}]"
                )
            ),
        );
        insert(
            &conn,
            8,
            &format!(
                "{} thread.id=thread-a turn.id=turn-1",
                event(
                    "resp-incomplete",
                    "gpt-5.5",
                    1000,
                    1010,
                    ",\"status\":\"incomplete\""
                )
            ),
        );
        let oversized = format!("websocket event: {}", "x".repeat(MAX_BODY_BYTES + 32));
        insert(&conn, 9, &oversized);
        drop(conn);

        let (from, to) = bounds();
        let report = report_from_path(&path, from, to);
        assert_eq!(report.status, "ready");
        assert_eq!(report.rows.len(), 2);
        let first = report
            .rows
            .iter()
            .find(|row| row.reasoning_effort.as_deref() == Some("medium"))
            .unwrap();
        assert_eq!(report.measurement, "response");
        assert_eq!(report.source, "codex_local_logs");
        assert_eq!(first.mode, "fast");
        assert_eq!(first.reasoning_effort.as_deref(), Some("medium"));
        assert_eq!(first.reasoning_tokens, Some(20));
        assert_eq!(first.duration_ms, 12_000.0);
        assert_eq!(first.output_tps, 10.0);
        assert_eq!(first.visible_tps, Some(100.0 / 12.0));
        assert_eq!(first.time_to_first_token_ms, None);
        assert_eq!(first.timing_source, "response");
        let unknown = report
            .rows
            .iter()
            .find(|row| row.mode == "unknown")
            .unwrap();
        assert_eq!(unknown.reasoning_effort, None);
        assert_eq!(report.excluded_count, 5);
        assert_eq!(report.scanned_rows, 8);
        assert!(report.truncated);
        assert_eq!(classify_tier("default"), Some("standard"));
        assert_eq!(classify_tier("auto"), None);

        let check = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .unwrap();
        assert!(
            check
                .query_row("PRAGMA query_only", [], |row| row.get::<_, i64>(0))
                .unwrap_or(0)
                == 0
        );
        assert!(
            check
                .query_row("SELECT COUNT(*) FROM logs", [], |row| row.get::<_, i64>(0))
                .unwrap()
                == 9
        );
    }

    #[test]
    fn malformed_schema_missing_database_and_invalid_ranges_are_honest() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("missing.sqlite");
        let (from, to) = bounds();
        assert_eq!(
            report_from_path(&missing, from, to).reason,
            Some("Codex local logs are unavailable.")
        );
        assert!(!missing.exists());

        let bad = dir.path().join("bad.sqlite");
        Connection::open(&bad)
            .unwrap()
            .execute_batch("CREATE TABLE logs(id INTEGER PRIMARY KEY, feedback_log_body TEXT);")
            .unwrap();
        assert_eq!(
            report_from_path(&bad, from, to).reason,
            Some("The Codex local log schema is not supported.")
        );
        assert!(parse_range(&SpeedQuery {
            from: "2026-01-01T00:00:00Z".into(),
            to: "2026-02-01T00:00:00Z".into(),
            measurement: SpeedMeasurement::Turn,
        })
        .is_none());
    }

    #[test]
    fn oversized_response_id_is_excluded_without_blocking_next_valid_response() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("logs_2.sqlite");
        let conn = create_logs_db(&path);
        insert(
            &conn,
            1,
            &event(
                &"x".repeat(MAX_RESPONSE_ID_BYTES + 1),
                "gpt-5.5",
                1000,
                1010,
                "",
            ),
        );
        insert(
            &conn,
            2,
            &event("valid-response", "gpt-5.5", 1000, 1010, ""),
        );
        drop(conn);

        let (from, to) = bounds();
        let report = report_from_path(&path, from, to);
        assert_eq!(report.status, "ready");
        assert_eq!(report.rows.len(), 1);
        assert_eq!(report.excluded_count, 1);
        assert_eq!(report.scanned_rows, 2);
        assert_eq!(report.rows[0].output_tokens, 120);
    }

    fn token_point(
        timestamp: i64,
        model: Option<&str>,
        service_tier: Option<&str>,
        output_tokens: u64,
        cumulative_total: u64,
    ) -> TokenHistoryPoint {
        token_point_at(
            DateTime::from_timestamp(timestamp, 0).unwrap(),
            model,
            service_tier,
            output_tokens,
            cumulative_total,
        )
    }

    fn token_point_at(
        timestamp: DateTime<Utc>,
        model: Option<&str>,
        service_tier: Option<&str>,
        output_tokens: u64,
        cumulative_total: u64,
    ) -> TokenHistoryPoint {
        TokenHistoryPoint {
            timestamp,
            model: model.map(str::to_owned),
            service_tier: service_tier.map(str::to_owned),
            request_input_tokens: None,
            total_tokens: cumulative_total,
            delta: crate::model::TokenTotals {
                output_tokens,
                total_tokens: output_tokens,
                ..Default::default()
            },
        }
    }

    fn synthetic_turn() -> TurnInfo {
        TurnInfo {
            turn_id: "turn-synthetic".into(),
            model: Some("stale-later-model".into()),
            reasoning_effort: Some("high".into()),
            service_tier: Some("fast".into()),
            status: TurnStatus::Completed,
            started_at: DateTime::from_timestamp(1000, 0),
            completed_at: DateTime::from_timestamp(1010, 0),
            duration_ms: Some(4_000),
            time_to_first_token_ms: Some(250),
            tokens: crate::model::TokenTotals {
                output_tokens: 120,
                reasoning_output_tokens: 30,
                total_tokens: 1_500,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn turn_report_prefers_explicit_duration_and_uses_delta_model_evidence() {
        let (from, to) = bounds();
        let turn = synthetic_turn();
        let history = vec![token_point(1005, Some("gpt-5.4"), Some("fast"), 120, 5_000)];
        let sample = turn_sample(&turn, &history, from, to).unwrap();

        assert_eq!(sample.model, "gpt-5.4");
        assert_eq!(sample.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(sample.mode, "fast");
        assert_eq!(sample.output_tokens, 120);
        assert_eq!(sample.output_tps, 30.0);
        assert_eq!(sample.time_to_first_token_ms, Some(250));
        assert_eq!(sample.timing_source, "explicit");
        assert_eq!(sample.reasoning_tokens, None);
        assert_eq!(sample.visible_tps, None);

        let mut invalid_ttft = turn.clone();
        invalid_ttft.time_to_first_token_ms = Some(4_001);
        let sample = turn_sample(&invalid_ttft, &history, from, to).unwrap();
        assert_eq!(sample.time_to_first_token_ms, None);
    }

    #[test]
    fn turn_report_falls_back_to_timestamps_and_hides_mixed_model_or_tier() {
        let (from, to) = bounds();
        let mut turn = synthetic_turn();
        turn.duration_ms = None;
        let one_model = vec![token_point(1005, Some("gpt-5.4"), Some("fast"), 120, 5_000)];
        let sample = turn_sample(&turn, &one_model, from, to).unwrap();
        assert_eq!(sample.duration_ms, 10_000.0);
        assert_eq!(sample.timing_source, "timestamps");
        assert_eq!(sample.mode, "fast");

        let mixed = vec![
            token_point(1004, Some("gpt-5.4"), Some("fast"), 60, 1_000),
            token_point(1006, Some("gpt-5.5"), Some("standard"), 60, 5_000),
        ];
        let mixed_sample = turn_sample(&turn, &mixed, from, to).unwrap();
        assert_eq!(mixed_sample.model, "Mixed models");
        assert_eq!(mixed_sample.mode, "unknown");
    }

    #[test]
    fn second_precision_completion_includes_final_event_but_extra_output_fails_closed() {
        let (from, to) = bounds();
        let turn = synthetic_turn();
        let final_event = vec![token_point_at(
            DateTime::from_timestamp(1010, 300_000_000).unwrap(),
            Some("gpt-5.4"),
            Some("fast"),
            120,
            5_000,
        )];
        let sample = turn_sample(&turn, &final_event, from, to).unwrap();
        assert_eq!(sample.model, "gpt-5.4");
        assert_eq!(sample.mode, "fast");
        assert_eq!(sample.completed_at, "1970-01-01T00:16:50.000Z");

        let mut with_unrelated_output = final_event;
        with_unrelated_output.push(token_point_at(
            DateTime::from_timestamp(1010, 600_000_000).unwrap(),
            Some("gpt-5.4"),
            Some("fast"),
            1,
            5_001,
        ));
        let sample = turn_sample(&turn, &with_unrelated_output, from, to).unwrap();
        assert_eq!(sample.model, "Unknown model");
        assert_eq!(sample.mode, "unknown");
    }

    #[test]
    fn turn_report_excludes_incomplete_out_of_range_and_zero_duration_turns() {
        let (from, to) = bounds();
        let history = vec![token_point(1005, Some("gpt-5.4"), Some("fast"), 120, 120)];
        let mut turn = synthetic_turn();
        turn.status = TurnStatus::InProgress;
        assert!(turn_sample(&turn, &history, from, to).is_none());
        turn.status = TurnStatus::Completed;
        turn.completed_at = DateTime::from_timestamp(1201, 0);
        assert!(turn_sample(&turn, &history, from, to).is_none());
        turn.completed_at = DateTime::from_timestamp(1010, 0);
        turn.duration_ms = Some(0);
        assert!(turn_sample(&turn, &history, from, to).is_none());
        turn.duration_ms = None;
        turn.started_at = DateTime::from_timestamp(1011, 0);
        assert!(turn_sample(&turn, &history, from, to).is_none());
    }

    #[test]
    fn served_modes_and_timing_bounds_do_not_guess_missing_evidence() {
        let (from, to) = bounds();
        let mut response = serde_json::json!({
            "model": "gpt-5.5", "created_at": 1000, "completed_at": 1010,
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "synthetic"}]}],
            "usage": {"output_tokens": 120}
        });
        for (tier, expected) in [
            ("priority", "fast"),
            ("fast", "fast"),
            ("default", "standard"),
            ("standard", "standard"),
            ("auto", "unknown"),
            ("flex", "unknown"),
        ] {
            response["service_tier"] = Value::String(tier.into());
            assert_eq!(
                parse_sample(&response, &HashMap::new(), from, to)
                    .unwrap()
                    .mode,
                expected
            );
        }
        response.as_object_mut().unwrap().remove("service_tier");
        assert_eq!(
            parse_sample(&response, &HashMap::new(), from, to)
                .unwrap()
                .mode,
            "unknown"
        );
        // Completion bounds are inclusive; log timestamps do not replace them.
        let completion = DateTime::from_timestamp(1010, 0).unwrap();
        assert!(parse_sample(&response, &HashMap::new(), completion, completion).is_some());
        assert!(parse_sample(
            &response,
            &HashMap::new(),
            completion + Duration::seconds(1),
            to
        )
        .is_none());
        response["completed_at"] = serde_json::json!(1004);
        assert!(parse_sample(&response, &HashMap::new(), from, to).is_none());
        response["completed_at"] = serde_json::json!(1005);
        assert!(parse_sample(&response, &HashMap::new(), from, to).is_some());
        response["usage"]["output_tokens"] = serde_json::json!(99);
        assert!(parse_sample(&response, &HashMap::new(), from, to).is_none());
    }

    #[test]
    fn span_metadata_cannot_be_supplied_by_prompt_or_response_text() {
        let fields = parse_prefix_fields(
            "span{turn.id=\"turn-a\" codex.turn.reasoning_effort=\"high\"}: websocket event: {\"text\":\"turn.id=fake codex.turn.reasoning_effort=low\"}",
        );
        assert_eq!(fields.get("turn.id").map(String::as_str), Some("turn-a"));
        assert_eq!(
            fields
                .get("codex.turn.reasoning_effort")
                .map(String::as_str),
            Some("high")
        );
        assert!(parse_prefix_fields(
            "websocket request: {\"text\":\"turn.id=fake codex.turn.reasoning_effort=low\"}"
        )
        .is_empty());
    }

    #[test]
    fn body_budget_counts_utf8_bytes_and_keeps_the_next_valid_response() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("logs_2.sqlite");
        let conn = create_logs_db(&path);
        let oversized = format!("websocket event: {}", "🧪".repeat(MAX_BODY_BYTES / 4 + 1));
        assert!(oversized.chars().count() < MAX_BODY_BYTES);
        insert(&conn, 1, &oversized);
        insert(&conn, 2, &event("valid", "gpt-5.5", 1000, 1010, ""));
        drop(conn);
        let original = std::fs::read(&path).unwrap();
        let (from, to) = bounds();
        let report = report_from_path(&path, from, to);
        assert_eq!(report.status, "ready");
        assert!(report.truncated);
        assert_eq!(report.scanned_rows, 2);
        assert_eq!(report.excluded_count, 1);
        assert_eq!(report.rows.len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn missing_output_is_excluded_and_missing_reasoning_stays_unknown() {
        let (from, to) = bounds();
        let no_output = serde_json::json!({
            "id": "no-output",
            "model": "gpt-5.5",
            "created_at": 1000,
            "completed_at": 1010,
            "usage": {"output_tokens": 120}
        });
        assert!(parse_sample(&no_output, &HashMap::new(), from, to).is_none());

        let output_only = serde_json::json!({
            "id": "output-only",
            "model": "gpt-5.5",
            "created_at": 1000,
            "completed_at": 1010,
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "synthetic"}]}],
            "usage": {"output_tokens": 120}
        });
        let sample = parse_sample(&output_only, &HashMap::new(), from, to).unwrap();
        assert_eq!(sample.reasoning_tokens, None);
        assert_eq!(sample.visible_tps, None);
        assert_eq!(sample.output_tps, 12.0);

        let malformed_reasoning = serde_json::json!({
            "id": "malformed-reasoning",
            "model": "gpt-5.5",
            "created_at": 1000,
            "completed_at": 1010,
            "output": [{"type": "message", "content": [{"type": "output_text", "text": "synthetic"}]}],
            "usage": {"output_tokens": 120, "output_tokens_details": {"reasoning_tokens": "unknown"}}
        });
        assert!(parse_sample(&malformed_reasoning, &HashMap::new(), from, to).is_none());
    }
}
