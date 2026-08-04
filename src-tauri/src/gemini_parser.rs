//! Incremental parser for Gemini CLI session logs
//! (`~/.gemini/tmp/<project-hash>/chats/session-*.jsonl`).
//!
//! Only the JSONL session format (Gemini CLI >= 0.39) is supported; see
//! `provider::GEMINI_CLI_DESCRIPTOR` for why the earlier single-JSON-document
//! format is deliberately out of scope. The shape below is not documented by
//! Google as a stable on-disk schema; it is reconstructed from Gemini CLI's
//! own session-management documentation plus two independent third-party
//! local-usage tools (`ccusage`, `codeburn`) that read real installs. Every
//! field this parser reads is corroborated by at least one of those sources;
//! see the issue #40 PR body for citations. Fields those tools observe but do
//! not attribute a stable meaning to (`tokens.tool`) are intentionally left
//! unread rather than guessed at.
//!
//! Two invariants specific to this format:
//! - The first line is a header record (`sessionId`, `startTime`, and
//!   optionally `projectHash`/`lastUpdated`/`kind`) with no `type` field.
//!   Every later line is a message record (`id`, `timestamp`, `type`) with no
//!   `sessionId`/`startTime`. A session has no identity until its header line
//!   has been read in full.
//! - Gemini's `tokens.input` is the gross prompt-token count and already
//!   INCLUDES `tokens.cached`, matching the viewer's convention that cached
//!   input is a subset of input (same mapping already used for Claude Code).
//!   `tokens.thoughts` (extended-thinking tokens) is a subset of output and
//!   billed at the output rate, so it is folded into `output_tokens` and also
//!   reported as `reasoning_output_tokens`. Gemini has no cache-creation
//!   ("cache write") token dimension, so that field is always 0.

use crate::model::{
    storage_id_for_session, Session, SourceAvailability, TokenHistoryPoint, TokenTotals, TurnStatus,
};
use crate::provider::gemini_cli_provider_id;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Max characters retained for per-turn prompt previews, matching the limit
/// Claude Code's parser uses for the same purpose.
const TURN_MESSAGE_LIMIT: usize = 500;

pub struct GeminiSessionParser {
    pub session: Option<Session>,
    pub byte_offset: u64,
    pub file_path: PathBuf,
    /// Message `id`s whose usage has already been counted.
    seen_message_ids: HashSet<String>,
    /// Tool-call `id`s already observed (Gemini logs one record per call, so
    /// this guards against a line being re-applied after a watcher restart).
    seen_tool_ids: HashSet<String>,
    /// uuid of the user prompt that opened the current turn.
    current_turn_id: Option<String>,
}

impl GeminiSessionParser {
    pub fn new(file_path: PathBuf) -> Self {
        Self {
            session: None,
            byte_offset: 0,
            file_path,
            seen_message_ids: HashSet::new(),
            seen_tool_ids: HashSet::new(),
            current_turn_id: None,
        }
    }

    pub fn parse_to_end(&mut self) -> anyhow::Result<bool> {
        let file = std::fs::File::open(&self.file_path)?;
        if file.metadata()?.len() < self.byte_offset {
            self.reset_for_replaced_file();
        }
        let mut reader = BufReader::new(file);
        reader.seek(SeekFrom::Start(self.byte_offset))?;

        let mut updated = false;
        let mut line = Vec::new();

        loop {
            let start = self.byte_offset;
            line.clear();
            let n = reader.read_until(b'\n', &mut line)?;
            if n == 0 {
                break;
            }

            // A partial trailing line has no terminating newline — leave
            // byte_offset at its start so the next call re-reads it whole.
            if !line.ends_with(b"\n") {
                break;
            }

            self.byte_offset = start + n as u64;

            let trimmed = match std::str::from_utf8(&line) {
                Ok(line) => line.trim(),
                Err(error) => {
                    tracing::warn!("skipping non-UTF-8 line at offset {}: {}", start, error);
                    continue;
                }
            };
            if trimmed.is_empty() {
                continue;
            }

            match self.apply_line(trimmed) {
                Ok(()) => updated = true,
                Err(e) => tracing::warn!("skipping unparseable line at offset {}: {}", start, e),
            }
        }

        if updated {
            if let Some(session) = self.session.as_mut() {
                crate::telemetry::refresh_session(session);
            }
        }
        Ok(updated)
    }

    fn reset_for_replaced_file(&mut self) {
        self.session = None;
        self.byte_offset = 0;
        self.seen_message_ids.clear();
        self.seen_tool_ids.clear();
        self.current_turn_id = None;
    }

    fn apply_line(&mut self, line: &str) -> anyhow::Result<()> {
        let root: Value = serde_json::from_str(line)?;

        let has_type = root.get("type").and_then(Value::as_str);
        let has_header_fields = root.get("sessionId").and_then(Value::as_str).is_some()
            && root.get("startTime").is_some();

        if self.session.is_none() {
            if has_type.is_none() && has_header_fields {
                self.ensure_session(&root);
            }
            return Ok(());
        }

        let Some(record_type) = has_type else {
            return Ok(());
        };

        let timestamp: Option<DateTime<Utc>> = root
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| s.parse().ok());

        if let (Some(s), Some(ts)) = (self.session.as_mut(), timestamp) {
            if ts > s.last_event_at {
                s.last_event_at = ts;
            }
        }

        match record_type {
            "user" => self.handle_user(&root, timestamp),
            "gemini" => self.handle_gemini(&root, timestamp),
            // "info" and any future record types carry no aggregate data.
            _ => {}
        }

        Ok(())
    }

    fn ensure_session(&mut self, header: &Value) {
        let Some(session_id) = header.get("sessionId").and_then(Value::as_str) else {
            return;
        };
        if session_id.is_empty() {
            return;
        }
        let Some(started_at) = header
            .get("startTime")
            .and_then(Value::as_str)
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        else {
            return;
        };

        self.session = Some(Session {
            id: session_id.to_owned(),
            storage_id: storage_id_for_session(&gemini_cli_provider_id(), session_id),
            harness: gemini_cli_provider_id(),
            thread_name: None,
            forked_from_id: None,
            parent_thread_id: None,
            agent_path: None,
            agent_nickname: None,
            file_path: self.file_path.to_string_lossy().into_owned(),
            source_availability: SourceAvailability::Present,
            archived: false,
            started_at,
            last_event_at: started_at,
            // Gemini CLI's session log does not record the working
            // directory. `projectHash` is an opaque hash of the project
            // root, not a decodable path, so it cannot fill this field
            // either — left None rather than guessed at.
            working_directory: None,
            originator: None,
            source: None,
            subagent_id_is_path_fallback: false,
            history_mode: None,
            memory_mode: None,
            // Gemini CLI does not stamp a CLI version on session records.
            cli_version: None,
            model_provider: Some("google".into()),
            model: None,
            service_tier: None,
            plan_type: None,
            credits_unlimited: None,
            credits_balance: None,
            context_window: None,
            latest_context_tokens: None,
            total_turns: 0,
            first_user_message: None,
            tokens_total: TokenTotals::default(),
            tokens_by_model: Default::default(),
            tokens_history: Vec::new(),
            rate_limits_history: Vec::new(),
            turns: Vec::new(),
            tool_observations: Vec::new(),
            tool_metrics: Default::default(),
            tool_metrics_by_model: Default::default(),
            category_totals: Default::default(),
            optimization_findings: Vec::new(),
            project_key: None,
            project_label: None,
            project_provenance: None,
        });
    }

    fn handle_user(&mut self, root: &Value, timestamp: Option<DateTime<Utc>>) {
        let Some(prompt) = user_message_text(root) else {
            return;
        };
        let turn_id = root
            .get("id")
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

        // A new prompt closes the previous turn; one that never saw a
        // "gemini" response was interrupted before the agent replied.
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

    fn handle_gemini(&mut self, root: &Value, timestamp: Option<DateTime<Utc>>) {
        let model = root.get("model").and_then(Value::as_str).map(str::to_owned);
        let message_id = root
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let first_occurrence = !message_id.is_empty() && self.seen_message_ids.insert(message_id);

        let last_agent_message = last_text(root);
        let tool_calls = tool_calls_of(root);

        let Some(s) = self.session.as_mut() else {
            return;
        };

        if let Some(model) = &model {
            s.model = Some(model.clone());
        }

        if let Some(ts) = timestamp {
            for (call_id, name, args, status) in tool_calls {
                if call_id.is_empty() || !self.seen_tool_ids.insert(call_id.clone()) {
                    continue;
                }
                crate::telemetry::observe_call(
                    &mut s.tool_observations,
                    crate::telemetry::ToolCallInput {
                        call_id: call_id.clone(),
                        turn_id: self.current_turn_id.clone(),
                        harness: gemini_cli_provider_id(),
                        model: model.clone(),
                        timestamp: ts,
                        name,
                        arguments: &args,
                    },
                );
                // Gemini logs each tool call as one already-settled record
                // (no separate call/result pairing), so the outcome is
                // known immediately.
                crate::telemetry::observe_result(
                    &mut s.tool_observations,
                    &call_id,
                    outcome_from_status(status.as_deref()),
                    None,
                    Some(ts),
                    0,
                );
            }
        }

        if first_occurrence && tokens_any_positive(root) {
            if let (Some(delta), Some(ts)) = (usage_to_totals(root), timestamp) {
                crate::model::add_totals(&mut s.tokens_total, &delta);
                s.latest_context_tokens = Some(delta.input_tokens + delta.output_tokens);
                if let Some(model) = &model {
                    let entry = s.tokens_by_model.entry(model.clone()).or_default();
                    crate::model::add_totals(entry, &delta);
                }
                s.tokens_history.push(TokenHistoryPoint {
                    timestamp: ts,
                    model: model.clone(),
                    service_tier: None,
                    request_input_tokens: Some(delta.input_tokens),
                    total_tokens: s.tokens_total.total_tokens,
                    delta: delta.clone(),
                });
                if let Some(tid) = &self.current_turn_id {
                    if let Some(turn) = s.turns.iter_mut().find(|t| &t.turn_id == tid) {
                        crate::model::add_totals(&mut turn.tokens, &delta);
                    }
                }
            }
        }

        // Update the active turn's lifecycle regardless of whether this
        // record carried new usage.
        if let Some(tid) = &self.current_turn_id {
            if let Some(turn) = s.turns.iter_mut().find(|t| &t.turn_id == tid) {
                if model.is_some() {
                    turn.model = model;
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
                if let Some(text) = last_agent_message {
                    turn.last_agent_message = Some(text);
                }
            }
        }
    }
}

/// Extracts the plain-text prompt from a `user` record's `content`, which is
/// either a bare string or an array of `{"text": "..."}` parts.
fn user_message_text(root: &Value) -> Option<String> {
    match root.get("content")? {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Array(parts) => {
            let joined = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.trim().is_empty()).then_some(joined)
        }
        _ => None,
    }
}

/// Truncated text of a `gemini` record's `content`, same shape as the user
/// message content field.
fn last_text(root: &Value) -> Option<String> {
    let text = user_message_text(root)?;
    Some(text.trim().chars().take(TURN_MESSAGE_LIMIT).collect())
}

fn tool_calls_of(root: &Value) -> Vec<(String, String, Value, Option<String>)> {
    root.get("toolCalls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|call| {
            let id = call.get("id").and_then(Value::as_str)?.to_owned();
            let name = call
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let args = call.get("args").cloned().unwrap_or(Value::Null);
            let status = call
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_owned);
            Some((id, name, args, status))
        })
        .collect()
}

fn outcome_from_status(status: Option<&str>) -> crate::model::ToolOutcome {
    match status.map(str::to_ascii_lowercase).as_deref() {
        None => crate::model::ToolOutcome::Unknown,
        Some(s) if s.contains("error") || s.contains("fail") || s.contains("cancel") => {
            crate::model::ToolOutcome::Failure
        }
        Some(_) => crate::model::ToolOutcome::Success,
    }
}

fn tokens_field(tokens: &Value, key: &str) -> u64 {
    tokens.get(key).and_then(Value::as_u64).unwrap_or(0)
}

fn tokens_any_positive(root: &Value) -> bool {
    let Some(tokens) = root.get("tokens") else {
        return false;
    };
    tokens_field(tokens, "input") > 0
        || tokens_field(tokens, "output") > 0
        || tokens_field(tokens, "cached") > 0
        || tokens_field(tokens, "thoughts") > 0
}

/// Maps Gemini's token fields onto the viewer's TokenTotals convention.
/// `input` is already gross (inclusive of `cached`); `thoughts` is a subset
/// of output and billed at the output rate, so it is folded into
/// `output_tokens` and separately reported as `reasoning_output_tokens`.
/// Gemini has no cache-creation dimension, so that field is always 0. The
/// `tool` field Gemini also reports has no corroborated meaning across the
/// sources this parser is built from, so it is intentionally not read.
fn usage_to_totals(root: &Value) -> Option<TokenTotals> {
    let tokens = root.get("tokens")?;
    let input = tokens_field(tokens, "input");
    let cached = tokens_field(tokens, "cached");
    let thoughts = tokens_field(tokens, "thoughts");
    let output = tokens_field(tokens, "output") + thoughts;
    Some(TokenTotals {
        input_tokens: input,
        cached_input_tokens: cached,
        cache_creation_input_tokens: 0,
        output_tokens: output,
        reasoning_output_tokens: thoughts,
        total_tokens: input + output,
    })
}

pub fn parse_file(path: &Path) -> anyhow::Result<Option<Session>> {
    let mut parser = GeminiSessionParser::new(path.to_path_buf());
    parser.parse_to_end()?;
    Ok(parser.session)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_lines(path: &Path, lines: &[&str]) {
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }

    const HEADER: &str = r#"{"sessionId":"gsess-1","startTime":"2026-01-01T00:00:00Z","projectHash":"abc123","kind":"chat"}"#;
    const USER_TURN: &str = r#"{"id":"u1","timestamp":"2026-01-01T00:00:01Z","type":"user","content":"Add a helper function"}"#;
    const GEMINI_TURN: &str = r#"{"id":"g1","timestamp":"2026-01-01T00:00:02Z","type":"gemini","model":"gemini-2.5-pro","content":"Done.","tokens":{"input":120,"output":40,"cached":20,"thoughts":10,"tool":5,"total":170},"toolCalls":[{"id":"call-1","name":"write_file","args":{"path":"a.rs"},"status":"success"}]}"#;

    #[test]
    fn parses_header_turn_and_usage_with_cached_and_thoughts_subsets() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-gsess-1.jsonl");
        write_lines(&path, &[HEADER, USER_TURN, GEMINI_TURN]);

        let session = parse_file(&path).unwrap().unwrap();
        assert_eq!(session.id, "gsess-1");
        assert_eq!(session.harness, gemini_cli_provider_id());
        assert_eq!(session.model.as_deref(), Some("gemini-2.5-pro"));
        assert_eq!(session.total_turns, 1);
        assert_eq!(session.turns.len(), 1);
        assert_eq!(session.turns[0].status, TurnStatus::Completed);

        let tokens = &session.tokens_total;
        assert_eq!(tokens.input_tokens, 120);
        assert_eq!(tokens.cached_input_tokens, 20);
        assert_eq!(tokens.cache_creation_input_tokens, 0);
        assert_eq!(tokens.output_tokens, 50); // 40 + 10 thoughts
        assert_eq!(tokens.reasoning_output_tokens, 10);
        assert_eq!(tokens.total_tokens, 170);

        assert_eq!(session.tool_observations.len(), 1);
        assert_eq!(
            session.tool_observations[0].outcome,
            crate::model::ToolOutcome::Success
        );
    }

    #[test]
    fn streamed_repeated_message_id_is_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-gsess-2.jsonl");
        let header = r#"{"sessionId":"gsess-2","startTime":"2026-01-01T00:00:00Z"}"#;
        let user = r#"{"id":"u1","timestamp":"2026-01-01T00:00:01Z","type":"user","content":"hi"}"#;
        let reply = r#"{"id":"g1","timestamp":"2026-01-01T00:00:02Z","type":"gemini","model":"gemini-2.5-flash","tokens":{"input":10,"output":5}}"#;
        write_lines(&path, &[header, user, reply, reply]);

        let session = parse_file(&path).unwrap().unwrap();
        assert_eq!(session.tokens_total.input_tokens, 10);
        assert_eq!(session.tokens_history.len(), 1);
    }

    #[test]
    fn incremental_parse_matches_full_parse_and_tracks_partial_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-gsess-3.jsonl");
        write_lines(&path, &[HEADER, USER_TURN, GEMINI_TURN]);
        let full = parse_file(&path).unwrap().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let first_newline = bytes.iter().position(|b| *b == b'\n').unwrap();
        // Truncate mid-header (before its terminating newline): no complete
        // record has been written yet, so no session should exist.
        std::fs::write(&path, &bytes[..first_newline - 5]).unwrap();
        let mut parser = GeminiSessionParser::new(path.clone());
        parser.parse_to_end().unwrap();
        assert!(
            parser.session.is_none(),
            "partial header line yields no session yet"
        );

        std::fs::write(&path, &bytes).unwrap();
        parser.parse_to_end().unwrap();
        let incremental = parser.session.unwrap();
        assert_eq!(incremental.tokens_total, full.tokens_total);
        assert_eq!(incremental.tokens_history, full.tokens_history);
        assert_eq!(incremental.total_turns, full.total_turns);
    }

    #[test]
    fn info_records_and_missing_content_are_ignored_without_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session-gsess-4.jsonl");
        let info =
            r#"{"id":"i1","timestamp":"2026-01-01T00:00:01Z","type":"info","content":"connected"}"#;
        write_lines(&path, &[HEADER, info]);
        let session = parse_file(&path).unwrap().unwrap();
        assert_eq!(session.total_turns, 0);
        assert_eq!(session.tokens_total.total_tokens, 0);
    }
}
