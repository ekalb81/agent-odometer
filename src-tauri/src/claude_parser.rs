//! Incremental parser for Claude Code session JSONL files
//! (`~/.claude/projects/<project>/<session-uuid>.jsonl`).
//!
//! Unlike Codex rollouts there is no `session_meta` envelope: every line is a
//! self-describing record (`user`, `assistant`, `system`, `custom-title`, …)
//! carrying `sessionId`, `timestamp`, `cwd`, and `version` fields. Assistant
//! records embed the Anthropic API message, including `message.usage`.
//!
//! Two invariants specific to this format:
//! - A streamed assistant message is written as several JSONL lines that share
//!   one `message.id` and repeat the same usage snapshot. Usage must be
//!   counted once per `message.id` or totals roughly double.
//! - Anthropic usage reports `input_tokens` EXCLUDING cache reads/writes,
//!   while the viewer's `TokenTotals` convention (from Codex) treats cached
//!   input as a subset of input. We therefore map:
//!   input = input + cache_read + cache_creation, cached = cache_read.
//!   Cache-creation tokens are priced at the ordinary input rate, a slight
//!   undercount of Anthropic's 1.25x cache-write premium.

use crate::model::{Harness, Session, TokenHistoryPoint, TokenTotals, TurnStatus};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Max characters retained for per-turn prompt / agent-message previews.
const TURN_MESSAGE_LIMIT: usize = 500;

pub struct ClaudeSessionParser {
    pub session: Option<Session>,
    pub byte_offset: u64,
    pub file_path: PathBuf,
    /// Assistant `message.id`s whose usage has already been counted.
    seen_message_ids: HashSet<String>,
    /// uuid of the user prompt that opened the current turn.
    current_turn_id: Option<String>,
    /// Thread name seen before the session object existed.
    pending_thread_name: Option<String>,
}

impl ClaudeSessionParser {
    pub fn new(file_path: PathBuf) -> Self {
        Self {
            session: None,
            byte_offset: 0,
            file_path,
            seen_message_ids: HashSet::new(),
            current_turn_id: None,
            pending_thread_name: None,
        }
    }

    pub fn parse_to_end(&mut self) -> anyhow::Result<bool> {
        let file = std::fs::File::open(&self.file_path)?;
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
        let root: Value = serde_json::from_str(line)?;

        let record_type = root.get("type").and_then(Value::as_str).unwrap_or("");

        // Timestamp is absent on some record types (custom-title, last-prompt);
        // never fall back to "now" — only parsed timestamps move the clock.
        let timestamp: Option<DateTime<Utc>> = root
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok());

        self.ensure_session(&root, timestamp);

        if let (Some(s), Some(ts)) = (self.session.as_mut(), timestamp) {
            if ts > s.last_event_at {
                s.last_event_at = ts;
            }
        }

        match record_type {
            "user" => self.handle_user(&root, timestamp),
            "assistant" => self.handle_assistant(&root, timestamp),
            "custom-title" => {
                let title = root
                    .get("customTitle")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(title) = title {
                    match self.session.as_mut() {
                        Some(s) => s.thread_name = Some(title),
                        None => self.pending_thread_name = Some(title),
                    }
                }
            }
            "summary" => {
                // Continuation summaries; use as a name only when nothing better exists.
                let summary = root
                    .get("summary")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if let Some(summary) = summary {
                    match self.session.as_mut() {
                        Some(s) => {
                            if s.thread_name.is_none() {
                                s.thread_name = Some(summary);
                            }
                        }
                        None => {
                            if self.pending_thread_name.is_none() {
                                self.pending_thread_name = Some(summary);
                            }
                        }
                    }
                }
            }
            // queue-operation, attachment, system, mode, pr-link, last-prompt,
            // file-history-snapshot, progress, … carry no aggregate data.
            _ => {}
        }

        Ok(())
    }

    /// Creates the Session lazily: the first record carrying a parseable
    /// timestamp establishes identity and started_at.
    fn ensure_session(&mut self, root: &Value, timestamp: Option<DateTime<Utc>>) {
        if self.session.is_some() {
            self.refresh_metadata(root);
            return;
        }
        let Some(started_at) = timestamp else {
            return;
        };

        let id = root
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                self.file_path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        if id.is_empty() {
            return;
        }

        self.session = Some(Session {
            id,
            harness: Harness::ClaudeCode,
            thread_name: self.pending_thread_name.take(),
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: self.file_path.to_string_lossy().into_owned(),
            archived: false,
            started_at,
            last_event_at: started_at,
            working_directory: None,
            originator: None,
            source: None,
            history_mode: None,
            memory_mode: None,
            cli_version: None,
            model_provider: Some("anthropic".into()),
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
        self.refresh_metadata(root);
    }

    /// Picks up cwd / CLI version / entrypoint from whichever record first
    /// carries them (most records repeat these fields).
    fn refresh_metadata(&mut self, root: &Value) {
        let Some(s) = self.session.as_mut() else {
            return;
        };
        if s.working_directory.is_none() {
            s.working_directory = root
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_owned);
        }
        if s.cli_version.is_none() {
            s.cli_version = root
                .get("version")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if s.originator.is_none() {
            s.originator = root
                .get("entrypoint")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }

    fn handle_user(&mut self, root: &Value, timestamp: Option<DateTime<Utc>>) {
        let Some(prompt) = extract_user_prompt(root) else {
            return;
        };
        let turn_id = root
            .get("uuid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if turn_id.is_empty() {
            return;
        }

        let truncated: String = prompt.trim_end().chars().take(TURN_MESSAGE_LIMIT).collect();

        let Some(s) = self.session.as_mut() else {
            return;
        };

        // A new prompt closes the previous turn; a turn that never saw an
        // assistant response was interrupted before the agent replied.
        if let Some(prev_id) = self.current_turn_id.take() {
            if let Some(prev) = s.turns.iter_mut().find(|t| t.turn_id == prev_id) {
                if prev.status == TurnStatus::InProgress {
                    prev.status = TurnStatus::Aborted;
                }
            }
        }

        if s.first_user_message.is_none() {
            s.first_user_message = Some(truncated.chars().take(200).collect());
        }

        let index = s.turns.len() as u32 + 1;
        s.turns.push(crate::model::TurnInfo {
            turn_id: turn_id.clone(),
            index,
            started_at: timestamp,
            user_message: Some(truncated),
            ..Default::default()
        });
        self.current_turn_id = Some(turn_id);
    }

    fn handle_assistant(&mut self, root: &Value, timestamp: Option<DateTime<Utc>>) {
        let Some(message) = root.get("message") else {
            return;
        };
        let model = message
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned);
        // "<synthetic>" is an error placeholder, not a real API call.
        if model.as_deref() == Some("<synthetic>") {
            return;
        }
        let is_sidechain = root
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let message_id = message
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        // Streamed content blocks repeat the same message id with identical
        // usage — count only the first occurrence.
        let first_occurrence = !message_id.is_empty() && self.seen_message_ids.insert(message_id);

        let usage = message.get("usage");
        let service_tier = usage.and_then(|u| {
            let speed = u.get("speed").and_then(Value::as_str);
            match speed {
                Some(s) if s != "standard" => Some(s.to_owned()),
                _ => u
                    .get("service_tier")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        });
        let last_agent_message = last_text_block(message);

        let Some(s) = self.session.as_mut() else {
            return;
        };

        if let Some(model) = &model {
            s.model = Some(model.clone());
        }
        if service_tier.is_some() {
            s.service_tier = service_tier.clone();
        }

        if first_occurrence {
            if let Some(delta) = usage.map(usage_to_totals) {
                if totals_any_positive(&delta) {
                    add_token_totals(&mut s.tokens_total, &delta);
                    if let Some(model) = &model {
                        let entry = s.tokens_by_model.entry(model.clone()).or_default();
                        add_token_totals(entry, &delta);
                    }
                    if let Some(ts) = timestamp {
                        s.tokens_history.push(TokenHistoryPoint {
                            timestamp: ts,
                            model: model.clone(),
                            service_tier: service_tier.clone(),
                            total_tokens: s.tokens_total.total_tokens,
                            delta: delta.clone(),
                        });
                    }
                    if let Some(tid) = &self.current_turn_id {
                        if let Some(turn) = s.turns.iter_mut().find(|t| &t.turn_id == tid) {
                            add_token_totals(&mut turn.tokens, &delta);
                        }
                    }
                }
            }
        }

        // Update the active turn's lifecycle regardless of usage dedup.
        if let Some(tid) = &self.current_turn_id {
            if let Some(turn) = s.turns.iter_mut().find(|t| &t.turn_id == tid) {
                if model.is_some() {
                    turn.model = model;
                }
                if service_tier.is_some() {
                    turn.service_tier = service_tier;
                }
                if turn.status == TurnStatus::InProgress {
                    turn.status = TurnStatus::Completed;
                    s.total_turns += 1;
                }
                if let Some(ts) = timestamp {
                    turn.completed_at = Some(ts);
                    if let Some(started) = turn.started_at {
                        turn.duration_ms = (ts - started).num_milliseconds().try_into().ok();
                    }
                }
                if !is_sidechain {
                    if let Some(text) = last_agent_message {
                        turn.last_agent_message = Some(text);
                    }
                }
            }
        }
    }
}

/// Returns the prompt text when a `user` record is a real human prompt:
/// not a tool result, not meta/caveat noise, not subagent (sidechain)
/// traffic, and not local slash-command echo.
fn extract_user_prompt(root: &Value) -> Option<String> {
    if root.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if root.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if root.get("toolUseResult").is_some_and(|v| !v.is_null()) {
        return None;
    }
    let content = root.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            if blocks
                .iter()
                .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            {
                return None;
            }
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            texts.join("\n")
        }
        _ => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty()
        || trimmed.starts_with("<command-")
        || trimmed.starts_with("<local-command")
        || trimmed.starts_with("[Request interrupted")
        || trimmed.starts_with("Caveat:")
    {
        return None;
    }
    Some(text)
}

/// Truncated text of the last text block in an assistant message, if any.
fn last_text_block(message: &Value) -> Option<String> {
    let blocks = message.get("content")?.as_array()?;
    blocks
        .iter()
        .rev()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .and_then(|b| b.get("text").and_then(Value::as_str))
        .map(|t| t.trim().chars().take(TURN_MESSAGE_LIMIT).collect())
}

/// Maps Anthropic usage fields onto the viewer's Codex-style TokenTotals,
/// where cached input is a subset of input (see module docs).
fn usage_to_totals(usage: &Value) -> TokenTotals {
    let field = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let uncached_input = field("input_tokens");
    let cache_read = field("cache_read_input_tokens");
    let cache_creation = field("cache_creation_input_tokens");
    let output = field("output_tokens");
    let input = uncached_input + cache_read + cache_creation;
    TokenTotals {
        input_tokens: input,
        cached_input_tokens: cache_read,
        output_tokens: output,
        // Anthropic bills thinking as output and does not break it out.
        reasoning_output_tokens: 0,
        total_tokens: input + output,
    }
}

fn add_token_totals(dst: &mut TokenTotals, src: &TokenTotals) {
    dst.input_tokens += src.input_tokens;
    dst.cached_input_tokens += src.cached_input_tokens;
    dst.output_tokens += src.output_tokens;
    dst.reasoning_output_tokens += src.reasoning_output_tokens;
    dst.total_tokens += src.total_tokens;
}

fn totals_any_positive(t: &TokenTotals) -> bool {
    t.input_tokens > 0
        || t.cached_input_tokens > 0
        || t.output_tokens > 0
        || t.reasoning_output_tokens > 0
        || t.total_tokens > 0
}

pub fn parse_file(path: &Path) -> anyhow::Result<Option<Session>> {
    let mut p = ClaudeSessionParser::new(path.to_path_buf());
    p.parse_to_end()?;
    Ok(p.session)
}
