use crate::model::{Session, TokenTotals};
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
    pub file_path: PathBuf,
    pub archived: bool,
}

impl SessionParser {
    pub fn new(file_path: PathBuf, archived: bool) -> Self {
        Self {
            session: None,
            byte_offset: 0,
            current_model: None,
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
        let last_event_at: DateTime<Utc> = timestamp_str
            .parse()
            .unwrap_or_else(|_| Utc::now());

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
            "event_msg" => self.handle_event_msg(payload)?,
            _ => {}
        }

        Ok(())
    }

    fn handle_session_meta(&mut self, payload: Value, last_event_at: DateTime<Utc>) -> anyhow::Result<()> {
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

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
        });

        Ok(())
    }

    fn handle_turn_context(&mut self, payload: Value) -> anyhow::Result<()> {
        let model = payload
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);

        if let Some(m) = &model {
            self.current_model = Some(m.clone());
        }

        if let Some(s) = self.session.as_mut() {
            if let Some(m) = model {
                s.model = Some(m);
            }
        }

        Ok(())
    }

    fn handle_event_msg(&mut self, payload: Value) -> anyhow::Result<()> {
        let msg_type = payload
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();

        match msg_type.as_str() {
            "task_started" => {
                if let Some(cw) = payload.get("model_context_window").and_then(Value::as_u64) {
                    if let Some(s) = self.session.as_mut() {
                        s.context_window = Some(cw as u32);
                    }
                }
            }
            "user_message" => {
                if let Some(s) = self.session.as_mut() {
                    if s.first_user_message.is_none() {
                        if let Some(msg) = payload.get("message").and_then(Value::as_str) {
                            let trimmed = msg.trim_end();
                            let truncated: String = trimmed.chars().take(200).collect();
                            s.first_user_message = Some(truncated);
                        }
                    }
                }
            }
            "token_count" => {
                self.handle_token_count(payload)?;
            }
            "task_complete" => {
                if let Some(s) = self.session.as_mut() {
                    s.total_turns += 1;
                }
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_token_count(&mut self, payload: Value) -> anyhow::Result<()> {
        let info = match payload.get("info") {
            Some(v) if !v.is_null() => v,
            // The first token_count event in a session commonly has info: null.
            _ => return Ok(()),
        };

        if let Some(s) = self.session.as_mut() {
            if let Some(total) = info.get("total_token_usage") {
                s.tokens_total = parse_token_totals(total);
            }

            if let Some(cw) = info.get("model_context_window").and_then(Value::as_u64) {
                s.context_window = Some(cw as u32);
            }

            // last_token_usage is the delta for the most recent API call, used for per-model attribution.
            if let (Some(last), Some(model)) = (
                info.get("last_token_usage"),
                self.current_model.as_ref(),
            ) {
                let delta = parse_token_totals(last);
                let entry = s.tokens_by_model.entry(model.clone()).or_default();
                add_token_totals(entry, &delta);
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

fn parse_token_totals(v: &Value) -> TokenTotals {
    TokenTotals {
        input_tokens: v.get("input_tokens").and_then(Value::as_u64).unwrap_or(0),
        cached_input_tokens: v.get("cached_input_tokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: v.get("output_tokens").and_then(Value::as_u64).unwrap_or(0),
        reasoning_output_tokens: v.get("reasoning_output_tokens").and_then(Value::as_u64).unwrap_or(0),
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

pub fn parse_file(path: &Path, archived: bool) -> anyhow::Result<Option<Session>> {
    let mut p = SessionParser::new(path.to_path_buf(), archived);
    p.parse_to_end()?;
    Ok(p.session)
}
