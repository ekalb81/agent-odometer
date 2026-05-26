use codex_data_viewer_lib::parser;
use codex_data_viewer_lib::model::TokenTotals;
use std::path::PathBuf;


fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-session.jsonl")
}

#[test]
fn parses_session_identity() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    assert_eq!(s.id, "019e2ba6-95be-7bd2-a255-238cdf02936c");
    assert_eq!(s.originator.as_deref(), Some("Codex Desktop"));
    assert_eq!(s.source.as_deref(), Some("vscode"));
    assert_eq!(s.cli_version.as_deref(), Some("0.130.0-alpha.5"));
    assert_eq!(s.model_provider.as_deref(), Some("openai"));
    assert!(s.working_directory.as_deref().unwrap().contains("summarize-my-codex-usage-for-may"));
    assert_eq!(s.started_at.to_rfc3339(), "2026-05-15T12:39:58.142+00:00");
    assert_eq!(s.thread_name, None); // not present in this fixture
}

#[test]
fn parses_active_model_from_turn_context() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    assert_eq!(s.model.as_deref(), Some("gpt-5.5"));
}

#[test]
fn parses_first_user_message_truncated() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    let m = s.first_user_message.unwrap();
    assert!(m.starts_with("Summarize my codex usage for May 7th"));
    assert!(!m.ends_with('\n'));   // trailing whitespace trimmed
    assert!(m.chars().count() <= 200);
}

#[test]
fn parses_tokens_total_from_latest_token_count() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    let t = &s.tokens_total;
    assert_eq!(t.input_tokens,            547_081);
    assert_eq!(t.cached_input_tokens,     429_696);
    assert_eq!(t.output_tokens,             5_812);
    assert_eq!(t.reasoning_output_tokens,   2_018);
    assert_eq!(t.total_tokens,            552_893);
}

#[test]
fn attributes_tokens_to_active_model() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    // All work in this session ran under gpt-5.5
    assert!(s.tokens_by_model.contains_key("gpt-5.5"));
    let per_model = &s.tokens_by_model["gpt-5.5"];
    // Sum of last_token_usage deltas should match total_token_usage exactly.
    assert_eq!(per_model.input_tokens,            547_081);
    assert_eq!(per_model.cached_input_tokens,     429_696);
    assert_eq!(per_model.output_tokens,             5_812);
    assert_eq!(per_model.reasoning_output_tokens,   2_018);
    assert_eq!(per_model.total_tokens,            552_893);
}

#[test]
fn captures_plan_and_credits_and_context_window() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    assert_eq!(s.plan_type.as_deref(), Some("business"));
    assert_eq!(s.credits_unlimited, Some(true));
    assert_eq!(s.credits_balance, None);
    assert_eq!(s.context_window, Some(258_400));
}

#[test]
fn counts_turns() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    assert_eq!(s.total_turns, 1);
}

#[test]
fn first_token_count_event_with_null_info_does_not_crash() {
    // The fixture's first token_count event has info: null — parser must
    // handle it gracefully. Covered transitively by other tests but we
    // assert it explicitly: parse_file succeeded => didn't crash.
    let s = parser::parse_file(&fixture(), false).unwrap();
    assert!(s.is_some());
}

#[test]
fn archived_flag_is_propagated() {
    let s = parser::parse_file(&fixture(), true).unwrap().unwrap();
    assert!(s.archived);
}

#[test]
fn incremental_parsing_matches_full_parse() {
    // Read the fixture in two halves to exercise byte_offset resumption.
    let path = fixture();
    let bytes = std::fs::read(&path).unwrap();
    let half = bytes.len() / 2;
    // Find the next newline AT OR AFTER the halfway point to split on a line boundary.
    let split = bytes[half..].iter().position(|&b| b == b'\n').map(|p| half + p + 1).unwrap();

    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), &bytes[..split]).unwrap();

    let mut p = parser::SessionParser::new(tmp.path().to_path_buf(), false);
    p.parse_to_end().unwrap();
    let mid_offset = p.byte_offset;
    assert!(mid_offset > 0 && mid_offset <= split as u64);

    std::fs::write(tmp.path(), &bytes).unwrap();
    p.parse_to_end().unwrap();

    let full = parser::parse_file(&path, false).unwrap().unwrap();
    let incr = p.session.unwrap();
    assert_eq!(incr.tokens_total.total_tokens, full.tokens_total.total_tokens);
    assert_eq!(incr.tokens_by_model, full.tokens_by_model);
    assert_eq!(incr.total_turns, full.total_turns);
}

#[test]
fn populates_tokens_history_from_token_count_events() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    // The fixture has 8 token_count events; the first has info: null, so 7 history points.
    assert_eq!(s.tokens_history.len(), 7);
    // Monotonic non-decreasing total_tokens.
    let totals: Vec<u64> = s.tokens_history.iter().map(|p| p.total_tokens).collect();
    assert!(totals.windows(2).all(|w| w[0] <= w[1]), "history not monotonic: {:?}", totals);
    // Last point matches tokens_total.
    assert_eq!(s.tokens_history.last().unwrap().total_tokens, s.tokens_total.total_tokens);
    // Timestamps are strictly increasing.
    let ts: Vec<_> = s.tokens_history.iter().map(|p| p.timestamp).collect();
    assert!(ts.windows(2).all(|w| w[0] < w[1]), "timestamps not strictly increasing");
}
