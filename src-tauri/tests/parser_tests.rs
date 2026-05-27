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

#[test]
fn resumed_session_attributes_carryover_total_to_active_model() {
    // Simulate a Codex rollout file that resumed from a prior session: the
    // first non-null token_count event's `total_token_usage` already includes
    // significant carry-over from the previous file, while `last_token_usage`
    // is just the latest API call's delta.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resumed.jsonl");

    let lines = [
        r#"{"timestamp":"2026-05-26T11:33:00.000Z","type":"session_meta","payload":{"id":"019e64eb-aaaa-bbbb-cccc-000000000001","timestamp":"2026-05-26T11:33:00.000Z","cwd":"C:\\Projects\\canopy","originator":"Codex Desktop","cli_version":"0.130.0","source":"vscode","model_provider":"openai"}}"#,
        r#"{"timestamp":"2026-05-26T11:33:01.000Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.5","effort":"high","approval_policy":"never","sandbox_policy":{"type":"danger-full-access"},"personality":"pragmatic","collaboration_mode":{"mode":"default"}}}"#,
        // First non-null token_count: total already at ~23M (carried over),
        // last is only ~100K (the most recent API call in this file).
        r#"{"timestamp":"2026-05-26T11:33:02.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":23000000,"cached_input_tokens":20000000,"output_tokens":80000,"reasoning_output_tokens":40000,"total_tokens":23080000},"last_token_usage":{"input_tokens":100000,"cached_input_tokens":80000,"output_tokens":1000,"reasoning_output_tokens":500,"total_tokens":101000},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":true,"balance":null}}}}"#,
        // Second token_count: total grew by `last`'s amount.
        r#"{"timestamp":"2026-05-26T11:33:03.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":23187732,"cached_input_tokens":20105344,"output_tokens":90479,"reasoning_output_tokens":46811,"total_tokens":23278211},"last_token_usage":{"input_tokens":187732,"cached_input_tokens":105344,"output_tokens":10479,"reasoning_output_tokens":6811,"total_tokens":198211},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":true,"balance":null}}}}"#,
    ];

    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let s = parser::parse_file(&path, false).unwrap().unwrap();

    // tokens_total = latest total_token_usage (latest-wins).
    assert_eq!(s.tokens_total.total_tokens, 23_278_211);

    // tokens_by_model must include the carry-over so the per-model bucket
    // matches the session total (single-model session).
    let per = s.tokens_by_model.get("gpt-5.5").expect("gpt-5.5 bucket present");
    assert_eq!(per.input_tokens,            23_187_732);
    assert_eq!(per.cached_input_tokens,     20_105_344);
    assert_eq!(per.output_tokens,                90_479);
    assert_eq!(per.reasoning_output_tokens,      46_811);
    assert_eq!(per.total_tokens,            23_278_211);
}

#[test]
fn mid_session_model_switch_attributes_each_phase_correctly() {
    // Simulate a session that starts on gpt-5.4 for the bulk of the work,
    // then switches to gpt-5.5 for the final turns. Per-model buckets must
    // sum to tokens_total, with the gpt-5.4 phase getting the heavy share.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("switch.jsonl");

    let lines = [
        r#"{"timestamp":"2026-05-26T15:33:42.000Z","type":"session_meta","payload":{"id":"019e64eb-aaaa-bbbb-cccc-000000000002","timestamp":"2026-05-26T15:33:42.000Z","cwd":"E:\\Projects","originator":"Codex Desktop","cli_version":"0.133.0","source":"vscode","model_provider":"openai"}}"#,
        r#"{"timestamp":"2026-05-26T15:33:45.000Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.4"}}"#,
        // First token_count under gpt-5.4: fresh, total == last.
        r#"{"timestamp":"2026-05-26T15:33:50.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100000,"cached_input_tokens":80000,"output_tokens":500,"reasoning_output_tokens":300,"total_tokens":100500},"last_token_usage":{"input_tokens":100000,"cached_input_tokens":80000,"output_tokens":500,"reasoning_output_tokens":300,"total_tokens":100500},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":true,"balance":null}}}}"#,
        // Second token_count under gpt-5.4: total grew, delta in last.
        r#"{"timestamp":"2026-05-26T15:34:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900000,"cached_input_tokens":800000,"output_tokens":3000,"reasoning_output_tokens":1000,"total_tokens":903000},"last_token_usage":{"input_tokens":800000,"cached_input_tokens":720000,"output_tokens":2500,"reasoning_output_tokens":700,"total_tokens":802500},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":true,"balance":null}}}}"#,
        // User switches model.
        r#"{"timestamp":"2026-05-26T15:34:30.000Z","type":"turn_context","payload":{"turn_id":"t2","model":"gpt-5.5"}}"#,
        // First token_count under gpt-5.5: total grew by 50k input, last==delta.
        r#"{"timestamp":"2026-05-26T15:34:35.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":950000,"cached_input_tokens":830000,"output_tokens":3500,"reasoning_output_tokens":1200,"total_tokens":953500},"last_token_usage":{"input_tokens":50000,"cached_input_tokens":30000,"output_tokens":500,"reasoning_output_tokens":200,"total_tokens":50500},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":true,"balance":null}}}}"#,
    ];

    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let s = parser::parse_file(&path, false).unwrap().unwrap();

    assert_eq!(s.tokens_total.total_tokens, 953_500);

    // gpt-5.4 phase: 100,500 + 802,500 = 903,000 tokens total
    let p54 = s.tokens_by_model.get("gpt-5.4").expect("gpt-5.4 bucket present");
    assert_eq!(p54.input_tokens,            900_000);
    assert_eq!(p54.cached_input_tokens,     800_000);
    assert_eq!(p54.output_tokens,             3_000);
    assert_eq!(p54.reasoning_output_tokens,   1_000);
    assert_eq!(p54.total_tokens,            903_000);

    // gpt-5.5 phase: 50,500 tokens (the post-switch delta)
    let p55 = s.tokens_by_model.get("gpt-5.5").expect("gpt-5.5 bucket present");
    assert_eq!(p55.input_tokens,             50_000);
    assert_eq!(p55.cached_input_tokens,      30_000);
    assert_eq!(p55.output_tokens,               500);
    assert_eq!(p55.reasoning_output_tokens,     200);
    assert_eq!(p55.total_tokens,             50_500);

    // Invariant: sum of buckets equals session total.
    let sum_input = p54.input_tokens + p55.input_tokens;
    assert_eq!(sum_input, s.tokens_total.input_tokens);
}

#[test]
fn duplicate_session_meta_does_not_wipe_accumulated_buckets() {
    // Codex Desktop emits session_meta a second time when a session is
    // resumed / re-opened in the same rollout file. The parser must not
    // treat that as a brand-new session and wipe tokens_by_model.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("doublemeta.jsonl");

    let lines = [
        // First session_meta + work on gpt-5.4
        r#"{"timestamp":"2026-05-26T15:33:43.000Z","type":"session_meta","payload":{"id":"019e64eb-aaaa-bbbb-cccc-000000000003","timestamp":"2026-05-26T15:33:43.000Z","cwd":"E:\\Projects","originator":"Codex Desktop","cli_version":"0.133.0","source":"vscode","model_provider":"openai"}}"#,
        r#"{"timestamp":"2026-05-26T15:33:45.000Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.4"}}"#,
        r#"{"timestamp":"2026-05-26T16:22:49.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":15263176,"cached_input_tokens":12731392,"output_tokens":63434,"reasoning_output_tokens":34004,"total_tokens":15326610},"last_token_usage":{"input_tokens":15263176,"cached_input_tokens":12731392,"output_tokens":63434,"reasoning_output_tokens":34004,"total_tokens":15326610},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":false,"balance":null}}}}"#,
        // Second session_meta (the resume marker) — must NOT wipe state
        r#"{"timestamp":"2026-05-26T16:25:00.000Z","type":"session_meta","payload":{"id":"019e64eb-aaaa-bbbb-cccc-000000000003","timestamp":"2026-05-26T16:25:00.000Z","cwd":"E:\\Projects","originator":"Codex Desktop","cli_version":"0.133.0","source":"vscode","model_provider":"openai"}}"#,
        // Switch to gpt-5.5 and do more work
        r#"{"timestamp":"2026-05-26T16:25:14.000Z","type":"turn_context","payload":{"turn_id":"t2","model":"gpt-5.5"}}"#,
        r#"{"timestamp":"2026-05-26T17:15:37.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":23187732,"cached_input_tokens":20105344,"output_tokens":90479,"reasoning_output_tokens":46811,"total_tokens":23278211},"last_token_usage":{"input_tokens":7924556,"cached_input_tokens":7373952,"output_tokens":27045,"reasoning_output_tokens":12807,"total_tokens":7951601},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":false,"balance":null}}}}"#,
    ];

    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let s = parser::parse_file(&path, false).unwrap().unwrap();

    // Identity is preserved from the first session_meta.
    assert_eq!(s.id, "019e64eb-aaaa-bbbb-cccc-000000000003");
    assert_eq!(s.tokens_total.total_tokens, 23_278_211);

    // Both buckets must be present — the second session_meta did NOT wipe gpt-5.4.
    let p54 = s.tokens_by_model.get("gpt-5.4").expect("gpt-5.4 bucket present");
    assert_eq!(p54.input_tokens,            15_263_176);
    assert_eq!(p54.cached_input_tokens,     12_731_392);
    assert_eq!(p54.total_tokens,            15_326_610);

    let p55 = s.tokens_by_model.get("gpt-5.5").expect("gpt-5.5 bucket present");
    assert_eq!(p55.input_tokens,             7_924_556);
    assert_eq!(p55.cached_input_tokens,      7_373_952);
    assert_eq!(p55.total_tokens,             7_951_601);

    // Sum-of-buckets invariant.
    assert_eq!(
        p54.input_tokens + p55.input_tokens,
        s.tokens_total.input_tokens
    );
}

#[test]
fn tokens_history_captures_model_and_delta_per_event() {
    // Each history point should carry the active model at the time of the
    // event and the per-call delta, so the frontend can roll up credit
    // spend by date with the correct rate.
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    assert_eq!(s.tokens_history.len(), 7);

    // The fixture's session is entirely under gpt-5.5.
    assert!(s.tokens_history.iter().all(|p| p.model.as_deref() == Some("gpt-5.5")));

    // Per-event deltas should sum to the cumulative total_token_usage.
    let summed: u64 = s.tokens_history.iter().map(|p| p.delta.total_tokens).sum();
    assert_eq!(summed, s.tokens_total.total_tokens);

    // First event: total == last == 29196 in this fixture.
    assert_eq!(s.tokens_history[0].delta.total_tokens, 29_196);
}
