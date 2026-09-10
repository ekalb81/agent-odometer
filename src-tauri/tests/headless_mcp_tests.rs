//! Adapter equivalence using synthetic ledger facts and a fixed clock.

use chrono::{TimeZone, Utc};
use odometer_lib::config::Config;
use odometer_lib::headless::{execute, QueryKind, Request};
use odometer_lib::history_store::HistoryStore;
use odometer_lib::mcp_server::call_tool_with_config;
use odometer_lib::model::{TokenHistoryPoint, TokenTotals};
use odometer_lib::provider::{claude_code_provider_id, codex_provider_id};
use odometer_lib::query_control::QueryControl;
use odometer_lib::rates::RateCard;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn fixture() -> (tempfile::TempDir, HistoryStore, RateCard) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("history.sqlite3");
    let store = HistoryStore::open(&path).unwrap();
    let generation = store.begin_scan().unwrap();
    let source =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-session.jsonl");
    for (id, harness, model, amount) in [
        ("synthetic-codex", codex_provider_id(), "gpt-5.5", 4000),
        (
            "synthetic-claude",
            claude_code_provider_id(),
            "claude-sonnet-4-6",
            2000,
        ),
    ] {
        let mut session = odometer_lib::parser::parse_file(&source, false)
            .unwrap()
            .unwrap();
        let timestamp = Utc.with_ymd_and_hms(2026, 9, 1, 12, 30, 0).unwrap();
        let tokens = TokenTotals {
            input_tokens: amount,
            cached_input_tokens: 300,
            cache_creation_input_tokens: 100,
            output_tokens: 500,
            reasoning_output_tokens: 100,
            total_tokens: amount + 500,
        };
        session.id = id.into();
        session.storage_id.clear();
        session.harness = harness;
        session.model = Some(model.into());
        session.started_at = timestamp;
        session.last_event_at = timestamp;
        session.tokens_total = tokens.clone();
        session.tokens_by_model = HashMap::from([(model.into(), tokens.clone())]);
        session.tokens_history = vec![TokenHistoryPoint {
            timestamp,
            model: Some(model.into()),
            service_tier: Some("fast".into()),
            request_input_tokens: None,
            total_tokens: tokens.total_tokens,
            delta: tokens,
        }];
        session.turns.clear();
        store
            .observe(Path::new(&format!("{id}.jsonl")), &session, generation)
            .unwrap();
    }
    store.finish_scan(generation).unwrap();
    drop(store);
    let store = HistoryStore::open_read_only(&path, QueryControl::default()).unwrap();
    (directory, store, RateCard::load_bundled().unwrap())
}

#[test]
fn every_mcp_report_matches_shared_dispatch_for_identical_facts_and_clock() {
    let (_directory, store, rates) = fixture();
    let config = Config::default();
    let now = Utc.with_ymd_and_hms(2026, 9, 1, 13, 0, 0).unwrap();
    for (name, kind) in [
        ("usage_report", QueryKind::Report),
        ("model_report", QueryKind::Models),
        ("project_report", QueryKind::Projects),
        ("workflow_metrics", QueryKind::Metrics),
        ("session_report", QueryKind::Sessions),
        ("activity_report", QueryKind::Activity),
        ("category_report", QueryKind::Categories),
        ("tools_report", QueryKind::Tools),
        ("context_report", QueryKind::Context),
        ("findings_report", QueryKind::Findings),
        ("diagnostics_report", QueryKind::Diagnostics),
        ("quota_status", QueryKind::Quota),
        ("ledger_status", QueryKind::Status),
        ("statusline", QueryKind::Statusline),
    ] {
        let args = if kind.accepts_window() {
            json!({"from":"2026-09-01", "to":"2026-09-01"})
        } else {
            json!({})
        };
        let request = if kind.accepts_window() {
            Request {
                from: Some(odometer_lib::headless::parse_date("2026-09-01", false).unwrap()),
                to: Some(odometer_lib::headless::parse_date("2026-09-01", true).unwrap()),
                ..Request::default()
            }
        } else {
            Request::default()
        };
        let expected = execute(kind, &store, &rates, &config, &request, now).unwrap();
        let actual: Value = serde_json::from_str(
            &call_tool_with_config(
                &store,
                &rates,
                &config,
                &json!({"name":name, "arguments":args}),
                now,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(actual, expected, "{name}");
        if name != "quota_status" {
            assert_eq!(actual["schema_version"], 1, "{name}");
        }
    }
}

#[test]
fn new_envelopes_preserve_the_legacy_quota_array_and_mirror_facts() {
    let (_directory, store, rates) = fixture();
    let config = Config::default();
    let now = Utc.with_ymd_and_hms(2026, 9, 1, 13, 0, 0).unwrap();
    let call = |name: &str| -> Value {
        serde_json::from_str(
            &call_tool_with_config(&store, &rates, &config, &json!({"name":name}), now).unwrap(),
        )
        .unwrap()
    };
    let legacy = call("quota_status");
    let versioned = call("quota_report");
    assert!(legacy.is_array());
    assert_eq!(versioned["schema_version"], 1);
    assert_eq!(versioned["snapshots"], legacy);
    let mirrors = call("mirrored_sessions");
    assert_eq!(mirrors["schema_version"], 1);
    assert_eq!(
        mirrors["groups"],
        serde_json::to_value(store.mirrored_session_groups().unwrap()).unwrap()
    );
}

#[test]
fn sessions_are_bounded_and_cancellation_rejects_shared_dispatch() {
    let (directory, store, rates) = fixture();
    let config = Config::default();
    let now = Utc.with_ymd_and_hms(2026, 9, 1, 13, 0, 0).unwrap();
    let response: Value = serde_json::from_str(
        &call_tool_with_config(
            &store,
            &rates,
            &config,
            &json!({"name":"session_report", "arguments":{"limit":1}}),
            now,
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(response["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(response["truncated_to"], 1);
    let control = QueryControl::default();
    let cancelled =
        HistoryStore::open_read_only(&directory.path().join("history.sqlite3"), control.clone())
            .unwrap();
    control.cancel();
    assert!(execute(
        QueryKind::Report,
        &cancelled,
        &rates,
        &config,
        &Request::default(),
        now
    )
    .unwrap_err()
    .to_string()
    .contains("cancelled"));
}
