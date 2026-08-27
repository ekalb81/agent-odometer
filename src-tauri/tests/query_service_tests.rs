//! Integration coverage for the UI-independent query service (issue #47).
//!
//! These run against a real `HistoryStore` rather than a mock, because the
//! properties that matter are exactly the ones a mock would assume: that a
//! report reads the durable rollups, that its totals reconcile with the
//! ledger's own per-session answers, and that pricing goes through the rate
//! card's resolver rather than a second implementation.

use chrono::{TimeZone, Utc};
use odometer_lib::history_store::HistoryStore;
use odometer_lib::model::{Session, TierBucket, TokenHistoryPoint, TokenTotals};
use odometer_lib::parser;
use odometer_lib::provider::codex_provider_id;
use odometer_lib::query::{price_tokens, range_report, PricedAmount, RateTable};
use odometer_lib::rates::{ModelRate, PricingBasis, RateCard};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn rate(input: f64) -> ModelRate {
    ModelRate {
        input,
        cached_input: input / 10.0,
        output: input * 5.0,
        reasoning: input * 5.0,
        cache_creation_input: None,
    }
}

fn card() -> RateCard {
    let mut card = RateCard {
        version: 1,
        currency: "USD".into(),
        unit: "per_1m_tokens".into(),
        source_url: "https://example.test".into(),
        fetched_at: None,
        fallback_model: "fallback-model".into(),
        ..Default::default()
    };
    card.models.insert("real-model".into(), rate(1.0));
    card.models.insert("fallback-model".into(), rate(99.0));
    card
}

fn tokens(input: u64, output: u64) -> TokenTotals {
    TokenTotals {
        input_tokens: input,
        output_tokens: output,
        total_tokens: input + output,
        ..Default::default()
    }
}

/// A real parsed session, re-stamped for the test.
///
/// Built from the committed transcript fixture rather than a hand-written
/// struct: the ledger derives its rollups from `tokens_history`, so a
/// synthetic session with the wrong shape would produce a report that
/// passes while the real one does not.
fn session_at(id: &str, model: &str, when_ms: i64, input: u64, output: u64) -> Session {
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-session.jsonl");
    let mut session = parser::parse_file(&fixture, false)
        .expect("fixture parses")
        .expect("fixture yields a session");
    let timestamp = Utc.timestamp_millis_opt(when_ms).single().expect("instant");

    session.id = id.to_string();
    session.storage_id = String::new();
    session.harness = codex_provider_id();
    session.model = Some(model.to_string());
    session.started_at = timestamp;
    session.last_event_at = timestamp;

    // One controlled usage event is what the rollups are built from, so the
    // window and per-model assertions have an exact expected value.
    let usage = tokens(input, output);
    session.tokens_history = vec![TokenHistoryPoint {
        timestamp,
        model: Some(model.to_string()),
        service_tier: None,
        request_input_tokens: None,
        total_tokens: usage.total_tokens,
        delta: usage.clone(),
    }];
    session.tokens_total = usage.clone();
    session.tokens_by_model = HashMap::from([(model.to_string(), usage)]);
    session.turns.clear();
    session
}

/// Stores `sessions` in a fresh ledger and returns it.
fn ledger(directory: &Path, sessions: &[Session]) -> HistoryStore {
    let store = HistoryStore::open(&directory.join("history.sqlite3")).expect("open ledger");
    let generation = store.begin_scan().expect("scan").max(1);
    for (index, session) in sessions.iter().enumerate() {
        store
            .observe(
                Path::new(&format!("session-{index}.jsonl")),
                session,
                generation,
            )
            .expect("observe");
    }
    store
}

#[test]
fn a_range_report_counts_only_sessions_inside_the_window() {
    let directory = tempfile::tempdir().unwrap();
    let august = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let july = Utc.with_ymd_and_hms(2026, 7, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[
            session_at(
                "in-window",
                "real-model",
                august.timestamp_millis(),
                1_000,
                500,
            ),
            session_at(
                "out-of-window",
                "real-model",
                july.timestamp_millis(),
                9_000,
                9_000,
            ),
        ],
    );

    let report = range_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        Some(Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap()),
        Some(Utc.with_ymd_and_hms(2026, 8, 31, 23, 59, 59).unwrap()),
        Utc::now(),
    )
    .expect("report");

    assert_eq!(report.sessions, 1, "only the August session has usage here");
    assert_eq!(report.tokens.total_tokens, 1_500);
    // The July session is still in the ledger and is reported as having no
    // usage in this window, rather than vanishing from the accounting.
    assert_eq!(report.sessions_without_usage, 1);
}

#[test]
fn range_report_totals_reconcile_with_the_ledgers_own_per_session_answer() {
    // The reconciliation criterion from #47: an aggregate produced by the
    // query service must equal what the ledger reports session by session,
    // or the CLI and the desktop would disagree about the same corpus.
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let sessions: Vec<Session> = (0..5)
        .map(|index| {
            session_at(
                &format!("session-{index}"),
                "real-model",
                when.timestamp_millis() + index as i64 * 3_600_000,
                1_000 * (index as u64 + 1),
                100,
            )
        })
        .collect();
    let store = ledger(directory.path(), &sessions);

    let report = range_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    let keys = store.session_keys().expect("keys");
    let per_session = store
        .range_totals_multi(&keys, &[(None, None)])
        .expect("totals")
        .into_iter()
        .next()
        .unwrap_or_default();
    let expected: u64 = per_session
        .values()
        .map(|range| range.tokens.total_tokens)
        .sum();

    assert_eq!(report.tokens.total_tokens, expected);
    assert_eq!(report.sessions, 5);
}

#[test]
fn an_empty_ledger_reports_zero_rather_than_failing() {
    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let report = range_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("an empty ledger is a valid state, not an error");

    assert_eq!(report.sessions, 0);
    assert_eq!(report.tokens.total_tokens, 0);
    assert!(
        report.cost_by_currency.is_empty(),
        "no usage means no cost, not a zero cost"
    );
}

#[test]
fn pricing_goes_through_the_cards_alias_resolver() {
    // The reason this module exists. `turn_receipts.rs` previously priced by
    // direct lookup with a fallback, so an aliased model silently resolved
    // to the *fallback* rate — 99x the real one in this fixture — while the
    // desktop priced it correctly through the alias.
    let mut rates = card();
    rates
        .model_aliases
        .insert("aliased-model".into(), "real-model".into());

    let priced = price_tokens(
        &rates,
        codex_provider_id().as_str(),
        "aliased-model",
        None,
        &tokens(1_000_000, 0),
        RateTable::Plan,
        Utc::now(),
    );

    assert_eq!(
        priced,
        PricedAmount {
            amount: Some(1.0),
            basis: PricingBasis::Aliased,
            resolved_model: "real-model".into(),
        },
        "an aliased model must price at its target's rate, not the fallback"
    );
}

#[test]
fn a_known_unpriced_model_is_unavailable_rather_than_free() {
    let mut rates = card();
    rates.unpriced_models.push("preview-model".into());

    let priced = price_tokens(
        &rates,
        codex_provider_id().as_str(),
        "preview-model",
        None,
        &tokens(1_000_000, 0),
        RateTable::Plan,
        Utc::now(),
    );

    assert_eq!(priced.amount, None, "unpriced is not zero");
    assert_eq!(priced.basis, PricingBasis::Unavailable);
}

#[test]
fn a_free_local_model_prices_at_zero_rather_than_unavailable() {
    // The mirror image of the case above, and the distinction the desktop
    // already draws: declared-free is a known zero, not missing data.
    let mut rates = card();
    rates.free_local_models.push("local-model".into());

    let priced = price_tokens(
        &rates,
        codex_provider_id().as_str(),
        "local-model",
        None,
        &tokens(1_000_000, 0),
        RateTable::Plan,
        Utc::now(),
    );

    assert_eq!(priced.amount, Some(0.0));
    assert_eq!(priced.basis, PricingBasis::FreeLocal);
}

#[test]
fn an_api_table_query_for_a_non_codex_provider_is_unavailable() {
    let mut rates = card();
    rates.api_models.insert("real-model".into(), rate(2.0));

    let priced = price_tokens(
        &rates,
        "claude_code",
        "real-model",
        None,
        &tokens(1_000_000, 0),
        RateTable::Api,
        Utc::now(),
    );

    assert_eq!(
        priced.amount, None,
        "only Codex has an API rate table; anywhere else the answer is unknown, not zero"
    );
}

#[test]
fn a_report_names_the_models_it_could_not_price() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let mut rates = card();
    rates.unpriced_models.push("mystery-model".into());
    let store = ledger(
        directory.path(),
        &[
            session_at(
                "priced",
                "real-model",
                when.timestamp_millis(),
                1_000_000,
                0,
            ),
            session_at(
                "unpriced",
                "mystery-model",
                when.timestamp_millis(),
                1_000_000,
                0,
            ),
        ],
    );

    let report = range_report(
        &store,
        &rates,
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    assert_eq!(
        report.unpriced_models,
        vec!["mystery-model".to_string()],
        "a total that excludes a model must say which, or it reads as complete"
    );
    assert_eq!(
        report
            .cost_by_currency
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![1.0],
        "the priceable half still totals"
    );
}

#[test]
fn buckets_reconcile_when_the_same_model_appears_in_several_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[
            session_at("a", "real-model", when.timestamp_millis(), 500_000, 0),
            session_at(
                "b",
                "real-model",
                when.timestamp_millis() + 3_600_000,
                500_000,
                0,
            ),
        ],
    );

    let report = range_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    assert_eq!(report.by_model.len(), 1, "one model, one row");
    assert_eq!(report.by_model[0].tokens.input_tokens, 1_000_000);
    assert_eq!(
        report
            .cost_by_currency
            .values()
            .copied()
            .collect::<Vec<_>>(),
        vec![1.0]
    );
}

/// Guards the DRY boundary itself: a `TierBucket` priced through the shared
/// service equals the same usage priced as a whole-model total, so the
/// receipt path and the report path cannot drift apart numerically.
#[test]
fn bucket_pricing_and_report_pricing_agree() {
    let rates = card();
    let usage = tokens(1_000_000, 200_000);
    let bucket = TierBucket {
        model: "real-model".into(),
        service_tier: None,
        tokens: usage.clone(),
    };

    let (bucket_total, omitted) = odometer_lib::query::price_buckets(
        &rates,
        codex_provider_id().as_str(),
        std::slice::from_ref(&bucket),
        RateTable::Plan,
        Utc::now(),
    );
    let direct = price_tokens(
        &rates,
        codex_provider_id().as_str(),
        "real-model",
        None,
        &usage,
        RateTable::Plan,
        Utc::now(),
    );

    assert!(omitted.is_empty());
    assert_eq!(bucket_total, direct.amount);
}

#[test]
fn session_keys_returns_every_stored_session() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[
            session_at("one", "real-model", when.timestamp_millis(), 10, 10),
            session_at("two", "real-model", when.timestamp_millis(), 10, 10),
        ],
    );

    let keys: HashMap<String, ()> = store
        .session_keys()
        .expect("keys")
        .into_iter()
        .map(|key| (key, ()))
        .collect();

    assert_eq!(keys.len(), 2);
}

/// Codex bills in plan credits and Claude in USD. Summing them produces a
/// number that is not money in any unit, and the desktop deliberately keeps
/// them apart — so the query service must too.
///
/// This is a regression test for a bug in this module's first draft, which
/// reported a single `cost` and cheerfully added 87,548 Codex credits to 126
/// Claude dollars against the real corpus.
#[test]
fn costs_are_reported_per_currency_and_never_summed_across_them() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let mut rates = card();
    rates.currency = "USD".into();
    rates
        .currencies
        .insert(codex_provider_id().as_str().to_owned(), "credits".into());
    rates.currencies.insert("claude_code".into(), "USD".into());

    let mut claude = session_at(
        "claude",
        "real-model",
        when.timestamp_millis(),
        1_000_000,
        0,
    );
    claude.harness = odometer_lib::provider::claude_code_provider_id();
    let store = ledger(
        directory.path(),
        &[
            session_at("codex", "real-model", when.timestamp_millis(), 1_000_000, 0),
            claude,
        ],
    );

    let report = range_report(
        &store,
        &rates,
        |key| {
            key.split_once(':')
                .map(|(provider, _)| provider.to_owned())
                .unwrap_or_default()
        },
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    assert_eq!(
        report.cost_by_currency.len(),
        2,
        "credits and USD must stay separate: {:?}",
        report.cost_by_currency
    );
    assert_eq!(report.cost_by_currency.get("credits"), Some(&1.0));
    assert_eq!(report.cost_by_currency.get("USD"), Some(&1.0));
}

/// End-to-end through the CLI's own report path, against a real ledger.
///
/// The parsing and rendering halves are unit-tested; this covers the seam
/// between them — flags in, ledger queried, output out — which is where a
/// wiring mistake would live and where neither half's tests would look.
#[test]
fn the_report_command_renders_a_real_ledger_end_to_end() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{report_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "cli",
            "real-model",
            when.timestamp_millis(),
            1_000_000,
            0,
        )],
    );
    let args: Vec<String> = ["--from", "2026-08-01", "--to", "2026-08-31"]
        .iter()
        .map(|value| value.to_string())
        .collect();

    let json = report_from(&store, &card(), &Config::default(), &args, Format::Json)
        .expect("report renders");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");

    assert_eq!(parsed["sessions"], 1);
    assert_eq!(parsed["tokens"]["input_tokens"], 1_000_000);
    assert_eq!(parsed["by_model"][0]["model"], "real-model");
}

/// A window that excludes every session must render an empty report rather
/// than fail — "nothing happened in July" is an answer.
#[test]
fn the_report_command_renders_an_empty_window() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{report_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "cli",
            "real-model",
            when.timestamp_millis(),
            1_000,
            0,
        )],
    );
    let args: Vec<String> = ["--from", "2026-07-01", "--to", "2026-07-31"]
        .iter()
        .map(|value| value.to_string())
        .collect();

    let text = report_from(&store, &card(), &Config::default(), &args, Format::Text)
        .expect("an empty window is an answer, not an error");

    assert!(text.contains("sessions with usage: 0"), "{text}");
    assert!(text.contains("total cost: unavailable"), "{text}");
}

/// A reversed range is rejected rather than silently returning nothing,
/// which would look identical to a quiet month.
#[test]
fn the_report_command_rejects_a_reversed_range() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{report_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);
    let args: Vec<String> = ["--from", "2026-08-31", "--to", "2026-08-01"]
        .iter()
        .map(|value| value.to_string())
        .collect();

    assert!(report_from(&store, &card(), &Config::default(), &args, Format::Text).is_err());
}

/// `status` reports the ledger it was given, and says "unavailable" rather
/// than 0 when there is none — a zero session count would read as an empty
/// install rather than an unreadable one.
#[test]
fn status_distinguishes_an_unavailable_ledger_from_an_empty_one() {
    use odometer_lib::report_cli::{render_status, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "one",
            "real-model",
            when.timestamp_millis(),
            10,
            10,
        )],
    );

    let present = render_status(Some(&store), &card(), Format::Json).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&present).unwrap();
    assert_eq!(parsed["ledger_available"], true);
    assert_eq!(parsed["sessions"], 1);

    let absent = render_status(None, &card(), Format::Json).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&absent).unwrap();
    assert_eq!(parsed["ledger_available"], false);
    assert!(
        parsed["sessions"].is_null(),
        "an unreadable ledger must not report 0 sessions"
    );
}

// ---------------------------------------------------------------------------
// Quota (issue #43): one service, fed from recently-active sessions only.
// ---------------------------------------------------------------------------

use odometer_lib::model::{RateLimitSnapshotPoint, RateLimitWindow};
use odometer_lib::query::QUOTA_LOOKBACK_DAYS;

/// A session carrying one provider rate-limit observation.
fn session_with_quota(id: &str, when_ms: i64, used_percent: f64) -> Session {
    let mut session = session_at(id, "real-model", when_ms, 1_000, 100);
    let timestamp = Utc.timestamp_millis_opt(when_ms).single().expect("instant");
    session.rate_limits_history = vec![RateLimitSnapshotPoint {
        timestamp,
        turn_id: Some(format!("{id}-turn")),
        limit_id: None,
        primary: Some(RateLimitWindow {
            used_percent,
            window_minutes: Some(300),
            resets_at: Some(timestamp + chrono::Duration::hours(5)),
        }),
        secondary: None,
        run_started_at: None,
        observation_count: 1,
    }];
    session
}

#[test]
fn quota_reports_every_provider_even_with_no_observations() {
    // #43: "no data" must be an explicit, honest state rather than an
    // absent row — an omitted provider reads as one that does not exist.
    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let snapshots =
        odometer_lib::query::quota_snapshots(&store, Utc::now(), chrono::Duration::hours(1))
            .expect("snapshots");

    assert!(
        snapshots.len() >= 2,
        "every registered provider gets a row: {snapshots:?}"
    );
    assert!(snapshots.iter().all(|snapshot| snapshot.windows.is_empty()
        || snapshot.windows.iter().all(|window| window.used.is_none())));
}

#[test]
fn quota_reads_a_recent_observation() {
    let directory = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let store = ledger(
        directory.path(),
        &[session_with_quota(
            "recent",
            (now - chrono::Duration::minutes(30)).timestamp_millis(),
            42.0,
        )],
    );

    let snapshots = odometer_lib::query::quota_snapshots(&store, now, chrono::Duration::hours(6))
        .expect("snapshots");

    let codex = snapshots
        .iter()
        .find(|snapshot| snapshot.provider == codex_provider_id())
        .expect("codex row");
    let window = codex
        .windows
        .iter()
        .find(|window| window.used.is_some())
        .expect("a window with a reading");
    assert_eq!(window.used, Some(42.0));
    assert!(!window.stale, "a 30-minute-old reading is not stale here");
}

#[test]
fn an_old_observation_is_marked_stale_rather_than_hidden() {
    // A stale number is a different and more honest fact than no number,
    // and both differ from zero usage.
    let directory = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let store = ledger(
        directory.path(),
        &[session_with_quota(
            "old",
            (now - chrono::Duration::hours(20)).timestamp_millis(),
            80.0,
        )],
    );

    let snapshots = odometer_lib::query::quota_snapshots(&store, now, chrono::Duration::hours(1))
        .expect("snapshots");

    let window = snapshots
        .iter()
        .find(|snapshot| snapshot.provider == codex_provider_id())
        .expect("codex row")
        .windows
        .iter()
        .find(|window| window.used.is_some())
        .expect("the reading is still reported");
    assert_eq!(window.used, Some(80.0));
    assert!(window.stale, "an out-of-date reading must say so");
}

#[test]
fn the_lookback_filters_on_when_a_session_was_last_observed() {
    // What keeps a quota answer cheap enough for a shell prompt. Note the
    // filter is `last_seen_at_ms` — when Odometer last *observed* the file,
    // not when the work happened. That is the correct axis here: a session
    // Odometer has not seen in a fortnight cannot hold an observation
    // relevant to a window measured in hours or days.
    let directory = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let store = ledger(
        directory.path(),
        &[session_with_quota("seen-now", now.timestamp_millis(), 50.0)],
    );

    // Observed just now, so a fortnight-old cutoff includes it...
    let included = store
        .session_keys_since((now - chrono::Duration::days(QUOTA_LOOKBACK_DAYS)).timestamp_millis())
        .expect("keys");
    assert_eq!(included.len(), 1);

    // ...and a cutoff after the observation excludes it, which is the
    // bound that stops a quota query walking the whole corpus.
    let excluded = store
        .session_keys_since((now + chrono::Duration::hours(1)).timestamp_millis())
        .expect("keys");
    assert!(excluded.is_empty(), "the cutoff must actually exclude");
}

#[test]
fn quota_text_output_never_renders_unlimited_as_a_number() {
    use odometer_lib::report_cli::{render_quota, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let rendered = render_quota(&store, chrono::Duration::hours(1), Utc::now(), Format::Text)
        .expect("renders");

    // With no observations at all, the output must say so per provider
    // rather than print zeros that read as "you have used nothing".
    assert!(rendered.contains("no quota observations"), "{rendered}");
    assert!(!rendered.contains("0 used"), "{rendered}");
}

#[test]
fn quota_csv_output_has_a_stable_header_and_one_row_per_window() {
    use odometer_lib::report_cli::{render_quota, Format};

    let directory = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let store = ledger(
        directory.path(),
        &[session_with_quota(
            "csv",
            (now - chrono::Duration::minutes(10)).timestamp_millis(),
            55.0,
        )],
    );

    let rendered =
        render_quota(&store, chrono::Duration::hours(6), now, Format::Csv).expect("renders");
    let mut lines = rendered.lines();

    assert_eq!(
        lines.next().unwrap(),
        "provider,window,unit,used,remaining,limit,unlimited,resets_at,stale,unavailable"
    );
    let rows: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
    assert!(
        rows.iter().any(|row| row.contains("55")),
        "the reading must appear in the rows: {rows:?}"
    );
}

#[test]
fn quota_text_output_renders_a_reading_with_its_reset_time() {
    use odometer_lib::report_cli::{render_quota, Format};

    let directory = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let store = ledger(
        directory.path(),
        &[session_with_quota(
            "text",
            (now - chrono::Duration::minutes(10)).timestamp_millis(),
            55.0,
        )],
    );

    let rendered =
        render_quota(&store, chrono::Duration::hours(6), now, Format::Text).expect("renders");

    assert!(rendered.contains("55 used"), "{rendered}");
    assert!(rendered.contains("resets "), "{rendered}");
    assert!(
        !rendered.contains("[stale]"),
        "a ten-minute-old reading is current: {rendered}"
    );
}

#[test]
fn quota_json_output_is_parseable_and_names_every_provider() {
    use odometer_lib::report_cli::{render_quota, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let rendered = render_quota(&store, chrono::Duration::hours(1), Utc::now(), Format::Json)
        .expect("renders");
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");

    let providers: Vec<&str> = parsed
        .as_array()
        .expect("an array of snapshots")
        .iter()
        .filter_map(|snapshot| snapshot["provider"].as_str())
        .collect();
    assert!(
        providers.contains(&codex_provider_id().as_str()),
        "every provider gets a row: {providers:?}"
    );
}

/// #43: "Forecast math is deterministic and suppresses projections when
/// evidence is insufficient." Two observations is below
/// `MIN_EVIDENCE_POINTS`, so no pace line may appear — extrapolating a
/// burn rate from two readings is the kind of confident guess this
/// criterion exists to prevent.
#[test]
fn quota_suppresses_a_forecast_when_the_evidence_is_thin() {
    use odometer_lib::report_cli::{render_quota, Format};

    let directory = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let started = now - chrono::Duration::hours(2);
    let resets_at = now + chrono::Duration::hours(3);
    let mut session = session_at("thin", "real-model", started.timestamp_millis(), 1_000, 100);
    session.rate_limits_history = vec![
        quota_point(started, "turn-1", 10.0, resets_at),
        quota_point(
            now - chrono::Duration::minutes(5),
            "turn-2",
            40.0,
            resets_at,
        ),
    ];
    let store = ledger(directory.path(), &[session]);

    let rendered =
        render_quota(&store, chrono::Duration::hours(6), now, Format::Text).expect("renders");

    assert!(
        rendered.contains("40 used"),
        "the reading is still shown: {rendered}"
    );
    assert!(
        !rendered.contains("pace "),
        "two observations must not produce a projection: {rendered}"
    );
}

/// The other side of that gate: with enough in-window observations spanning
/// enough of the window, the pace and reserve/deficit line is rendered.
#[test]
fn quota_renders_a_forecast_once_there_is_enough_evidence() {
    use odometer_lib::report_cli::{render_quota, Format};

    let directory = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let resets_at = now + chrono::Duration::hours(3);
    let started = now - chrono::Duration::hours(2);
    let mut session = session_at(
        "thick",
        "real-model",
        started.timestamp_millis(),
        1_000,
        100,
    );
    // Six readings across two hours of a five-hour window: over
    // MIN_EVIDENCE_POINTS, and well over MIN_EVIDENCE_SPAN_FRACTION.
    session.rate_limits_history = (0..6)
        .map(|index| {
            quota_point(
                started + chrono::Duration::minutes(index * 24),
                &format!("turn-{index}"),
                10.0 + index as f64 * 5.0,
                resets_at,
            )
        })
        .collect();
    let store = ledger(directory.path(), &[session]);

    let rendered =
        render_quota(&store, chrono::Duration::hours(6), now, Format::Text).expect("renders");

    assert!(
        rendered.contains("pace "),
        "expected a pace line: {rendered}"
    );
    assert!(
        rendered.contains("deficit") || rendered.contains("reserve"),
        "a forecast must say which side of an even pace it is on: {rendered}"
    );
}

/// An unlimited credit balance renders the word, never a number — "0 used"
/// would be wrong in both directions.
#[test]
fn quota_renders_an_unlimited_window_as_a_state_not_a_number() {
    use odometer_lib::report_cli::{render_quota, Format};

    let directory = tempfile::tempdir().unwrap();
    let now = Utc::now();
    let store = ledger(
        directory.path(),
        &[session_with_quota(
            "unlimited",
            (now - chrono::Duration::minutes(10)).timestamp_millis(),
            20.0,
        )],
    );

    let rendered =
        render_quota(&store, chrono::Duration::hours(6), now, Format::Text).expect("renders");

    assert!(rendered.contains("unlimited"), "{rendered}");
}

fn quota_point(
    at: chrono::DateTime<Utc>,
    turn: &str,
    used_percent: f64,
    resets_at: chrono::DateTime<Utc>,
) -> RateLimitSnapshotPoint {
    RateLimitSnapshotPoint {
        timestamp: at,
        turn_id: Some(turn.to_string()),
        limit_id: None,
        primary: Some(RateLimitWindow {
            used_percent,
            window_minutes: Some(300),
            resets_at: Some(resets_at),
        }),
        secondary: None,
        run_started_at: None,
        observation_count: 1,
    }
}

// ---------------------------------------------------------------------------
// Per-project reporting (issue #47's `projects`, over #41's dimension).
// ---------------------------------------------------------------------------

/// A session whose stored project identity is set directly, so a test can
/// choose the provenance without needing a real repository on disk.
fn session_in_project(
    id: &str,
    when_ms: i64,
    project_key: &str,
    label: &str,
    provenance: &str,
) -> Session {
    let mut session = session_at(id, "real-model", when_ms, 1_000, 100);
    session.working_directory = Some(label.to_string());
    session.project_key = Some(project_key.to_string());
    session.project_label = Some(label.to_string());
    session.project_provenance = Some(match provenance {
        "fallback_path_identity" => {
            odometer_lib::project_identity::ProjectProvenance::FallbackPathIdentity
        }
        _ => odometer_lib::project_identity::ProjectProvenance::RepositoryRoot,
    });
    session
}

#[test]
fn a_project_report_groups_usage_and_counts_sessions() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    // Real, distinct directories: project identity is computed from the
    // working directory, so two sessions only share a project when they
    // genuinely share a directory.
    let shared = directory.path().join("shared-project");
    let other = directory.path().join("other-project");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::create_dir_all(&other).unwrap();
    let store = ledger(
        directory.path(),
        &[
            session_in_project(
                "a",
                when.timestamp_millis(),
                "repo:one",
                &shared.to_string_lossy(),
                "repository_root",
            ),
            session_in_project(
                "b",
                when.timestamp_millis() + 3_600_000,
                "repo:one",
                &shared.to_string_lossy(),
                "repository_root",
            ),
            session_in_project(
                "c",
                when.timestamp_millis(),
                "repo:two",
                &other.to_string_lossy(),
                "repository_root",
            ),
        ],
    );

    let report = odometer_lib::query::project_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    // The store recomputes `project_key` from the working directory, so the
    // assertion is on the grouping, not on a synthetic key.
    assert_eq!(report.projects.len(), 2);
    let mut counts: Vec<usize> = report
        .projects
        .iter()
        .map(|project| project.sessions)
        .collect();
    counts.sort_unstable();
    assert_eq!(counts, vec![1, 2]);
    let two_session = report
        .projects
        .iter()
        .find(|project| project.sessions == 2)
        .expect("the shared-directory project");
    assert_eq!(two_session.tokens.input_tokens, 2_000);
}

/// #41's redaction contract, applied where data leaves the desktop. A
/// `fallback_path_identity` label *is* an absolute local path, and CLI
/// output gets piped into files and pasted into issues.
#[test]
fn a_path_identified_project_is_redacted_unless_paths_are_requested() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    // A real directory with no repository or workspace marker, so
    // identity falls back to the path on every platform. A hard-coded
    // Windows path is not a path on Linux, so it resolved differently
    // there and this test passed only on Windows.
    let work = directory.path().join("private-work");
    std::fs::create_dir_all(&work).unwrap();
    let work_label = work.to_string_lossy().to_string();
    let store = ledger(
        directory.path(),
        &[session_in_project(
            "p",
            when.timestamp_millis(),
            "path:abc123",
            &work_label,
            "fallback_path_identity",
        )],
    );

    let report = odometer_lib::query::project_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    let project = &report.projects[0];
    assert!(project.label_is_path);
    assert_eq!(
        project.redacted_label(),
        project.project_key,
        "a path label must fall back to the already-hashed key"
    );
    assert!(
        project.project_key.starts_with("path:"),
        "a directory with no repository root is path-identified: {}",
        project.project_key
    );
    // Not an exact match: the stored label is already display-shortened,
    // so what matters is that opting in still names the directory and
    // the default does not.
    assert!(
        project.label_for(true).contains("private-work"),
        "opting in must name the directory: {}",
        project.label_for(true)
    );
    let _ = &work_label;
    assert!(
        !project.redacted_label().contains("private-work"),
        "the redacted form must not carry the directory name"
    );
}

/// A repository-root label is a project name, not a path, so it is not
/// redacted — over-redacting would make the report useless.
#[test]
fn a_repository_project_label_is_not_redacted() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_in_project(
            "r",
            when.timestamp_millis(),
            "repo:abc",
            "my-project",
            "repository_root",
        )],
    );

    let report = odometer_lib::query::project_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    assert!(!report.projects[0].label_is_path);
    assert_eq!(report.projects[0].redacted_label(), "my-project");
}

/// JSON is the format most likely to be stored or piped, so its labels must
/// be redacted too — not just the human-readable text output.
#[test]
fn project_json_output_redacts_by_default_and_honours_the_opt_in() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{projects_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    // A real directory with no repository or workspace marker, so
    // identity falls back to the path on every platform. A hard-coded
    // Windows path is not a path on Linux, so it resolved differently
    // there and this test passed only on Windows.
    let work = directory.path().join("private-work");
    std::fs::create_dir_all(&work).unwrap();
    let work_label = work.to_string_lossy().to_string();
    let store = ledger(
        directory.path(),
        &[session_in_project(
            "p",
            when.timestamp_millis(),
            "path:abc123",
            &work_label,
            "fallback_path_identity",
        )],
    );

    let default = projects_from(&store, &card(), &Config::default(), &[], Format::Json).unwrap();
    assert!(
        !default.contains("private-work"),
        "default JSON must not carry a local path: {default}"
    );

    let opted_in = projects_from(
        &store,
        &card(),
        &Config::default(),
        &["--include-paths".to_string()],
        Format::Json,
    )
    .unwrap();
    assert!(opted_in.contains("private-work"));
}

/// Sessions with usage but no project are counted, not dropped — otherwise
/// per-project totals silently fail to reconcile with the overall report.
#[test]
fn sessions_without_a_project_are_reported_rather_than_dropped() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let mut orphan = session_at("orphan", "real-model", when.timestamp_millis(), 1_000, 0);
    orphan.working_directory = None;
    orphan.project_key = None;
    orphan.project_label = None;
    orphan.project_provenance = None;
    let store = ledger(directory.path(), &[orphan]);

    let report = odometer_lib::query::project_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    assert!(report.projects.is_empty());
    assert_eq!(report.sessions_without_project, 1);
}

#[test]
fn project_csv_output_quotes_labels_and_names_the_currency() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{projects_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    // A label with a comma in it: aliases are user-supplied, so an
    // unquoted field would shift every later column in a spreadsheet.
    let work = directory.path().join("comma, project");
    std::fs::create_dir_all(&work).unwrap();
    let store = ledger(
        directory.path(),
        &[session_in_project(
            "c",
            when.timestamp_millis(),
            "repo:comma",
            &work.to_string_lossy(),
            "repository_root",
        )],
    );

    let rendered = projects_from(
        &store,
        &card(),
        &Config::default(),
        &["--include-paths".to_string()],
        Format::Csv,
    )
    .unwrap();
    let mut lines = rendered.lines();

    assert_eq!(
        lines.next().unwrap(),
        "project_key,label,sessions,total_tokens,cost,currency"
    );
    let row = lines.next().expect("one project row");
    assert!(
        row.contains('"'),
        "a label containing a comma must be quoted: {row}"
    );
}

#[test]
fn project_text_output_says_so_when_the_window_is_empty() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{projects_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let rendered = projects_from(&store, &card(), &Config::default(), &[], Format::Text).unwrap();

    assert!(
        rendered.contains("no project activity"),
        "an empty window is an answer, not blank output: {rendered}"
    );
}

#[test]
fn project_text_output_reports_orphan_sessions() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{projects_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let mut orphan = session_at("orphan", "real-model", when.timestamp_millis(), 1_000, 0);
    orphan.working_directory = None;
    orphan.project_key = None;
    orphan.project_label = None;
    orphan.project_provenance = None;
    let store = ledger(directory.path(), &[orphan]);

    let rendered = projects_from(&store, &card(), &Config::default(), &[], Format::Text).unwrap();

    assert!(
        rendered.contains("belong to no project"),
        "orphans must be visible so totals reconcile: {rendered}"
    );
}

// ---------------------------------------------------------------------------
// Workflow metrics (issue #45)
// ---------------------------------------------------------------------------

fn metric<'a>(
    report: &'a odometer_lib::query::WorkflowMetrics,
    id: &str,
) -> &'a odometer_lib::query::WorkflowMetric {
    report
        .metrics
        .iter()
        .find(|metric| metric.id == id)
        .unwrap_or_else(|| panic!("no metric {id}"))
}

fn metrics_for(store: &HistoryStore, rates: &RateCard) -> odometer_lib::query::WorkflowMetrics {
    odometer_lib::query::workflow_metrics(
        store,
        rates,
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Utc::now(),
    )
    .expect("metrics")
}

/// #45: "Every metric has a documented denominator, coverage rule,
/// missing-data state."
///
/// An empty corpus gives every metric a zero denominator, and the value must
/// then be absent rather than 0.0 — "no tool calls happened" and "no tool
/// call failed" are opposite facts that a bare 0.0 collapses into one. A
/// float division would also produce NaN here and poison anything that
/// aggregated it.
#[test]
fn a_metric_with_no_denominator_reports_no_value_rather_than_zero() {
    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let report = metrics_for(&store, &card());

    assert_eq!(report.sessions, 0);
    for metric in &report.metrics {
        assert_eq!(
            metric.value, None,
            "{} must have no value on an empty corpus",
            metric.id
        );
        assert_eq!(metric.denominator, 0.0);
        assert!(
            !metric.denominator_is.is_empty(),
            "{} must document what its denominator counts",
            metric.id
        );
    }
}

#[test]
fn metrics_carry_their_definition_version() {
    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    // A changed denominator silently changes the meaning of a series, so a
    // stored comparison needs to be able to refuse to compare across
    // versions rather than plot a discontinuity as a trend.
    assert_eq!(
        metrics_for(&store, &card()).schema_version,
        odometer_lib::query::WORKFLOW_METRICS_VERSION
    );
}

#[test]
fn context_to_output_ratio_divides_input_by_output() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "r",
            "real-model",
            when.timestamp_millis(),
            1_000,
            250,
        )],
    );

    let report = metrics_for(&store, &card());
    let ratio = metric(&report, "context_to_output_ratio");

    assert_eq!(ratio.numerator, 1_000.0);
    assert_eq!(ratio.denominator, 250.0);
    assert_eq!(ratio.value, Some(4.0));
}

/// A fallback rate produces a number, but it is a stand-in. Counting it as
/// covered is exactly how a pricing gap hides — the metric would read 100%
/// while every model was being priced at someone else's rate.
#[test]
fn pricing_coverage_excludes_fallback_priced_usage() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let mut rates = card();
    rates.fallback_models.clear();
    let store = ledger(
        directory.path(),
        &[
            session_at("known", "real-model", when.timestamp_millis(), 1_000, 0),
            session_at(
                "unknown",
                "model-not-in-the-card",
                when.timestamp_millis(),
                1_000,
                0,
            ),
        ],
    );

    let report = metrics_for(&store, &rates);
    let coverage = metric(&report, "pricing_coverage");

    assert_eq!(
        coverage.denominator, 2_000.0,
        "both models are priceable usage"
    );
    assert_eq!(
        coverage.numerator, 1_000.0,
        "only the directly-priced model counts as covered"
    );
    assert_eq!(coverage.value, Some(0.5));
}

/// An aliased model *is* covered: it resolves to a real published rate for
/// the model it points at, unlike a fallback.
#[test]
fn pricing_coverage_counts_an_aliased_model_as_covered() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let mut rates = card();
    rates
        .model_aliases
        .insert("aliased-model".into(), "real-model".into());
    let store = ledger(
        directory.path(),
        &[session_at(
            "aliased",
            "aliased-model",
            when.timestamp_millis(),
            1_000,
            0,
        )],
    );

    let report = metrics_for(&store, &rates);

    assert_eq!(metric(&report, "pricing_coverage").value, Some(1.0));
}

#[test]
fn metrics_text_output_shows_the_denominator_beside_the_value() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{metrics_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "t",
            "real-model",
            when.timestamp_millis(),
            1_000,
            250,
        )],
    );

    let rendered = metrics_from(&store, &card(), &Config::default(), &[], Format::Text).unwrap();

    // A ratio without its sample size is not interpretable, so the rendering
    // must not print one alone.
    assert!(rendered.contains("context_to_output_ratio"), "{rendered}");
    assert!(rendered.contains("output tokens produced"), "{rendered}");
    assert!(rendered.contains("metrics v"), "{rendered}");
}

#[test]
fn metrics_text_output_marks_a_metric_with_no_evidence() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{metrics_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let rendered = metrics_from(&store, &card(), &Config::default(), &[], Format::Text).unwrap();

    assert!(
        rendered.contains("n/a"),
        "a metric with no denominator must read as unavailable, not 0: {rendered}"
    );
}

#[test]
fn metrics_csv_output_carries_the_denominator_and_its_description() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{metrics_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "csv",
            "real-model",
            when.timestamp_millis(),
            1_000,
            250,
        )],
    );

    let rendered = metrics_from(&store, &card(), &Config::default(), &[], Format::Csv).unwrap();
    let mut lines = rendered.lines();

    assert_eq!(
        lines.next().unwrap(),
        "metric,value,numerator,denominator,denominator_is"
    );
    let rows: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
    assert_eq!(rows.len(), 5, "one row per metric");
    let ratio = rows
        .iter()
        .find(|row| row.starts_with("context_to_output_ratio,"))
        .expect("the ratio row");
    // The description contains a comma-free phrase today, but the field is
    // quoted through the same helper the projects report uses, so a future
    // wording change cannot shift the columns.
    assert!(ratio.contains("1000"), "{ratio}");
    assert!(ratio.contains("250"), "{ratio}");
}

#[test]
fn metrics_json_output_parses_and_keeps_absent_values_null() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{metrics_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let rendered = metrics_from(&store, &card(), &Config::default(), &[], Format::Json).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");

    assert_eq!(
        parsed["schema_version"],
        odometer_lib::query::WORKFLOW_METRICS_VERSION
    );
    for metric in parsed["metrics"].as_array().expect("metrics array") {
        assert!(
            metric["value"].is_null(),
            "an absent value must be null, not 0: {metric}"
        );
        assert!(metric["denominator_is"]
            .as_str()
            .is_some_and(|d| !d.is_empty()));
    }
}

// ---------------------------------------------------------------------------
// Mirrored-history detection (issue #40)
// ---------------------------------------------------------------------------

/// The same underlying run recorded by two tools must be detected, or its
/// usage is counted twice in every total.
///
/// Synthetic by necessity: the corpus this was written against contains no
/// cross-provider duplicates at all, so there is no local evidence to test
/// with. That is a reason to build the fixture, not a reason to skip the
/// feature.
#[test]
fn a_run_recorded_by_two_providers_is_detected_as_mirrored() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();

    // Identical content, stored under two providers: same start time, same
    // first turn id, same first usage event — which is what the first-event
    // fingerprint hashes.
    let codex = session_at(
        "shared-run",
        "real-model",
        when.timestamp_millis(),
        1_000,
        100,
    );
    let mut claude = codex.clone();
    claude.harness = odometer_lib::provider::claude_code_provider_id();

    let store = ledger(directory.path(), &[codex, claude]);

    let groups = store.mirrored_session_groups().expect("groups");

    assert_eq!(groups.len(), 1, "the two copies form one group: {groups:?}");
    assert_eq!(groups[0].session_keys.len(), 2);
    assert_eq!(
        groups[0].providers,
        vec!["claude_code".to_string(), "codex".to_string()],
        "the group names both providers so a reader knows which tools disagree"
    );
}

/// Two sessions under the *same* provider sharing a fingerprint are the
/// existing collision case, already flagged by `collision`. Reporting them
/// as mirrors would be a false positive on ordinary data — this corpus has
/// eight such same-provider pairs and zero real mirrors.
#[test]
fn a_same_provider_fingerprint_collision_is_not_reported_as_a_mirror() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let first = session_at("collide", "real-model", when.timestamp_millis(), 1_000, 100);
    let mut second = first.clone();
    second.id = "collide-2".into();

    let store = ledger(directory.path(), &[first, second]);

    assert!(
        store.mirrored_session_groups().expect("groups").is_empty(),
        "a single-provider collision is not a mirror"
    );
}

#[test]
fn distinct_runs_are_not_reported_as_mirrored() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let codex = session_at("one", "real-model", when.timestamp_millis(), 1_000, 100);
    let mut claude = session_at(
        "two",
        "real-model",
        when.timestamp_millis() + 60_000,
        2_000,
        200,
    );
    claude.harness = odometer_lib::provider::claude_code_provider_id();

    let store = ledger(directory.path(), &[codex, claude]);

    assert!(store.mirrored_session_groups().expect("groups").is_empty());
}

#[test]
fn the_mirrors_command_says_so_when_none_are_found() {
    use odometer_lib::report_cli::{render_mirrors, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let rendered = render_mirrors(&store, Format::Text).expect("renders");

    // "None found" is a result. Blank output reads as a broken command.
    assert!(rendered.contains("no mirrored sessions"), "{rendered}");
}

#[test]
fn the_mirrors_command_explains_why_a_group_matters() {
    use odometer_lib::report_cli::{render_mirrors, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let codex = session_at("shared", "real-model", when.timestamp_millis(), 1_000, 100);
    let mut claude = codex.clone();
    claude.harness = odometer_lib::provider::claude_code_provider_id();
    let store = ledger(directory.path(), &[codex, claude]);

    let rendered = render_mirrors(&store, Format::Text).expect("renders");

    assert!(
        rendered.contains("double counted"),
        "a detection is only useful if it says what it means: {rendered}"
    );
    assert!(rendered.contains("codex"), "{rendered}");
    assert!(rendered.contains("claude_code"), "{rendered}");
}

#[test]
fn mirrors_csv_and_json_carry_one_entry_per_copy() {
    use odometer_lib::report_cli::{render_mirrors, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let codex = session_at("shared", "real-model", when.timestamp_millis(), 1_000, 100);
    let mut claude = codex.clone();
    claude.harness = odometer_lib::provider::claude_code_provider_id();
    let store = ledger(directory.path(), &[codex, claude]);

    let csv = render_mirrors(&store, Format::Csv).expect("csv");
    let mut lines = csv.lines();
    assert_eq!(lines.next().unwrap(), "fingerprint,providers,session_key");
    let rows: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
    assert_eq!(rows.len(), 2, "one row per copy, not per group: {rows:?}");

    let json = render_mirrors(&store, Format::Json).expect("json");
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    let groups = parsed.as_array().expect("array of groups");
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["session_keys"].as_array().map(Vec::len), Some(2));
}

#[test]
fn mirrors_csv_is_empty_apart_from_its_header_when_none_are_found() {
    use odometer_lib::report_cli::{render_mirrors, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let csv = render_mirrors(&store, Format::Csv).expect("csv");

    // A header with no rows is a valid, parseable "none" for a script.
    assert_eq!(csv.trim(), "fingerprint,providers,session_key");
}

// ---------------------------------------------------------------------------
// Per-session listing (issue #47's `sessions`)
// ---------------------------------------------------------------------------

fn sessions_ledger(directory: &Path) -> HistoryStore {
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let sessions: Vec<Session> = (0..5)
        .map(|index| {
            session_at(
                &format!("s{index}"),
                "real-model",
                when.timestamp_millis() + index as i64 * 3_600_000,
                1_000 * (index as u64 + 1),
                0,
            )
        })
        .collect();
    ledger(directory, &sessions)
}

#[test]
fn a_session_listing_is_ordered_heaviest_first() {
    let directory = tempfile::tempdir().unwrap();
    let store = sessions_ledger(directory.path());

    let report = odometer_lib::query::session_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        None,
        Utc::now(),
    )
    .expect("report");

    let totals: Vec<u64> = report
        .sessions
        .iter()
        .map(|session| session.tokens.total_tokens)
        .collect();
    let mut sorted = totals.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    assert_eq!(totals, sorted, "heaviest first is the order a reader wants");
    assert_eq!(report.truncated_to, None, "nothing was cut");
}

/// A truncated list that looks complete is how a partial total gets read as
/// the whole picture, so the truncation is recorded in the payload and
/// stated in the rendering.
#[test]
fn a_truncated_listing_says_it_was_truncated() {
    let directory = tempfile::tempdir().unwrap();
    let store = sessions_ledger(directory.path());

    let report = odometer_lib::query::session_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Some(2),
        Utc::now(),
    )
    .expect("report");

    assert_eq!(report.sessions.len(), 2);
    assert_eq!(report.truncated_to, Some(2));
}

#[test]
fn a_limit_larger_than_the_corpus_is_not_reported_as_truncation() {
    let directory = tempfile::tempdir().unwrap();
    let store = sessions_ledger(directory.path());

    let report = odometer_lib::query::session_report(
        &store,
        &card(),
        |_| codex_provider_id().as_str().to_owned(),
        None,
        None,
        Some(500),
        Utc::now(),
    )
    .expect("report");

    assert_eq!(report.sessions.len(), 5);
    assert_eq!(
        report.truncated_to, None,
        "a limit that cut nothing must not claim it did"
    );
}

#[test]
fn a_zero_limit_lists_every_session() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{sessions_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = sessions_ledger(directory.path());

    // `--limit 0` means "no limit", the only useful reading — "show
    // nothing" would be a command nobody runs.
    let rendered = sessions_from(
        &store,
        &card(),
        &Config::default(),
        &["--limit".to_string(), "0".to_string()],
        Format::Csv,
    )
    .unwrap();

    let rows = rendered
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .count();
    assert_eq!(rows, 5);
}

#[test]
fn a_malformed_limit_is_rejected_rather_than_ignored() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{sessions_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = sessions_ledger(directory.path());

    let error = sessions_from(
        &store,
        &card(),
        &Config::default(),
        &["--limit".to_string(), "twenty".to_string()],
        Format::Text,
    )
    .unwrap_err()
    .to_string();

    assert!(
        error.contains("twenty"),
        "the error must name the value: {error}"
    );
}

#[test]
fn a_session_listing_names_models_it_could_not_price() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{sessions_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let mut rates = card();
    rates.unpriced_models.push("mystery-model".into());
    let store = ledger(
        directory.path(),
        &[session_at(
            "unpriced",
            "mystery-model",
            when.timestamp_millis(),
            1_000,
            0,
        )],
    );

    let rendered = sessions_from(&store, &rates, &Config::default(), &[], Format::Text).unwrap();

    assert!(
        rendered.contains("unpriced"),
        "a per-session cost must never be a silent floor: {rendered}"
    );
}

#[test]
fn an_empty_window_says_so_rather_than_printing_nothing() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{sessions_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let rendered = sessions_from(&store, &card(), &Config::default(), &[], Format::Text).unwrap();

    assert!(rendered.contains("no session activity"), "{rendered}");
}

#[test]
fn session_csv_and_json_carry_the_same_rows() {
    use odometer_lib::config::Config;
    use odometer_lib::report_cli::{sessions_from, Format};

    let directory = tempfile::tempdir().unwrap();
    let store = sessions_ledger(directory.path());
    let args = vec!["--limit".to_string(), "0".to_string()];

    let csv = sessions_from(&store, &card(), &Config::default(), &args, Format::Csv).unwrap();
    assert_eq!(
        csv.lines().next().unwrap(),
        "session_key,harness,total_tokens,input,output,cost,currency"
    );

    let json = sessions_from(&store, &card(), &Config::default(), &args, Format::Json).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
    assert_eq!(
        parsed["sessions"].as_array().map(Vec::len),
        Some(5),
        "both formats list the same sessions"
    );
    assert_eq!(
        parsed["schema_version"],
        odometer_lib::query::SESSION_REPORT_SCHEMA_VERSION
    );
}

// ---------------------------------------------------------------------------
// MCP tools (issue #47)
// ---------------------------------------------------------------------------

fn call_mcp(store: &HistoryStore, rates: &RateCard, params: serde_json::Value) -> String {
    odometer_lib::mcp_server::call_tool_with(store, rates, &params, Utc::now()).expect("tool call")
}

#[test]
fn the_usage_report_tool_answers_from_the_ledger() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "mcp",
            "real-model",
            when.timestamp_millis(),
            1_000,
            100,
        )],
    );

    let text = call_mcp(
        &store,
        &card(),
        serde_json::json!({ "name": "usage_report", "arguments": {} }),
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("tool returns JSON");

    assert_eq!(parsed["sessions"], 1);
    assert_eq!(parsed["tokens"]["input_tokens"], 1_000);
}

/// The CLI has `--include-paths` because a person can consent to seeing
/// their own paths. An agent over MCP is a different audience with nobody
/// present to make that call, so redaction is unconditional here.
#[test]
fn the_project_tool_never_returns_a_local_path() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let work = directory.path().join("secret-project");
    std::fs::create_dir_all(&work).unwrap();
    let store = ledger(
        directory.path(),
        &[session_in_project(
            "p",
            when.timestamp_millis(),
            "path:abc",
            &work.to_string_lossy(),
            "fallback_path_identity",
        )],
    );

    let text = call_mcp(
        &store,
        &card(),
        serde_json::json!({ "name": "project_report", "arguments": {} }),
    );

    assert!(
        !text.contains("secret-project"),
        "MCP must never hand an agent a local path: {text}"
    );
    assert!(
        text.contains("path:"),
        "the hashed key still identifies it: {text}"
    );
}

#[test]
fn the_metrics_tool_returns_versioned_metrics() {
    let directory = tempfile::tempdir().unwrap();
    let when = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "m",
            "real-model",
            when.timestamp_millis(),
            1_000,
            250,
        )],
    );

    let text = call_mcp(
        &store,
        &card(),
        serde_json::json!({ "name": "workflow_metrics", "arguments": {} }),
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("JSON");

    assert_eq!(
        parsed["schema_version"],
        odometer_lib::query::WORKFLOW_METRICS_VERSION
    );
}

#[test]
fn the_quota_tool_answers_without_a_window_argument() {
    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let text = call_mcp(
        &store,
        &card(),
        serde_json::json!({ "name": "quota_status", "arguments": {} }),
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("JSON");

    assert!(parsed.is_array(), "quota returns one entry per provider");
}

#[test]
fn a_tool_window_argument_filters_the_result() {
    let directory = tempfile::tempdir().unwrap();
    let august = Utc.with_ymd_and_hms(2026, 8, 10, 12, 0, 0).unwrap();
    let store = ledger(
        directory.path(),
        &[session_at(
            "aug",
            "real-model",
            august.timestamp_millis(),
            1_000,
            0,
        )],
    );

    let text = call_mcp(
        &store,
        &card(),
        serde_json::json!({
            "name": "usage_report",
            "arguments": { "from": "2026-07-01", "to": "2026-07-31" }
        }),
    );
    let parsed: serde_json::Value = serde_json::from_str(&text).expect("JSON");

    assert_eq!(parsed["sessions"], 0, "the August session is outside July");
}

#[test]
fn a_tool_call_without_a_name_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let store = ledger(directory.path(), &[]);

    let error = odometer_lib::mcp_server::call_tool_with(
        &store,
        &card(),
        &serde_json::json!({ "arguments": {} }),
        Utc::now(),
    )
    .expect_err("a call with no tool name is malformed");

    assert!(format!("{error:?}").contains("name"), "{error:?}");
}
