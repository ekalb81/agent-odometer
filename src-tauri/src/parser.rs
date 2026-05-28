use crate::model::{Session, TokenHistoryPoint, TokenTotals};
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

        let source = payload
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_owned);

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

        self.session = Some(Session {
            id,
            thread_name,
            forked_from_id: None,
            file_path: self.file_path.to_string_lossy().into_owned(),
            archived: self.archived,
            started_at,
            last_event_at,
            working_directory: cwd,
            originator,
            source,
            cli_version,
            model_provider,
            model: None,
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

        if let Some(m) = &model {
            self.current_model = Some(m.clone());
        }
        if let Some(t) = &turn_id {
            self.current_turn_id = Some(t.clone());
        }

        if let Some(s) = self.session.as_mut() {
            if let Some(m) = &model {
                s.model = Some(m.clone());
            }
            if let Some(tid) = &turn_id {
                let idx = ensure_turn_index(s, tid);
                if model.is_some() {
                    s.turns[idx].model = model;
                }
            }
        }

        Ok(())
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
                if let Some(s) = self.session.as_mut() {
                    if let Some(cw) = cw {
                        s.context_window = Some(cw as u32);
                    }
                    if let Some(tid) = &turn_id {
                        let idx = ensure_turn_index(s, tid);
                        if s.turns[idx].started_at.is_none() {
                            s.turns[idx].started_at = Some(event_ts);
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
                if let Some(s) = self.session.as_mut() {
                    s.total_turns += 1;
                    if let Some(tid) = &turn_id {
                        let idx = ensure_turn_index(s, tid);
                        let turn = &mut s.turns[idx];
                        turn.completed_at = Some(event_ts);
                        turn.duration_ms = duration_ms;
                        turn.time_to_first_token_ms = ttft;
                        if turn.last_agent_message.is_none() {
                            turn.last_agent_message = last_agent;
                        }
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
                    total_tokens: total.total_tokens,
                    delta: contribution.clone(),
                });
            }

            if let Some(tid) = &turn_id {
                if let Some(turn) = s.turns.iter_mut().find(|t| &t.turn_id == tid) {
                    add_token_totals(&mut turn.tokens, &contribution);
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
