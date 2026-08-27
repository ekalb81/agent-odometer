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
