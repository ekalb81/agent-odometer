use crate::model::{Session, TokenHistoryPoint, TokenTotals, TurnStatus};
use anyhow::Context;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

pub struct SessionParser {
    pub session: Option<Session>,
    pub byte_offset: u64,
    pub current_model: Option<String>,
    pub current_service_tier: Option<String>,
    pub current_turn_id: Option<String>,
    pub file_path: PathBuf,
    pub archived: bool,
}

/// Max characters retained for per-turn prompt / agent-message previews.
const TURN_MESSAGE_LIMIT: usize = 500;

impl SessionParser {
    pub fn new(file_path: PathBuf, archived: bool) -> Self {
        Self {
            session: None,
            byte_offset: 0,
            current_model: None,
            current_service_tier: None,
            current_turn_id: None,
            file_path,
            archived,
        }
    }

    pub fn parse_to_end(&mut self) -> anyhow::Result<bool> {
        let file = std::fs::File::open(&self.file_path)
            .with_context(|| format!("opening {:?}", self.file_path))?;
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(self.byte_offset))?;

        let mut updated = false;
        let mut line = String::new();

        loop {
            let start = self.byte_offset;
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }

            // A partial trailing line has no terminating newline — leave byte_offset at its start.
            if !line.ends_with('\n') {
                break;
            }

            self.byte_offset = start + n as u64;

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            match self.apply_line(trimmed) {
                Ok(()) => updated = true,
                Err(e) => tracing::warn!("skipping unparseable line at offset {}: {}", start, e),
            }
        }

        Ok(updated)
    }

    pub fn apply_line(&mut self, line: &str) -> anyhow::Result<()> {
        // Fast path: record types we discard wholesale (response_item, compacted)
        // account for the vast majority of bytes in a rollout file. When the line
        // has the exact leading shape Codex writes — {"timestamp":"<ts>","type":"<t>"
        // — we can extract the timestamp and skip the full JSON parse entirely.
        // These lines must still advance last_event_at, hence the extraction.
        if let Some(ts) = fast_skip_timestamp(line) {
            let last_event_at: DateTime<Utc> = ts.parse().unwrap_or_else(|_| Utc::now());
            if let Some(s) = self.session.as_mut() {
                if last_event_at > s.last_event_at {
                    s.last_event_at = last_event_at;
                }
            }
            return Ok(());
        }

        let mut root: Value = serde_json::from_str(line)?;

        let timestamp_str = root
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let last_event_at: DateTime<Utc> = timestamp_str.parse().unwrap_or_else(|_| Utc::now());

        if let Some(s) = self.session.as_mut() {
            if last_event_at > s.last_event_at {
                s.last_event_at = last_event_at;
            }
        }

        let event_type = root
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        // Skip response_item entirely — it's bulky and carries no usage stats.
        if event_type == "response_item" {
            return Ok(());
        }

        let payload = root
            .get_mut("payload")
            .map(Value::take)
            .unwrap_or(Value::Null);

        match event_type.as_str() {
            "session_meta" => self.handle_session_meta(payload, last_event_at)?,
            "turn_context" => self.handle_turn_context(payload)?,
            "event_msg" => self.handle_event_msg(payload, last_event_at)?,
            _ => {}
        }

        Ok(())
    }

    fn handle_session_meta(
        &mut self,
        payload: Value,
        last_event_at: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        // If we've already built a Session from a prior session_meta event in
        // this file, do NOT recreate it — Codex Desktop emits session_meta
        // again on resume / re-open within the same rollout file, and
        // recreating the Session here would wipe tokens_by_model,
        // tokens_history, total_turns, etc. that we've accumulated. Refresh
        // only the identity/metadata fields that can legitimately change.
        if let Some(s) = self.session.as_mut() {
            if !id.is_empty() && s.id != id {
                tracing::warn!(
                    "session_meta id changed mid-file (was {}, now {}); keeping prior session state",
                    s.id, id
                );
            }
            if let Some(name) = payload.get("thread_name").and_then(Value::as_str) {
                s.thread_name = Some(name.to_owned());
            }
            if let Some(cli) = payload.get("cli_version").and_then(Value::as_str) {
                s.cli_version = Some(cli.to_owned());
            }
            if let Some(value) = payload.get("forked_from_id").and_then(Value::as_str) {
                s.forked_from_id = Some(value.to_owned());
            }
            if let Some(value) = payload.get("parent_thread_id").and_then(Value::as_str) {
                s.parent_thread_id = Some(value.to_owned());
            }
            if let Some(value) = payload.get("agent_path").and_then(Value::as_str) {
                s.agent_path = Some(value.to_owned());
            }
            if let Some(value) = payload.get("agent_nickname").and_then(Value::as_str) {
                s.agent_nickname = Some(value.to_owned());
            }
            if let Some(value) = parse_session_source(&payload) {
                s.source = Some(value);
            }
            if let Some(value) = payload.get("history_mode").and_then(Value::as_str) {
                s.history_mode = Some(value.to_owned());
            }
            if let Some(value) = payload.get("memory_mode").and_then(Value::as_str) {
                s.memory_mode = Some(value.to_owned());
            }
            if last_event_at > s.last_event_at {
                s.last_event_at = last_event_at;
            }
            return Ok(());
        }

        let started_at: DateTime<Utc> = payload
            .get("timestamp")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("missing session_meta.payload.timestamp"))?
            .parse()
            .context("parsing session_meta timestamp")?;

        let cwd = payload
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let originator = payload
            .get("originator")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let source = parse_session_source(&payload);

        let cli_version = payload
            .get("cli_version")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let model_provider = payload
            .get("model_provider")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let thread_name = payload
            .get("thread_name")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let forked_from_id = payload
            .get("forked_from_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let parent_thread_id = payload
            .get("parent_thread_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let agent_path = payload
            .get("agent_path")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let agent_nickname = payload
            .get("agent_nickname")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let history_mode = payload
            .get("history_mode")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let memory_mode = payload
            .get("memory_mode")
            .and_then(Value::as_str)
            .map(str::to_owned);

        self.session = Some(Session {
            id,
            harness: crate::model::Harness::Codex,
            thread_name,
            forked_from_id,
            parent_thread_id,
            agent_path,
            agent_nickname,
            file_path: self.file_path.to_string_lossy().into_owned(),
            archived: self.archived,
            started_at,
            last_event_at,
            working_directory: cwd,
            originator,
            source,
            history_mode,
            memory_mode,
            cli_version,
            model_provider,
            model: None,
            service_tier: None,
            plan_type: None,
            credits_unlimited: None,
            credits_balance: None,
            context_window: None,
            total_turns: 0,
            first_user_message: None,
            tokens_total: TokenTotals::default(),
            tokens_by_model: HashMap::new(),
            tokens_history: Vec::new(),
            turns: Vec::new(),
        });

        Ok(())
    }

    fn handle_turn_context(&mut self, payload: Value) -> anyhow::Result<()> {
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let turn_id = payload
            .get("turn_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let reasoning_effort = payload
            .get("effort")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let collaboration_mode = payload
            .get("collaboration_mode")
            .and_then(|value| {
                value
                    .as_str()
                    .or_else(|| value.get("mode").and_then(Value::as_str))
            })
            .map(str::to_owned);
        let explicit_service_tier = payload
            .get("service_tier")
            .and_then(Value::as_str)
            .map(str::to_owned);

        if let Some(m) = &model {
            self.current_model = Some(m.clone());
        }
        if let Some(t) = &turn_id {
            self.current_turn_id = Some(t.clone());
        }
        if let Some(tier) = &explicit_service_tier {
            self.current_service_tier = Some(tier.clone());
        }
        let service_tier = self.current_service_tier.clone();

        if let Some(s) = self.session.as_mut() {
            if let Some(m) = &model {
                s.model = Some(m.clone());
            }
            if service_tier.is_some() {
                s.service_tier = service_tier.clone();
            }
            if let Some(tid) = &turn_id {
                let idx = ensure_turn_index(s, tid);
                if model.is_some() {
                    s.turns[idx].model = model;
                }
                if reasoning_effort.is_some() {
                    s.turns[idx].reasoning_effort = reasoning_effort;
                }
                if collaboration_mode.is_some() {
                    s.turns[idx].collaboration_mode = collaboration_mode;
                }
                if service_tier.is_some() {
                    s.turns[idx].service_tier = service_tier;
                }
            }
        }

        Ok(())
    }

    fn handle_thread_settings(&mut self, payload: &Value) {
        let Some(settings) = payload.get("thread_settings") else {
            return;
        };
        let model = settings
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let service_tier = settings
            .get("service_tier")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let reasoning_effort = settings
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let collaboration_mode = settings
            .get("collaboration_mode")
            .and_then(|value| {
                value
                    .as_str()
                    .or_else(|| value.get("mode").and_then(Value::as_str))
            })
            .map(str::to_owned);

        if let Some(value) = &model {
            self.current_model = Some(value.clone());
        }
        self.current_service_tier = service_tier.clone();

        if let Some(session) = self.session.as_mut() {
            if model.is_some() {
                session.model = model;
            }
            session.service_tier = service_tier.clone();
            if let Some(turn_id) = &self.current_turn_id {
                let idx = ensure_turn_index(session, turn_id);
                session.turns[idx].service_tier = service_tier;
                if reasoning_effort.is_some() {
                    session.turns[idx].reasoning_effort = reasoning_effort;
                }
                if collaboration_mode.is_some() {
                    session.turns[idx].collaboration_mode = collaboration_mode;
                }
            }
        }
    }

    fn handle_event_msg(&mut self, payload: Value, event_ts: DateTime<Utc>) -> anyhow::Result<()> {
        let msg_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        match msg_type.as_str() {
            "task_started" => {
                let turn_id = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(t) = &turn_id {
                    self.current_turn_id = Some(t.clone());
                }
                let cw = payload.get("model_context_window").and_then(Value::as_u64);
                let started_at = timestamp_field(&payload, "started_at").unwrap_or(event_ts);
                let collaboration_mode = payload
                    .get("collaboration_mode_kind")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(s) = self.session.as_mut() {
                    if let Some(cw) = cw {
                        s.context_window = Some(cw as u32);
                    }
                    if let Some(tid) = &turn_id {
                        let idx = ensure_turn_index(s, tid);
                        if s.turns[idx].started_at.is_none() {
                            s.turns[idx].started_at = Some(started_at);
                        }
                        if collaboration_mode.is_some() {
                            s.turns[idx].collaboration_mode = collaboration_mode;
                        }
                    }
                }
            }
            "user_message" => {
                let msg = payload.get("message").and_then(Value::as_str).map(|m| {
                    let trimmed = m.trim_end();
                    trimmed.chars().take(TURN_MESSAGE_LIMIT).collect::<String>()
                });
                let turn_id = self.current_turn_id.clone();
                if let Some(s) = self.session.as_mut() {
                    if s.first_user_message.is_none() {
                        if let Some(m) = &msg {
                            s.first_user_message = Some(m.chars().take(200).collect());
                        }
                    }
                    if let Some(tid) = &turn_id {
                        let idx = ensure_turn_index(s, tid);
                        if s.turns[idx].user_message.is_none() {
                            s.turns[idx].user_message = msg;
                        }
                    }
                }
            }
            "token_count" => {
                self.handle_token_count(payload, event_ts)?;
            }
            "thread_settings_applied" => {
                self.handle_thread_settings(&payload);
            }
            "task_complete" => {
                let turn_id = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| self.current_turn_id.clone());
                let duration_ms = payload.get("duration_ms").and_then(Value::as_u64);
                let ttft = payload
                    .get("time_to_first_token_ms")
                    .and_then(Value::as_u64);
                let last_agent = payload
                    .get("last_agent_message")
                    .and_then(Value::as_str)
                    .map(|m| {
                        m.trim()
                            .chars()
                            .take(TURN_MESSAGE_LIMIT)
                            .collect::<String>()
                    });
                let completed_at = timestamp_field(&payload, "completed_at").unwrap_or(event_ts);
                if let Some(s) = self.session.as_mut() {
                    s.total_turns += 1;
                    if let Some(tid) = &turn_id {
                        let idx = ensure_turn_index(s, tid);
                        let turn = &mut s.turns[idx];
                        turn.completed_at = Some(completed_at);
                        turn.duration_ms = duration_ms;
                        turn.time_to_first_token_ms = ttft;
                        turn.status = TurnStatus::Completed;
                        if turn.last_agent_message.is_none() {
                            turn.last_agent_message = last_agent;
                        }
                    }
                }
                self.current_turn_id = None;
            }
            "turn_aborted" => {
                let turn_id = payload
                    .get("turn_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| self.current_turn_id.clone());
                let completed_at = timestamp_field(&payload, "completed_at").unwrap_or(event_ts);
                let duration_ms = payload.get("duration_ms").and_then(Value::as_u64);
                let reason = payload
                    .get("reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let (Some(s), Some(tid)) = (self.session.as_mut(), turn_id.as_deref()) {
                    let idx = ensure_turn_index(s, tid);
                    let turn = &mut s.turns[idx];
                    turn.completed_at = Some(completed_at);
                    turn.duration_ms = duration_ms;
                    turn.status = TurnStatus::Aborted;
                    turn.abort_reason = reason;
                }
                self.current_turn_id = None;
            }
            "thread_rolled_back" => {
                let count = payload
                    .get("num_turns")
                    .and_then(Value::as_u64)
                    .unwrap_or(0) as usize;
                if let Some(s) = self.session.as_mut() {
                    for turn in s
                        .turns
                        .iter_mut()
                        .rev()
                        .filter(|turn| turn.status != TurnStatus::RolledBack)
                        .take(count)
                    {
                        turn.status = TurnStatus::RolledBack;
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_token_count(
        &mut self,
        payload: Value,
        event_ts: DateTime<Utc>,
    ) -> anyhow::Result<()> {
        let info = match payload.get("info") {
            Some(v) if !v.is_null() => v,
            // The first token_count event in a session commonly has info: null.
            _ => return Ok(()),
        };

        let total_usage = info.get("total_token_usage").map(parse_token_totals);
        let last_usage = info.get("last_token_usage").map(parse_token_totals);
        let context_window = info
            .get("model_context_window")
            .and_then(Value::as_u64)
            .map(|v| v as u32);
        let model = self.current_model.clone();
        let service_tier = self.current_service_tier.clone();
        let turn_id = self.current_turn_id.clone();

        if let Some(s) = self.session.as_mut() {
            if let Some(total) = &total_usage {
                s.tokens_total = total.clone();
            }
            if let Some(cw) = context_window {
                s.context_window = Some(cw);
            }

            // Compute the per-event contribution once, then attribute it
            // identically to the model bucket, the history point, and the
            // current turn so all three stay consistent with tokens_total.
            //
            // Start from `last_token_usage` (the most recent API call's
            // tokens). Then reconcile: enforce sum(tokens_by_model) ==
            // tokens_total and fold any positive remainder (carry-over from a
            // prior rollout file, or an early event before current_model was
            // known) into the contribution. This converges for fresh, resumed,
            // and mid-session model-switch cases.
            let mut contribution = last_usage.clone().unwrap_or_default();
            if let Some(model) = &model {
                let entry = s.tokens_by_model.entry(model.clone()).or_default();
                add_token_totals(entry, &contribution);

                let bucket_sum = sum_bucket_totals(&s.tokens_by_model);
                let remainder = subtract_totals_saturating(&s.tokens_total, &bucket_sum);
                if totals_any_positive(&remainder) {
                    let entry = s.tokens_by_model.entry(model.clone()).or_default();
                    add_token_totals(entry, &remainder);
                    add_token_totals(&mut contribution, &remainder);
                }
            }

            if let Some(total) = &total_usage {
                s.tokens_history.push(TokenHistoryPoint {
                    timestamp: event_ts,
                    model: model.clone(),
                    service_tier: service_tier.clone(),
                    total_tokens: total.total_tokens,
                    delta: contribution.clone(),
                });
            }

            if let Some(tid) = &turn_id {
                if let Some(turn) = s.turns.iter_mut().find(|t| &t.turn_id == tid) {
                    add_token_totals(&mut turn.tokens, &contribution);
                    if service_tier.is_some() {
                        turn.service_tier = service_tier;
                    }
                }
            }
        }

        if let Some(rate_limits) = payload.get("rate_limits") {
            if let Some(s) = self.session.as_mut() {
                if let Some(plan) = rate_limits.get("plan_type").and_then(Value::as_str) {
                    s.plan_type = Some(plan.to_owned());
                }
                if let Some(credits) = rate_limits.get("credits") {
                    if let Some(unlimited) = credits.get("unlimited").and_then(Value::as_bool) {
                        s.credits_unlimited = Some(unlimited);
                    }
                    match credits.get("balance") {
                        Some(Value::Null) | None => s.credits_balance = None,
                        Some(v) => {
                            if let Some(bal) = v.as_f64() {
                                s.credits_balance = Some(bal);
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// If `line` starts with the exact shape `{"timestamp":"<ts>","type":"<t>"` and
/// `<t>` is a record type the parser discards without inspecting its payload,
/// returns the raw timestamp string so the caller can skip full JSON parsing.
///
/// Returns `None` — falling back to the full parse — the moment the structure
/// deviates: no prefix match, a backslash escape inside the timestamp value, a
/// missing `","type":"` separator, or any other record type. Correctness over
/// cleverness: the fast path only fires when the line shape is unambiguous.
fn fast_skip_timestamp(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("{\"timestamp\":\"")?;
    let ts_end = rest.find('"')?;
    let ts = &rest[..ts_end];
    // Codex timestamps contain no escapes; a backslash means the closing quote
    // we found may be escaped, so bail to the full parse.
    if ts.contains('\\') {
        return None;
    }
    let after = rest[ts_end + 1..].strip_prefix(",\"type\":\"")?;
    let ty_end = after.find('"')?;
    match &after[..ty_end] {
        "response_item" | "compacted" => Some(ts),
        _ => None,
    }
}

fn parse_session_source(payload: &Value) -> Option<String> {
    match payload.get("source") {
        Some(Value::String(source)) => Some(source.clone()),
        Some(Value::Object(source)) if source.contains_key("subagent") => Some("subagent".into()),
        _ => payload
            .get("thread_source")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

fn timestamp_field(payload: &Value, key: &str) -> Option<DateTime<Utc>> {
    payload.get(key).and_then(Value::as_str)?.parse().ok()
}

/// Finds the index of the turn with the given id, creating it (with the next
/// 1-based ordinal) if absent.
fn ensure_turn_index(s: &mut Session, turn_id: &str) -> usize {
    if let Some(pos) = s.turns.iter().position(|t| t.turn_id == turn_id) {
        return pos;
    }
    let index = s.turns.len() as u32 + 1;
    s.turns.push(crate::model::TurnInfo {
        turn_id: turn_id.to_owned(),
        index,
        ..Default::default()
    });
    s.turns.len() - 1
}

fn parse_token_totals(v: &Value) -> TokenTotals {
    TokenTotals {
        input_tokens: v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input_tokens: v
            .get("cached_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        reasoning_output_tokens: v
            .get("reasoning_output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        total_tokens: v.get("total_tokens").and_then(Value::as_u64).unwrap_or(0),
    }
}

fn add_token_totals(dst: &mut TokenTotals, src: &TokenTotals) {
    dst.input_tokens += src.input_tokens;
    dst.cached_input_tokens += src.cached_input_tokens;
    dst.output_tokens += src.output_tokens;
    dst.reasoning_output_tokens += src.reasoning_output_tokens;
    dst.total_tokens += src.total_tokens;
}

fn sum_bucket_totals(buckets: &HashMap<String, TokenTotals>) -> TokenTotals {
    let mut acc = TokenTotals::default();
    for t in buckets.values() {
        add_token_totals(&mut acc, t);
    }
    acc
}

fn subtract_totals_saturating(a: &TokenTotals, b: &TokenTotals) -> TokenTotals {
    TokenTotals {
        input_tokens: a.input_tokens.saturating_sub(b.input_tokens),
        cached_input_tokens: a.cached_input_tokens.saturating_sub(b.cached_input_tokens),
        output_tokens: a.output_tokens.saturating_sub(b.output_tokens),
        reasoning_output_tokens: a
            .reasoning_output_tokens
            .saturating_sub(b.reasoning_output_tokens),
        total_tokens: a.total_tokens.saturating_sub(b.total_tokens),
    }
}

fn totals_any_positive(t: &TokenTotals) -> bool {
    t.input_tokens > 0
        || t.cached_input_tokens > 0
        || t.output_tokens > 0
        || t.reasoning_output_tokens > 0
        || t.total_tokens > 0
}

pub fn parse_file(path: &Path, archived: bool) -> anyhow::Result<Option<Session>> {
    let mut p = SessionParser::new(path.to_path_buf(), archived);
    p.parse_to_end()?;
    Ok(p.session)
}
