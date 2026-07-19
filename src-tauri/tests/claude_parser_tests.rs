use odometer_lib::claude_parser::{self, ClaudeSessionParser};
use odometer_lib::model::{Harness, TurnStatus};
use std::io::Write;
use std::path::PathBuf;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-session.jsonl")
}

#[test]
fn parses_session_identity() {
    let s = claude_parser::parse_file(&fixture()).unwrap().unwrap();
    assert_eq!(s.id, "11111111-2222-3333-4444-555555555555");
    assert_eq!(s.harness, Harness::ClaudeCode);
    assert_eq!(
        s.working_directory.as_deref(),
        Some("D:\\projects\\demo-app")
    );
    assert_eq!(s.cli_version.as_deref(), Some("2.1.209"));
    assert_eq!(s.originator.as_deref(), Some("cli"));
    assert_eq!(s.model_provider.as_deref(), Some("anthropic"));
    assert!(!s.archived);
    assert_eq!(s.started_at.to_rfc3339(), "2026-07-01T10:00:00+00:00");
    assert_eq!(s.last_event_at.to_rfc3339(), "2026-07-01T10:02:31+00:00");
}

#[test]
fn custom_title_becomes_thread_name() {
    let s = claude_parser::parse_file(&fixture()).unwrap().unwrap();
    assert_eq!(s.thread_name.as_deref(), Some("Healthcheck endpoint"));
}

#[test]
fn dedupes_streamed_usage_by_message_id() {
    // msg_aaa appears on two lines with identical usage; it must count once.
    let s = claude_parser::parse_file(&fixture()).unwrap().unwrap();
    let t = &s.tokens_total;
    // input = uncached + cache_read + cache_creation summed across the four
    // real messages (msg_aaa, msg_bbb, msg_side, msg_ccc).
    assert_eq!(t.input_tokens, 6_320);
    assert_eq!(t.cached_input_tokens, 5_600);
    assert_eq!(t.output_tokens, 155);
    assert_eq!(t.reasoning_output_tokens, 0);
    assert_eq!(t.total_tokens, 6_475);
    assert_eq!(s.tokens_history.len(), 4);
    // Last counted message (msg_ccc): input 40+50+2000 = 2090, output 60.
    assert_eq!(s.latest_context_tokens, Some(2_150));
}

#[test]
fn buckets_tokens_by_model_and_skips_synthetic() {
    let s = claude_parser::parse_file(&fixture()).unwrap().unwrap();
    assert_eq!(s.tokens_by_model.len(), 2);
    let opus = &s.tokens_by_model["claude-opus-4-8"];
    assert_eq!(opus.input_tokens, 6_110);
    assert_eq!(opus.output_tokens, 140);
    let haiku = &s.tokens_by_model["claude-haiku-4-5"];
    assert_eq!(haiku.input_tokens, 210);
    assert_eq!(haiku.output_tokens, 15);
    assert!(!s.tokens_by_model.contains_key("<synthetic>"));
    assert_eq!(s.model.as_deref(), Some("claude-opus-4-8"));
}

#[test]
fn derives_turns_from_user_prompts() {
    let s = claude_parser::parse_file(&fixture()).unwrap().unwrap();
    // Tool results, meta caveats, slash-command echoes, and sidechain prompts
    // must not open turns.
    assert_eq!(s.turns.len(), 2);
    assert_eq!(s.total_turns, 2);

    let t1 = &s.turns[0];
    assert_eq!(t1.index, 1);
    assert_eq!(t1.status, TurnStatus::Completed);
    assert_eq!(
        t1.user_message.as_deref(),
        Some("Add a healthcheck endpoint to the server")
    );
    assert_eq!(
        t1.last_agent_message.as_deref(),
        Some("Done. The healthcheck endpoint is live at /healthz.")
    );
    assert_eq!(t1.tokens.input_tokens, 4_020);
    assert_eq!(t1.tokens.output_tokens, 80);
    assert!(t1.duration_ms.is_some());

    let t2 = &s.turns[1];
    assert_eq!(t2.user_message.as_deref(), Some("Now write a test for it"));
    // Sidechain (subagent) usage is attributed to the enclosing turn.
    assert_eq!(t2.tokens.input_tokens, 2_300);
    assert_eq!(t2.tokens.output_tokens, 75);
    // But a sidechain reply must not become the turn's final message.
    assert_eq!(
        t2.last_agent_message.as_deref(),
        Some("Test added and passing.")
    );

    assert_eq!(
        s.first_user_message.as_deref(),
        Some("Add a healthcheck endpoint to the server")
    );
}

#[test]
fn incremental_append_with_partial_trailing_line() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("session.jsonl");
    let full = std::fs::read_to_string(fixture()).unwrap();
    let lines: Vec<&str> = full.lines().collect();

    let mut file = std::fs::File::create(&path).unwrap();
    // First two complete lines plus half of the third.
    write!(file, "{}\n{}\n{}", lines[0], lines[1], &lines[2][..40]).unwrap();
    file.flush().unwrap();

    let mut parser = ClaudeSessionParser::new(path.clone());
    parser.parse_to_end().unwrap();
    let s = parser.session.as_ref().unwrap();
    assert_eq!(s.tokens_history.len(), 0); // partial assistant line not consumed
    assert_eq!(s.turns.len(), 1);

    // Complete the file.
    writeln!(file, "{}", &lines[2][40..]).unwrap();
    for line in &lines[3..] {
        writeln!(file, "{}", line).unwrap();
    }
    file.flush().unwrap();

    parser.parse_to_end().unwrap();
    let s = parser.session.as_ref().unwrap();
    assert_eq!(s.tokens_total.total_tokens, 6_475);
    assert_eq!(s.turns.len(), 2);
    assert_eq!(s.thread_name.as_deref(), Some("Healthcheck endpoint"));
}

#[test]
fn subagent_transcript_gets_own_identity_and_parent_link() {
    // agent-*.jsonl files reuse the PARENT session's sessionId on every
    // record; they must not collide with the parent in the session map.
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/agent-fixture123.jsonl");
    let s = claude_parser::parse_file(&path).unwrap().unwrap();
    assert_eq!(s.id, "agent-fixture123");
    assert_eq!(
        s.parent_thread_id.as_deref(),
        Some("11111111-2222-3333-4444-555555555555")
    );
    assert_eq!(s.source.as_deref(), Some("subagent"));
    assert_eq!(s.harness, Harness::ClaudeCode);
    // Sidechain records form turns inside a subagent transcript.
    assert_eq!(s.turns.len(), 1);
    assert_eq!(
        s.first_user_message.as_deref(),
        Some("Search the codebase for the config loader")
    );
    assert_eq!(s.turns[0].status, TurnStatus::Completed);
    assert_eq!(
        s.turns[0].last_agent_message.as_deref(),
        Some("Found it in src/config.rs.")
    );
    assert_eq!(s.tokens_total.input_tokens, 540);
    assert_eq!(s.tokens_total.output_tokens, 20);
}

#[test]
fn parent_transcript_still_ignores_sidechain_prompts() {
    // The main fixture has an in-file sidechain prompt; it must not open a
    // turn there (only subagent transcript files waive the filter).
    let s = claude_parser::parse_file(&fixture()).unwrap().unwrap();
    assert!(s
        .turns
        .iter()
        .all(|t| t.user_message.as_deref() != Some("You are a subagent. Find the test file.")));
    assert!(s.parent_thread_id.is_none());
    assert!(s.source.is_none());
}
