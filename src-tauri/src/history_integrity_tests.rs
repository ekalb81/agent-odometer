// Included in history_store::tests to reuse synthetic ledger fixtures.
fn integrity_pair() -> (tempfile::TempDir, HistoryStore, Vec<String>) {
    let (directory, store) = store();
    let generation = store.begin_scan().unwrap();
    let mut first = session("integrity-shared", 10);
    first.turns.push(TurnInfo {
        turn_id: "first".into(),
        ..TurnInfo::default()
    });
    let mut second = first.clone();
    second.turns[0].turn_id = "second".into();
    let left = store
        .observe(Path::new("first.jsonl"), &first, generation)
        .unwrap();
    let right = store
        .observe(Path::new("second.jsonl"), &second, generation)
        .unwrap();
    (directory, store, vec![left.key, right.key])
}

#[test]
fn integrity_complete_delta_scope_rejects_duplicates_but_preserves_inspection() {
    let (_directory, store, keys) = integrity_pair();
    let clean = store.range_totals_multi(&keys, &[(None, None)]).unwrap();
    assert_eq!(
        clean[0]
            .values()
            .map(|v| v.tokens.total_tokens)
            .sum::<u64>(),
        30
    );
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE durable_sessions SET lifecycle = 'superseded' WHERE session_key = ?1",
            [&keys[0]],
        )
        .unwrap();
    for fetched in [&keys[..1], &keys[..0], keys.as_slice()] {
        let error = store
            .range_totals_multi_for_scope(fetched, &keys, &[(None, None)])
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AccountingIntegrityError>(),
            Some(&AccountingIntegrityError::Ambiguous)
        );
    }
    // Selecting one source counts one copy; distinct filters are not blanket-blocked.
    assert_eq!(
        store
            .range_totals_multi(&keys[..1], &[(None, None)])
            .unwrap()[0][&keys[0]]
            .tokens
            .total_tokens,
        15
    );
    assert!(store
        .range_totals_multi(&keys, &[(Some(timestamp("2026-01-02T00:00:00Z")), None)])
        .unwrap()[0]
        .is_empty());
    assert_eq!(store.session_count().unwrap(), 2);
    assert_eq!(
        store
            .load_one(&keys[0])
            .unwrap()
            .session
            .tokens_total
            .total_tokens,
        15
    );

    let mut newer = store.load_one(&keys[1]).unwrap().session;
    // A parser supplies its provider identity, not the archive collision key.
    newer.storage_id = crate::model::storage_id_for_session(&newer.harness, &newer.id);
    let mut point = newer.tokens_history[0].clone();
    point.timestamp = timestamp("2026-01-02T00:00:01Z");
    point.delta = totals(20);
    point.total_tokens = 45;
    newer.tokens_history.push(point);
    newer.tokens_total = totals(30);
    newer.last_event_at = timestamp("2026-01-02T00:00:01Z");
    assert_eq!(
        store
            .observe(
                Path::new("second.jsonl"),
                &newer,
                store.begin_scan().unwrap()
            )
            .unwrap()
            .key,
        keys[1]
    );
    let recent = store
        .range_totals_multi(&keys, &[(Some(timestamp("2026-01-02T00:00:00Z")), None)])
        .unwrap();
    assert_eq!(
        recent[0]
            .values()
            .map(|v| v.tokens.total_tokens)
            .sum::<u64>(),
        30,
        "a valid nonzero recent period is independent of old ambiguity"
    );
    let unrelated = store
        .observe(
            Path::new("unrelated.jsonl"),
            &session("unrelated", 30),
            store.begin_scan().unwrap(),
        )
        .unwrap()
        .key;
    assert_eq!(
        store
            .range_totals_multi(std::slice::from_ref(&unrelated), &[(None, None)])
            .unwrap()[0][&unrelated]
            .tokens
            .total_tokens,
        45
    );
}

#[test]
fn integrity_alltime_summary_keeps_cumulative_totals_and_pricing_oracle() {
    let (_directory, store) = store();
    let mut fixture = session("cumulative", 10);
    fixture.tokens_total = totals(100);
    fixture
        .tokens_by_model
        .insert("gpt-test".into(), totals(100));
    let key = store
        .observe(
            Path::new("cumulative.jsonl"),
            &fixture,
            store.begin_scan().unwrap(),
        )
        .unwrap()
        .key;
    let rates = crate::rates::RateCard::default();
    let now = timestamp("2026-01-03T00:00:00Z");
    let prices = store
        .accounting_summary_prices(
            std::slice::from_ref(&key),
            std::slice::from_ref(&key),
            &rates,
            now,
        )
        .unwrap();
    let expected =
        crate::query::price_summary(&crate::model::SessionSummary::of(&fixture), &rates, now);
    assert_eq!(
        serde_json::to_value(&prices[&key]).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
    assert_eq!(prices[&key].tokens.total_tokens, 150);
    assert_eq!(
        store
            .range_totals_multi(std::slice::from_ref(&key), &[(None, None)])
            .unwrap()[0][&key]
            .tokens
            .total_tokens,
        15
    );
}

#[test]
fn integrity_alltime_no_history_preserves_model_bucket_fallback() {
    let (_directory, store) = store();
    let mut fixture = session("no-events", 10);
    fixture.tokens_history.clear();
    fixture
        .tokens_by_model
        .insert("gpt-test".into(), totals(10));
    let key = store
        .observe(
            Path::new("no-events.jsonl"),
            &fixture,
            store.begin_scan().unwrap(),
        )
        .unwrap()
        .key;
    let rates = crate::rates::RateCard::default();
    let now = timestamp("2026-01-03T00:00:00Z");
    let prices = store
        .accounting_summary_prices(
            std::slice::from_ref(&key),
            std::slice::from_ref(&key),
            &rates,
            now,
        )
        .unwrap();
    assert_eq!(
        serde_json::to_value(&prices[&key]).unwrap(),
        serde_json::to_value(crate::query::price_summary(
            &crate::model::SessionSummary::of(&fixture),
            &rates,
            now
        ))
        .unwrap()
    );
}

#[test]
fn integrity_exhaustion_missing_and_invalid_subset_are_explicit_unverified() {
    let (_directory, mut store, keys) = integrity_pair();
    let mut control = QueryControl::default();
    control.max_sessions = 1;
    store.query_control = Some(control);
    let error = store
        .range_totals_multi(&keys, &[(None, None)])
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Unverified)
    );
    store.query_control = None;
    for (fetch, scope) in [
        (vec![keys[0].clone()], vec![]),
        (vec![], vec!["codex:unknown".into()]),
    ] {
        let error = store
            .range_totals_multi_for_scope(&fetch, &scope, &[(None, None)])
            .unwrap_err();
        assert_eq!(
            error.downcast_ref::<AccountingIntegrityError>(),
            Some(&AccountingIntegrityError::Unverified)
        );
    }
}

#[test]
fn integrity_actual_range_read_keeps_proof_snapshot_across_writer_commit() {
    let (directory, store) = store();
    let fixture = session("snapshot", 10);
    let key = store
        .observe(
            Path::new("snapshot.jsonl"),
            &fixture,
            store.begin_scan().unwrap(),
        )
        .unwrap()
        .key;
    let path = directory.path().join("history.sqlite3");
    let writer_key = key.clone();
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        start_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut connection = Connection::open(path).unwrap();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        transaction
            .execute(
                "UPDATE rollup_token_totals SET total_tokens = 999 WHERE session_key = ?1",
                [&writer_key],
            )
            .unwrap();
        transaction
            .execute(
                "UPDATE durable_sessions SET ledger_dirty = 1 WHERE session_key = ?1",
                [&writer_key],
            )
            .unwrap();
        transaction.commit().unwrap();
        done_tx.send(()).unwrap();
    });
    // The actual range implementation pauses after proving scope; a second
    // connection commits changed numbers and authority before its numeric read.
    let maps = store
        .range_totals_multi_for_scope_impl(
            std::slice::from_ref(&key),
            std::slice::from_ref(&key),
            &[(None, None)],
            None,
            || {
                start_tx.send(()).unwrap();
                done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok(())
            },
        )
        .unwrap();
    writer.join().unwrap();
    assert_eq!(
        maps[0][&key].tokens.total_tokens, 15,
        "proof and facts must retain the same old snapshot"
    );
    assert_eq!(
        store
            .connection()
            .unwrap()
            .query_row(
                "SELECT total_tokens FROM rollup_token_totals WHERE session_key = ?1",
                [&key],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        999
    );
    let error = store
        .range_totals_multi(std::slice::from_ref(&key), &[(None, None)])
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Unverified)
    );
}

#[test]
fn integrity_shared_report_paths_withhold_legacy_duplicate_accounting() {
    let (_directory, store, keys) = integrity_pair();
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE durable_sessions SET lifecycle = 'superseded' WHERE session_key = ?1",
            [&keys[0]],
        )
        .unwrap();
    let rates = crate::rates::RateCard::default();
    let now = timestamp("2026-01-03T00:00:00Z");
    let provider = |_: &str| "codex".to_owned();
    assert!(crate::query::range_report(&store, &rates, provider, None, None, now).is_err());
    assert!(crate::query::project_report(&store, &rates, provider, None, None, now).is_err());
    assert!(crate::query::workflow_metrics(&store, &rates, provider, None, None, now).is_err());
    assert!(crate::query::session_report(&store, &rates, provider, None, None, None, now).is_err());
    assert!(crate::query::category_report(&store, &rates, None, None, now).is_err());
    assert!(store.token_totals_by_provider(None, None).is_err());
    assert!(store.activity_by_hour(None, None).is_err());
    assert!(store
        .accounting_summary_prices(&keys[..1], &keys, &rates, now)
        .is_err());
    assert_eq!(
        store.session_count().unwrap(),
        2,
        "no stored row is repaired or deleted"
    );
}

#[test]
fn integrity_missing_category_snapshot_and_repeated_fetch_fail_closed() {
    let (_directory, store, keys) = integrity_pair();
    let duplicated = vec![keys[0].clone(), keys[0].clone()];
    for result in [
        store
            .range_totals_multi_for_scope(&duplicated, &keys[..1], &[(None, None)])
            .map(|_| ()),
        store
            .accounting_summary_prices(
                &duplicated,
                &keys[..1],
                &crate::rates::RateCard::load_bundled().unwrap(),
                timestamp("2026-01-01T00:00:00Z"),
            )
            .map(|_| ()),
    ] {
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<AccountingIntegrityError>(),
            Some(&AccountingIntegrityError::Unverified)
        );
    }
    store
        .connection()
        .unwrap()
        .execute(
            "DELETE FROM session_snapshots WHERE session_key=?1",
            [&keys[0]],
        )
        .unwrap();
    let mut visited = 0;
    let error = store
        .stream_category_snapshots(|_| {
            visited += 1;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Unverified)
    );
    assert_eq!(visited, 0);
    assert_eq!(store.session_count().unwrap(), 2);
}

#[test]
fn integrity_positive_no_history_superseded_pair_is_unverified_only_for_cumulative() {
    let (_directory, store, keys) = integrity_pair();
    let connection = store.connection().unwrap();
    connection
        .execute(
            "DELETE FROM durable_token_events WHERE session_key IN (?1,?2)",
            params![keys[0], keys[1]],
        )
        .unwrap();
    connection
        .execute(
            "DELETE FROM rollup_token_totals WHERE session_key IN (?1,?2)",
            params![keys[0], keys[1]],
        )
        .unwrap();
    connection.execute("UPDATE session_snapshots SET session_json=json_set(session_json,'$.tokens_history',json('[]')) WHERE session_key IN (?1,?2)", params![keys[0],keys[1]]).unwrap();
    connection
        .execute(
            "UPDATE durable_sessions SET lifecycle='superseded' WHERE session_key=?1",
            [&keys[0]],
        )
        .unwrap();
    drop(connection);
    let rates = crate::rates::RateCard::load_bundled().unwrap();
    let now = timestamp("2026-01-01T00:00:00Z");
    let error = store
        .accounting_summary_prices(&keys, &keys, &rates, now)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Unverified)
    );
    assert!(store
        .accounting_summary_prices(&keys[..1], &keys[..1], &rates, now)
        .is_ok());
    assert!(store.range_totals_multi(&keys, &[(None, None)]).unwrap()[0].is_empty());
}

#[test]
fn integrity_shared_numeric_deadline_is_not_restarted_after_proof() {
    let (_directory, store, keys) = integrity_pair();
    let control = QueryControl::with_timeout(Duration::from_millis(20));
    let error = store
        .range_totals_multi_for_scope_impl(&keys, &keys, &[(None, None)], Some(&control), || {
            std::thread::sleep(Duration::from_millis(30));
            Ok(())
        })
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Unverified)
    );
    assert!(store.range_totals_multi(&keys, &[(None, None)]).is_ok());
}

#[test]
fn integrity_oversized_snapshot_rejected_before_json_projection() {
    let (_directory, store, keys) = integrity_pair();
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE session_snapshots SET session_json=zeroblob(67108865) WHERE session_key=?1",
            [&keys[0]],
        )
        .unwrap();
    let error = store
        .accounting_summary_prices(
            &keys,
            &keys,
            &crate::rates::RateCard::load_bundled().unwrap(),
            timestamp("2026-01-01T00:00:00Z"),
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Unverified)
    );
    let error = store.stream_category_snapshots(|_| Ok(())).unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Unverified)
    );
}
fn integrity_export_request(
    keys: Vec<String>,
    format: &str,
) -> accounting_export::SessionSummaryExportRequest {
    accounting_export::SessionSummaryExportRequest {
        session_ids: keys,
        from: None,
        to: None,
        format: format.into(),
        include_working_directory: false,
    }
}
#[test]
fn integrity_export_unchanged_accepts_canonical_rates_and_ignores_caller_bytes() {
    let (_directory, store, keys) = integrity_pair();
    let mut rates = crate::rates::RateCard::load_bundled().unwrap();
    let now = timestamp("2026-01-01T12:00:00Z");
    for format in ["json", "csv"] {
        let mut prepared = store
            .prepare_session_summary_export(
                integrity_export_request(keys.clone(), format),
                &rates,
                now,
                0,
            )
            .unwrap();
        let expected = prepared.content.clone();
        if format == "json" {
            let rows: serde_json::Value = serde_json::from_str(&expected).unwrap();
            assert_eq!(rows[0]["total_tokens"], 15);
            assert!(rows[0].get("working_directory").is_none());
            assert!(rows[0].get("first_user_message").is_none());
        }
        // Reinsert map entries in a different order; canonical full-card
        // hashing must not reject an unchanged card.
        let mut entries: Vec<_> = rates.models.drain().collect();
        entries.reverse();
        rates.models.extend(entries);
        prepared.content = "untrusted replacement bytes".into();
        let mut published = None;
        store
            .publish_session_summary_export(&prepared, &rates, now, 0, |content| {
                published = Some(content.to_owned());
                Ok(())
            })
            .unwrap();
        assert_eq!(published.as_deref(), Some(expected.as_str()));
    }
}
#[test]
fn integrity_export_pending_picker_rejects_purge_replacement_rate_and_expiry() {
    let (directory, store) = store();
    let source = directory.path().join("export.jsonl");
    let fixture = session("reviewed-export", 10);
    let key = store.observe(&source, &fixture, 1).unwrap().key;
    let rates = crate::rates::RateCard::load_bundled().unwrap();
    let now = timestamp("2026-10-04T12:00:00Z");
    let prepared = store
        .prepare_session_summary_export(
            integrity_export_request(vec![key.clone()], "json"),
            &rates,
            now,
            0,
        )
        .unwrap();
    let mut changed = rates.clone();
    changed.version += 1;
    let mut calls = 0;
    assert!(store
        .publish_session_summary_export(&prepared, &changed, now, 0, |_| {
            calls += 1;
            Ok(())
        })
        .unwrap_err()
        .to_string()
        .starts_with("accounting_export_changed:"));
    assert!(store
        .publish_session_summary_export(
            &prepared,
            &rates,
            now + chrono::Duration::minutes(16),
            0,
            |_| {
                calls += 1;
                Ok(())
            }
        )
        .is_err());
    // An identical saved-card replacement still invalidates reviewed prices,
    // even when the card version and every serialized rate remain unchanged.
    assert!(store
        .publish_session_summary_export(&prepared, &rates, now, 1, |_| {
            calls += 1;
            Ok(())
        })
        .unwrap_err()
        .to_string()
        .starts_with("accounting_export_changed:"));
    // Source location change, even with unchanged numeric totals, revokes
    // captured bytes while a native picker may still be open.
    store.mark_path_missing(&source).unwrap();
    assert!(store
        .publish_session_summary_export(&prepared, &rates, now, 0, |_| {
            calls += 1;
            Ok(())
        })
        .unwrap_err()
        .to_string()
        .starts_with("accounting_export_changed:"));
    let retained = store
        .prepare_session_summary_export(integrity_export_request(vec![key], "json"), &rates, now, 0)
        .unwrap();
    store
        .set_retention_policy(&lifecycle::RetentionPolicy {
            retained_days: Some(1),
        })
        .unwrap();
    store
        .purge_retained(&store.preview_purge(now).unwrap(), now)
        .unwrap();
    assert!(store
        .publish_session_summary_export(&retained, &rates, now, 0, |_| {
            calls += 1;
            Ok(())
        })
        .is_err());
    assert_eq!(
        calls, 0,
        "obsolete bytes must never reach atomic publication"
    );
}
#[test]
fn integrity_export_publication_holds_writer_protection_through_actual_publish() {
    let (directory, store, keys) = integrity_pair();
    let rates = crate::rates::RateCard::load_bundled().unwrap();
    let now = timestamp("2026-01-01T12:00:00Z");
    let prepared = store
        .prepare_session_summary_export(
            integrity_export_request(keys.clone(), "json"),
            &rates,
            now,
            0,
        )
        .unwrap();
    let path = directory.path().join("history.sqlite3");
    let (start_tx, start_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let key = keys[0].clone();
    let writer = std::thread::spawn(move || {
        let connection = Connection::open(path).unwrap();
        connection.busy_timeout(Duration::from_millis(30)).unwrap();
        start_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let result = connection.execute(
            "UPDATE durable_sessions SET ledger_dirty=1 WHERE session_key=?1",
            [key],
        );
        done_tx.send(result.is_err()).unwrap();
    });
    store
        .publish_session_summary_export(&prepared, &rates, now, 0, |content| {
            start_tx.send(()).unwrap();
            assert!(
                done_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                "concurrent purge/mutation cannot acquire writer lock during publish"
            );
            assert_eq!(content, prepared.content);
            Ok(())
        })
        .unwrap();
    writer.join().unwrap();
    assert!(store.range_totals_multi(&keys, &[(None, None)]).is_ok());
}

#[test]
fn integrity_tool_and_correlation_accounting_bypasses_reject_legacy_pair() {
    let (_directory, store, keys) = integrity_pair();
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE durable_sessions SET lifecycle='superseded' WHERE session_key=?1",
            [&keys[0]],
        )
        .unwrap();
    let error = store
        .accounting_tool_comparison(
            &keys,
            crate::tool_impact::ToolImpactTargetKind::Tool,
            "shell",
            None,
            None,
        )
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Ambiguous)
    );
    let query = crate::correlation::CorrelationQuery {
        events: vec![crate::correlation::ExternalEvent {
            id: "change".into(),
            timestamp: timestamp("2026-01-01T00:00:00Z"),
            scope: None,
            source: "synthetic".into(),
            kind: "config".into(),
            metadata: BTreeMap::new(),
        }],
        before_days: 1,
        after_days: 1,
        exclude_confounded: false,
        include_subagents: true,
    };
    let error = store
        .accounting_correlation(&keys, query.clone())
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Ambiguous)
    );
    let mut later = query;
    later.events[0].timestamp = timestamp("2026-01-04T00:00:00Z");
    assert!(
        store.accounting_correlation(&keys, later).is_ok(),
        "an unaffected event window remains available"
    );
}

#[test]
fn integrity_activity_export_preserves_empty_scope_and_rejects_changed_facts_or_coverage() {
    let (_directory, store, keys) = integrity_pair();
    let windows = [(
        Some(timestamp("2026-01-01T00:00:00Z")),
        Some(timestamp("2026-01-01T23:59:59Z")),
    )];
    let facts = store.range_totals_multi(&keys, &windows).unwrap();
    let mut request = accounting_export::ActivitySummaryExportRequest {
        session_ids: keys.clone(),
        days: vec![accounting_export::ActivitySummaryDay {
            from: windows[0].0.unwrap(),
            to: windows[0].1.unwrap(),
            tokens: 30,
            tool_calls: facts[0]
                .values()
                .map(|range| range.tool_metrics.calls)
                .sum(),
        }],
        coverage_complete: true,
        svg: "<svg xmlns=\"http://www.w3.org/2000/svg\"><text>30 tokens</text></svg>".into(),
    };
    let mut content = None;
    store
        .publish_activity_summary_export(&request, true, |svg| {
            content = Some(svg.to_owned());
            Ok(())
        })
        .unwrap();
    assert_eq!(content.as_deref(), Some(request.svg.as_str()));
    request.days[0].tokens += 1;
    assert!(store
        .publish_activity_summary_export(&request, true, |_| panic!("changed bytes cannot publish"))
        .unwrap_err()
        .to_string()
        .starts_with("accounting_export_changed:"));
    request.session_ids.clear();
    request.svg = "<svg xmlns=\"http://www.w3.org/2000/svg\"><text>0 tokens</text></svg>".into();
    request.days[0].tokens = 0;
    request.days[0].tool_calls = 0;
    store
        .publish_activity_summary_export(&request, true, |_| Ok(()))
        .unwrap();
    request.coverage_complete = false;
    assert!(store
        .publish_activity_summary_export(&request, true, |_| panic!(
            "wrong coverage cannot publish"
        ))
        .is_err());
    store
        .publish_activity_summary_export(&request, false, |_| Ok(()))
        .unwrap();
    request.session_ids = keys;
    request.days[0].tokens = 30;
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE durable_sessions SET lifecycle='superseded' WHERE session_key=?1",
            [&request.session_ids[0]],
        )
        .unwrap();
    let error = store
        .publish_activity_summary_export(&request, false, |_| {
            panic!("ambiguous bytes cannot publish")
        })
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<AccountingIntegrityError>(),
        Some(&AccountingIntegrityError::Ambiguous)
    );
}
#[test]
fn integrity_dimension_export_rechecks_context_tokens_before_fixed_serialization() {
    let (directory, store) = store();
    let key = store
        .observe(Path::new("dimensions.jsonl"), &session("dimensions", 10), 1)
        .unwrap()
        .key;
    let facts = store
        .range_totals_multi(std::slice::from_ref(&key), &[(None, None)])
        .unwrap();
    let rows: Vec<_> = facts[0][&key]
        .tool_dimensions
        .iter()
        .flat_map(|(kind, values)| {
            values.iter().map(
                move |(value, metric)| accounting_export::ToolDimensionExportRow {
                    dimension_kind: kind.clone(),
                    dimension_value: value.clone(),
                    calls: metric.calls,
                    failures: metric.failures,
                    output_bytes: metric.output_bytes,
                    duration_ms: metric.duration_ms,
                    tokens: metric.tokens,
                },
            )
        })
        .collect();
    assert!(rows
        .iter()
        .any(|row| row.dimension_kind == "context_source" && row.tokens > 0));
    let mut request = accounting_export::ToolDimensionExportRequest {
        session_ids: vec![key.clone()],
        from: None,
        to: None,
        format: "json".into(),
        rows,
    };
    for format in ["json", "csv"] {
        request.format = format.into();
        store.publish_tool_dimension_export(&request,|content|{
            if format=="json" {let serialized:Vec<accounting_export::ToolDimensionExportRow>=serde_json::from_str(content)?;assert_eq!(serialized,request.rows);} else {assert!(content.starts_with("dimension_kind,dimension_value,calls,failures,output_bytes,duration_ms,tokens\r\n"));}
            Ok(())
        }).unwrap();
    }
    // The token dimension is derived from facts, not a tool-observation body.
    store
        .connection()
        .unwrap()
        .execute(
            "UPDATE rollup_token_totals SET input_tokens=999 WHERE session_key=?1",
            [&key],
        )
        .unwrap();
    assert!(store
        .publish_tool_dimension_export(&request, |_| panic!(
            "changed context tokens cannot publish"
        ))
        .unwrap_err()
        .to_string()
        .starts_with("accounting_export_changed:"));
    // Real purge cascades all relevant fact domains; obsolete requested IDs
    // then fail closed, while an explicit empty selected scope is valid.
    store
        .mark_path_missing(Path::new("dimensions.jsonl"))
        .unwrap();
    let now = timestamp("2026-10-04T12:00:00Z");
    store
        .set_retention_policy(&lifecycle::RetentionPolicy {
            retained_days: Some(1),
        })
        .unwrap();
    store
        .purge_retained(&store.preview_purge(now).unwrap(), now)
        .unwrap();
    assert!(store
        .publish_tool_dimension_export(&request, |_| panic!("purged context tokens cannot publish"))
        .is_err());
    request.session_ids.clear();
    request.rows.clear();
    store
        .publish_tool_dimension_export(&request, |content| {
            assert_eq!(content, "");
            Ok(())
        })
        .unwrap();
    assert!(directory.path().join("history.sqlite3").is_file());
}
#[test]
fn integrity_publication_cleanup_preserves_later_writes_on_success_and_error() {
    let (_directory, store) = store();
    for fail in [false, true] {
        let result = store.with_accounting_publication(1, |_, control| {
            control.cancel();
            if fail {
                bail!("synthetic publication refusal");
            } else {
                Ok(())
            }
        });
        assert_eq!(result.is_err(), fail);
        let conn = store.connection().unwrap();
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000);
        let count:i64=conn.query_row("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<5000) SELECT SUM(x) FROM n",[],|row|row.get(0)).unwrap();
        assert_eq!(
            count, 12_502_500,
            "an expired/cancelled request must not poison the persistent writer"
        );
    }
}
