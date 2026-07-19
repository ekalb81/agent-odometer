use odometer_lib::model::TurnStatus;
use odometer_lib::parser;
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
    assert!(s
        .working_directory
        .as_deref()
        .unwrap()
        .contains("summarize-my-codex-usage-for-may"));
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
    assert!(!m.ends_with('\n')); // trailing whitespace trimmed
    assert!(m.chars().count() <= 200);
}

#[test]
fn parses_tokens_total_from_latest_token_count() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    let t = &s.tokens_total;
    assert_eq!(t.input_tokens, 547_081);
    assert_eq!(t.cached_input_tokens, 429_696);
    assert_eq!(t.output_tokens, 5_812);
    assert_eq!(t.reasoning_output_tokens, 2_018);
    assert_eq!(t.total_tokens, 552_893);
}

#[test]
fn attributes_tokens_to_active_model() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    // All work in this session ran under gpt-5.5
    assert!(s.tokens_by_model.contains_key("gpt-5.5"));
    let per_model = &s.tokens_by_model["gpt-5.5"];
    // Sum of last_token_usage deltas should match total_token_usage exactly.
    assert_eq!(per_model.input_tokens, 547_081);
    assert_eq!(per_model.cached_input_tokens, 429_696);
    assert_eq!(per_model.output_tokens, 5_812);
    assert_eq!(per_model.reasoning_output_tokens, 2_018);
    assert_eq!(per_model.total_tokens, 552_893);
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
    let split = bytes[half..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|p| half + p + 1)
        .unwrap();

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
    assert_eq!(
        incr.tokens_total.total_tokens,
        full.tokens_total.total_tokens
    );
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
    assert!(
        totals.windows(2).all(|w| w[0] <= w[1]),
        "history not monotonic: {:?}",
        totals
    );
    // Last point matches tokens_total.
    assert_eq!(
        s.tokens_history.last().unwrap().total_tokens,
        s.tokens_total.total_tokens
    );
    // Timestamps are strictly increasing.
    let ts: Vec<_> = s.tokens_history.iter().map(|p| p.timestamp).collect();
    assert!(
        ts.windows(2).all(|w| w[0] < w[1]),
        "timestamps not strictly increasing"
    );
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
    let per = s
        .tokens_by_model
        .get("gpt-5.5")
        .expect("gpt-5.5 bucket present");
    assert_eq!(per.input_tokens, 23_187_732);
    assert_eq!(per.cached_input_tokens, 20_105_344);
    assert_eq!(per.output_tokens, 90_479);
    assert_eq!(per.reasoning_output_tokens, 46_811);
    assert_eq!(per.total_tokens, 23_278_211);
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
    let p54 = s
        .tokens_by_model
        .get("gpt-5.4")
        .expect("gpt-5.4 bucket present");
    assert_eq!(p54.input_tokens, 900_000);
    assert_eq!(p54.cached_input_tokens, 800_000);
    assert_eq!(p54.output_tokens, 3_000);
    assert_eq!(p54.reasoning_output_tokens, 1_000);
    assert_eq!(p54.total_tokens, 903_000);

    // gpt-5.5 phase: 50,500 tokens (the post-switch delta)
    let p55 = s
        .tokens_by_model
        .get("gpt-5.5")
        .expect("gpt-5.5 bucket present");
    assert_eq!(p55.input_tokens, 50_000);
    assert_eq!(p55.cached_input_tokens, 30_000);
    assert_eq!(p55.output_tokens, 500);
    assert_eq!(p55.reasoning_output_tokens, 200);
    assert_eq!(p55.total_tokens, 50_500);

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
    let p54 = s
        .tokens_by_model
        .get("gpt-5.4")
        .expect("gpt-5.4 bucket present");
    assert_eq!(p54.input_tokens, 15_263_176);
    assert_eq!(p54.cached_input_tokens, 12_731_392);
    assert_eq!(p54.total_tokens, 15_326_610);

    let p55 = s
        .tokens_by_model
        .get("gpt-5.5")
        .expect("gpt-5.5 bucket present");
    assert_eq!(p55.input_tokens, 7_924_556);
    assert_eq!(p55.cached_input_tokens, 7_373_952);
    assert_eq!(p55.total_tokens, 7_951_601);

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
    assert!(s
        .tokens_history
        .iter()
        .all(|p| p.model.as_deref() == Some("gpt-5.5")));

    // Per-event deltas should sum to the cumulative total_token_usage.
    let summed: u64 = s.tokens_history.iter().map(|p| p.delta.total_tokens).sum();
    assert_eq!(summed, s.tokens_total.total_tokens);

    // First event: total == last == 29196 in this fixture.
    assert_eq!(s.tokens_history[0].delta.total_tokens, 29_196);
}

#[test]
fn builds_turn_details_from_fixture() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();

    // The fixture is a single-turn session.
    assert_eq!(s.turns.len(), 1);
    let t = &s.turns[0];

    assert_eq!(t.index, 1);
    assert_eq!(t.turn_id, "019e2ba6-9637-7682-9b6d-7838b0c2e0e5");
    assert_eq!(t.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(t.status, TurnStatus::Completed);

    // Prompt + final agent message captured.
    assert!(t
        .user_message
        .as_deref()
        .unwrap()
        .starts_with("Summarize my codex usage"));
    assert!(t.last_agent_message.as_deref().unwrap().contains("May 7"));

    // Lifecycle from task_complete.
    assert_eq!(t.duration_ms, Some(106_663));
    assert_eq!(t.time_to_first_token_ms, Some(21_311));
    assert!(t.started_at.is_some());
    assert!(t.completed_at.is_some());

    // All session tokens belong to this single turn.
    assert_eq!(t.tokens.total_tokens, s.tokens_total.total_tokens);
    assert_eq!(t.tokens.total_tokens, 552_893);
}

#[test]
fn model_switch_produces_two_turns_with_scoped_tokens() {
    // Two turns: gpt-5.4 then gpt-5.5. Each turn's tokens should reflect only
    // its own deltas, and the per-turn totals should sum to tokens_total.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("turns.jsonl");

    let lines = [
        r#"{"timestamp":"2026-05-26T15:33:42.000Z","type":"session_meta","payload":{"id":"019eaaaa-bbbb-cccc-dddd-000000000010","timestamp":"2026-05-26T15:33:42.000Z","cwd":"E:\\Projects","originator":"Codex Desktop","cli_version":"0.133.0","source":"vscode","model_provider":"openai"}}"#,
        r#"{"timestamp":"2026-05-26T15:33:45.000Z","type":"turn_context","payload":{"turn_id":"turn-A","model":"gpt-5.4"}}"#,
        r#"{"timestamp":"2026-05-26T15:33:46.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-A","model_context_window":258400}}"#,
        r#"{"timestamp":"2026-05-26T15:33:47.000Z","type":"event_msg","payload":{"type":"user_message","message":"Do the first thing"}}"#,
        r#"{"timestamp":"2026-05-26T15:34:00.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":900000,"cached_input_tokens":800000,"output_tokens":3000,"reasoning_output_tokens":1000,"total_tokens":903000},"last_token_usage":{"input_tokens":900000,"cached_input_tokens":800000,"output_tokens":3000,"reasoning_output_tokens":1000,"total_tokens":903000},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":false,"balance":null}}}}"#,
        r#"{"timestamp":"2026-05-26T15:34:05.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-A","duration_ms":19000,"time_to_first_token_ms":2000,"last_agent_message":"Done with first."}}"#,
        r#"{"timestamp":"2026-05-26T15:34:30.000Z","type":"turn_context","payload":{"turn_id":"turn-B","model":"gpt-5.5"}}"#,
        r#"{"timestamp":"2026-05-26T15:34:31.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-B","model_context_window":258400}}"#,
        r#"{"timestamp":"2026-05-26T15:34:32.000Z","type":"event_msg","payload":{"type":"user_message","message":"Do the second thing"}}"#,
        r#"{"timestamp":"2026-05-26T15:34:40.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":950000,"cached_input_tokens":830000,"output_tokens":3500,"reasoning_output_tokens":1200,"total_tokens":953500},"last_token_usage":{"input_tokens":50000,"cached_input_tokens":30000,"output_tokens":500,"reasoning_output_tokens":200,"total_tokens":50500},"model_context_window":258400},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":false,"balance":null}}}}"#,
        r#"{"timestamp":"2026-05-26T15:34:45.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"turn-B","duration_ms":13000,"time_to_first_token_ms":1500,"last_agent_message":"Done with second."}}"#,
    ];

    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let s = parser::parse_file(&path, false).unwrap().unwrap();

    assert_eq!(s.turns.len(), 2);

    let a = &s.turns[0];
    assert_eq!(a.index, 1);
    assert_eq!(a.turn_id, "turn-A");
    assert_eq!(a.model.as_deref(), Some("gpt-5.4"));
    assert_eq!(a.status, TurnStatus::Completed);
    assert_eq!(a.user_message.as_deref(), Some("Do the first thing"));
    assert_eq!(a.last_agent_message.as_deref(), Some("Done with first."));
    assert_eq!(a.tokens.total_tokens, 903_000);

    let b = &s.turns[1];
    assert_eq!(b.index, 2);
    assert_eq!(b.turn_id, "turn-B");
    assert_eq!(b.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(b.status, TurnStatus::Completed);
    assert_eq!(b.tokens.total_tokens, 50_500);

    // Per-turn totals sum to the session total.
    assert_eq!(
        a.tokens.total_tokens + b.tokens.total_tokens,
        s.tokens_total.total_tokens
    );
}

#[test]
fn trailing_response_item_advances_last_event_at() {
    // response_item records are skipped without a full parse, but they must
    // still advance last_event_at — a session whose final records are all
    // response_item lines would otherwise report a stale last-activity time.
    let mut p = parser::SessionParser::new(PathBuf::from("unused.jsonl"), false);
    p.apply_line(
        r#"{"timestamp":"2026-05-26T11:33:00.000Z","type":"session_meta","payload":{"id":"019e64eb-aaaa-bbbb-cccc-000000000020","timestamp":"2026-05-26T11:33:00.000Z","cwd":"E:\\Projects","originator":"Codex Desktop","cli_version":"0.133.0","source":"vscode","model_provider":"openai"}}"#,
    )
    .unwrap();
    let before = p.session.as_ref().unwrap().last_event_at;

    p.apply_line(
        r#"{"timestamp":"2026-05-26T11:40:00.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"hello"}]}}"#,
    )
    .unwrap();

    let after = p.session.as_ref().unwrap().last_event_at;
    assert!(after > before, "response_item must advance last_event_at");
    assert_eq!(after.to_rfc3339(), "2026-05-26T11:40:00+00:00");

    // compacted records take the same fast-skip path and must behave the same.
    p.apply_line(
        r#"{"timestamp":"2026-05-26T11:45:00.000Z","type":"compacted","payload":{"message":"summary of earlier turns"}}"#,
    )
    .unwrap();
    let final_ts = p.session.as_ref().unwrap().last_event_at;
    assert_eq!(final_ts.to_rfc3339(), "2026-05-26T11:45:00+00:00");
}

#[test]
fn fast_skip_does_not_misfire_on_payload_containing_response_item_text() {
    // This event_msg's payload CONTAINS the literal text "type":"response_item"
    // (and even a leading {"timestamp":"..." inside the message string), but its
    // real record type is event_msg — it must still be fully processed.
    let mut p = parser::SessionParser::new(PathBuf::from("unused.jsonl"), false);
    p.apply_line(
        r#"{"timestamp":"2026-05-26T11:33:00.000Z","type":"session_meta","payload":{"id":"019e64eb-aaaa-bbbb-cccc-000000000021","timestamp":"2026-05-26T11:33:00.000Z","cwd":"E:\\Projects","originator":"Codex Desktop","cli_version":"0.133.0","source":"vscode","model_provider":"openai"}}"#,
    )
    .unwrap();
    p.apply_line(
        r#"{"timestamp":"2026-05-26T11:33:05.000Z","type":"event_msg","payload":{"type":"user_message","message":"look at this line: {\"timestamp\":\"2026-01-01T00:00:00Z\",\"type\":\"response_item\"} please"}}"#,
    )
    .unwrap();

    let s = p.session.as_ref().unwrap();
    assert_eq!(
        s.first_user_message.as_deref(),
        Some(
            r#"look at this line: {"timestamp":"2026-01-01T00:00:00Z","type":"response_item"} please"#
        ),
        "event_msg containing response_item text must be fully processed"
    );
}

#[test]
fn response_item_with_reordered_keys_still_advances_last_event_at() {
    // A response_item line that doesn't match the fast-path shape (type before
    // timestamp) must fall through to the full parse and behave identically.
    let mut p = parser::SessionParser::new(PathBuf::from("unused.jsonl"), false);
    p.apply_line(
        r#"{"timestamp":"2026-05-26T11:33:00.000Z","type":"session_meta","payload":{"id":"019e64eb-aaaa-bbbb-cccc-000000000022","timestamp":"2026-05-26T11:33:00.000Z","cwd":"E:\\Projects","originator":"Codex Desktop","cli_version":"0.133.0","source":"vscode","model_provider":"openai"}}"#,
    )
    .unwrap();
    p.apply_line(
        r#"{"type":"response_item","timestamp":"2026-05-26T11:50:00.000Z","payload":{"type":"message"}}"#,
    )
    .unwrap();
    assert_eq!(
        p.session.as_ref().unwrap().last_event_at.to_rfc3339(),
        "2026-05-26T11:50:00+00:00"
    );
}

#[test]
fn parses_current_chatgpt_subagent_and_rollback_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("current.jsonl");
    let lines = [
        r#"{"timestamp":"2026-07-13T12:00:00.000Z","type":"session_meta","payload":{"id":"019f-current-subagent","timestamp":"2026-07-13T12:00:00.000Z","cwd":"E:\\Projects","originator":"Codex Desktop","cli_version":"0.144.2","source":{"subagent":{"thread_spawn":{"depth":1}}},"model_provider":"openai","forked_from_id":"fork-source","parent_thread_id":"parent-task","agent_path":"/root/reviewer","agent_nickname":"Reviewer","history_mode":"save-all","memory_mode":"enabled"}}"#,
        r#"{"timestamp":"2026-07-13T12:00:00.500Z","type":"event_msg","payload":{"type":"thread_settings_applied","thread_settings":{"model":"gpt-5.6-sol","reasoning_effort":"high","collaboration_mode":{"mode":"default"},"service_tier":"standard"}}}"#,
        r#"{"timestamp":"2026-07-13T12:00:01.000Z","type":"turn_context","payload":{"turn_id":"turn-current","model":"gpt-5.6-sol","effort":"high","collaboration_mode":{"mode":"default"}}}"#,
        r#"{"timestamp":"2026-07-13T12:00:02.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"turn-current","started_at":"2026-07-13T12:00:01.500Z","model_context_window":272000,"collaboration_mode_kind":"default"}}"#,
        r#"{"timestamp":"2026-07-13T12:00:03.000Z","type":"event_msg","payload":{"type":"user_message","message":"Review the current behavior"}}"#,
        r#"{"timestamp":"2026-07-13T12:00:03.250Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10,"reasoning_output_tokens":5,"total_tokens":110},"last_token_usage":{"input_tokens":100,"cached_input_tokens":50,"output_tokens":10,"reasoning_output_tokens":5,"total_tokens":110},"model_context_window":272000},"rate_limits":{"plan_type":"business","credits":{"has_credits":true,"unlimited":false,"balance":1000}}}}"#,
        r#"{"timestamp":"2026-07-13T12:00:04.000Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"turn-current","completed_at":"2026-07-13T12:00:03.500Z","duration_ms":2000,"reason":"user_interrupt"}}"#,
        r#"{"timestamp":"2026-07-13T12:00:05.000Z","type":"event_msg","payload":{"type":"thread_rolled_back","num_turns":1}}"#,
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();

    let session = parser::parse_file(&path, false).unwrap().unwrap();
    assert_eq!(session.source.as_deref(), Some("subagent"));
    assert_eq!(session.forked_from_id.as_deref(), Some("fork-source"));
    assert_eq!(session.parent_thread_id.as_deref(), Some("parent-task"));
    assert_eq!(session.agent_path.as_deref(), Some("/root/reviewer"));
    assert_eq!(session.agent_nickname.as_deref(), Some("Reviewer"));
    assert_eq!(session.history_mode.as_deref(), Some("save-all"));
    assert_eq!(session.memory_mode.as_deref(), Some("enabled"));
    assert_eq!(session.context_window, Some(272_000));
    assert_eq!(session.service_tier.as_deref(), Some("standard"));
    assert_eq!(
        session.tokens_history[0].service_tier.as_deref(),
        Some("standard")
    );

    let turn = &session.turns[0];
    assert_eq!(turn.model.as_deref(), Some("gpt-5.6-sol"));
    assert_eq!(turn.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(turn.collaboration_mode.as_deref(), Some("default"));
    assert_eq!(turn.service_tier.as_deref(), Some("standard"));
    assert_eq!(turn.status, TurnStatus::RolledBack);
    assert_eq!(turn.abort_reason.as_deref(), Some("user_interrupt"));
    assert_eq!(turn.duration_ms, Some(2_000));
    assert_eq!(
        turn.started_at.unwrap().to_rfc3339(),
        "2026-07-13T12:00:01.500+00:00"
    );
    assert_eq!(
        turn.completed_at.unwrap().to_rfc3339(),
        "2026-07-13T12:00:03.500+00:00"
    );
}

#[test]
fn latest_context_tokens_tracks_last_call_not_cumulative() {
    let s = parser::parse_file(&fixture(), false).unwrap().unwrap();
    // Final token_count's raw last_token_usage: input 119,667 + output 683.
    assert_eq!(s.latest_context_tokens, Some(120_350));
    // Sanity: cumulative total is far larger — the old context math was wrong.
    assert!(s.tokens_total.total_tokens > 120_350);
}

#[test]
fn new_turn_marks_stale_in_progress_turn_aborted() {
    use odometer_lib::parser::SessionParser;
    let mut p = SessionParser::new(std::path::PathBuf::from("synthetic.jsonl"), false);
    let apply = |p: &mut SessionParser, line: &str| p.apply_line(line).unwrap();

    apply(
        &mut p,
        r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"s1","timestamp":"2026-01-01T00:00:00Z"}}"#,
    );
    apply(
        &mut p,
        r#"{"timestamp":"2026-01-01T00:00:01Z","type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5.5"}}"#,
    );
    apply(
        &mut p,
        r#"{"timestamp":"2026-01-01T00:00:02Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
    );
    // No task_complete/turn_aborted for t1 — the next turn supersedes it.
    apply(
        &mut p,
        r#"{"timestamp":"2026-01-01T00:01:00Z","type":"turn_context","payload":{"turn_id":"t2","model":"gpt-5.5"}}"#,
    );

    let s = p.session.as_ref().unwrap();
    assert_eq!(s.turns.len(), 2);
    assert_eq!(s.turns[0].status, TurnStatus::Aborted);
    assert_eq!(s.turns[1].status, TurnStatus::InProgress);

    // A late task_complete for the superseded turn still wins.
    apply(
        &mut p,
        r#"{"timestamp":"2026-01-01T00:01:05Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1"}}"#,
    );
    let s = p.session.as_ref().unwrap();
    assert_eq!(s.turns[0].status, TurnStatus::Completed);
}
