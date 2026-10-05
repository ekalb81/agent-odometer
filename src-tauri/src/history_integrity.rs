//! Read-only accounting authority. A proof and its facts share one SQLite snapshot.
use super::*;
use crate::model::{CategoryMetric, SessionSummary, TaskCategory};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountingIntegrityError {
    Ambiguous,
    Unverified,
}

impl std::fmt::Display for AccountingIntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Ambiguous => "accounting_identity_ambiguous: historical usage identities overlap; aggregate unavailable",
            Self::Unverified => "accounting_identity_unverified: historical accounting proof is unfinished; aggregate unavailable",
        })
    }
}
impl std::error::Error for AccountingIntegrityError {}
impl AccountingIntegrityError {
    pub(super) fn from_query(error: anyhow::Error) -> anyhow::Error {
        if error.downcast_ref::<Self>().is_some() {
            error
        } else {
            Self::Unverified.into()
        }
    }
}

#[derive(serde::Deserialize)]
pub(crate) struct AccountingSummary {
    pub tokens_total: TokenTotals,
    pub tokens_by_model: HashMap<String, TokenTotals>,
    #[serde(default)]
    pub category_totals: BTreeMap<TaskCategory, CategoryMetric>,
}

impl HistoryStore {
    pub(super) fn accounting_reader(&self) -> Result<(QueryReader<'_>, QueryControl)> {
        self.accounting_reader_with_control(None)
    }

    pub(super) fn accounting_reader_with_control(
        &self,
        provided: Option<&QueryControl>,
    ) -> Result<(QueryReader<'_>, QueryControl)> {
        let control = provided
            .cloned()
            .or_else(|| self.query_control.clone())
            .unwrap_or_else(|| {
                let mut control = QueryControl::default();
                control.max_windows = 64;
                control
            });
        let connection = self
            .open_reader()
            .map_err(|_| AccountingIntegrityError::Unverified)?;
        // open_reader already starts a deferred transaction; headless stores
        // reuse their request's transaction. Never begin a second snapshot.
        connection.busy_timeout(control.remaining().min(Duration::from_millis(50)))?;
        let progress = control.clone();
        connection.progress_handler(1000, Some(move || progress.check().is_err()))?;
        Ok((connection, control))
    }

    pub(super) fn accounting_keys_on(
        &self,
        connection: &Connection,
        control: &QueryControl,
    ) -> Result<Vec<String>> {
        let mut statement = connection
            .prepare("SELECT session_key FROM durable_sessions ORDER BY session_key LIMIT ?1")?;
        let keys = statement
            .query_map([control.max_sessions.saturating_add(1) as i64], |row| {
                row.get(0)
            })?
            .collect_bounded(Some(control))?;
        if keys.len() > control.max_sessions {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        Ok(keys)
    }

    pub(super) fn prove_accounting_on(
        &self,
        connection: &Connection,
        selected_keys: &[String],
        windows: &[RangeWindow],
        control: &QueryControl,
    ) -> Result<()> {
        let prove = || -> Result<()> {
            control.check()?;
            if selected_keys.len() > control.max_sessions || windows.len() > control.max_windows {
                bail!("accounting scope limit exceeded");
            }
            let unique: std::collections::HashSet<_> = selected_keys.iter().collect();
            if unique.len() != selected_keys.len() {
                bail!("duplicate accounting scope key");
            }
            let json = serde_json::to_string(selected_keys)?;
            if json.len() > 8 * 1024 * 1024 {
                bail!("accounting scope size exceeded");
            }
            let unverified: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM json_each(?1) selected
                 LEFT JOIN durable_sessions d ON d.session_key = selected.value
                 WHERE d.session_key IS NULL OR d.ledger_dirty = 1)",
                [json.as_str()],
                |row| row.get(0),
            )?;
            if unverified || rollups_are_stale(connection)? {
                bail!("unreconciled accounting scope");
            }
            for (from, to) in windows {
                if self.has_ambiguous_accounting_identity_on(
                    connection,
                    selected_keys,
                    from.map_or(i64::MIN, |at| at.timestamp_millis()),
                    to.map_or(i64::MAX, |at| at.timestamp_millis()),
                    control,
                )? {
                    return Err(AccountingIntegrityError::Ambiguous.into());
                }
            }
            Ok(())
        };
        prove().map_err(|error| {
            if error.downcast_ref::<AccountingIntegrityError>().is_some() {
                error
            } else {
                AccountingIntegrityError::Unverified.into()
            }
        })
    }

    pub(super) fn bounded_snapshots_on(
        &self,
        connection: &Connection,
        keys: &[String],
    ) -> Result<()> {
        let bad: bool=connection.query_row("SELECT EXISTS(SELECT 1 FROM json_each(?1) selected LEFT JOIN durable_sessions d ON d.session_key=selected.value LEFT JOIN session_snapshots s ON s.session_key=d.session_key AND s.version=d.current_snapshot_version WHERE s.session_key IS NULL OR length(CAST(s.session_json AS BLOB))>67108864)", [serde_json::to_string(keys)?], |row| row.get(0)).map_err(|_| AccountingIntegrityError::Unverified)?;
        if bad {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        Ok(())
    }

    pub(super) fn prove_cumulative_on(
        &self,
        connection: &Connection,
        keys: &[String],
        control: &QueryControl,
    ) -> Result<()> {
        control
            .check()
            .map_err(|_| AccountingIntegrityError::Unverified)?;
        self.bounded_snapshots_on(connection, keys)?;
        // Positive cumulative totals without any event history cannot prove
        // independence from a selected superseded sibling. Present/present
        // collisions and inspection of one retained source remain available.
        let json = serde_json::to_string(keys)?;
        let uncertain: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM durable_sessions a JOIN durable_sessions b
               ON b.identity_key=a.identity_key AND b.session_key>a.session_key
             LEFT JOIN session_snapshots sa ON sa.session_key=a.session_key AND sa.version=a.current_snapshot_version
             LEFT JOIN session_snapshots sb ON sb.session_key=b.session_key AND sb.version=b.current_snapshot_version
             WHERE a.session_key IN (SELECT value FROM json_each(?1))
               AND b.session_key IN (SELECT value FROM json_each(?1))
               AND a.collision=1 AND b.collision=1
               AND (a.lifecycle='superseded' OR b.lifecycle='superseded')
               AND (sa.session_key IS NULL OR sb.session_key IS NULL OR
                  (COALESCE(json_extract(sa.session_json,'$.tokens_total.total_tokens'),0)>0
                   AND NOT EXISTS(SELECT 1 FROM durable_token_events WHERE session_key=a.session_key)) OR
                  (COALESCE(json_extract(sb.session_json,'$.tokens_total.total_tokens'),0)>0
                   AND NOT EXISTS(SELECT 1 FROM durable_token_events WHERE session_key=b.session_key))))",
            [json], |row| row.get(0),
        ).map_err(|_| AccountingIntegrityError::Unverified)?;
        if uncertain {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        Ok(())
    }

    pub(crate) fn accounting_summary_prices(
        &self,
        fetched_keys: &[String],
        aggregate_keys: &[String],
        rates: &crate::rates::RateCard,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<HashMap<String, crate::query::SummaryPricing>> {
        let (connection, control) = self.accounting_reader()?;
        self.accounting_summary_prices_on(
            &connection,
            &control,
            fetched_keys,
            aggregate_keys,
            rates,
            now,
        )
        .map_err(AccountingIntegrityError::from_query)
    }

    pub(super) fn accounting_summary_prices_on(
        &self,
        connection: &Connection,
        control: &QueryControl,
        fetched_keys: &[String],
        aggregate_keys: &[String],
        rates: &crate::rates::RateCard,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<HashMap<String, crate::query::SummaryPricing>> {
        self.prove_accounting_on(connection, aggregate_keys, &[(None, None)], control)?;
        self.prove_cumulative_on(connection, aggregate_keys, control)?;
        let scope: std::collections::HashSet<_> = aggregate_keys.iter().collect();
        let fetched: std::collections::HashSet<_> = fetched_keys.iter().collect();
        if fetched.len() != fetched_keys.len()
            || fetched_keys.len() > control.max_sessions
            || fetched_keys.iter().any(|key| !scope.contains(key))
        {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        // SQLite projects only accounting fields. Do not deserialize prompts,
        // turns, histories, or a full Session just to price a summary.
        let ranges = self
            .range_totals_multi_on(connection, control, fetched_keys, &[(None, None)])?
            .pop()
            .unwrap_or_default();
        let mut statement = connection.prepare(
            "SELECT json_object('tokens_total', json_extract(s.session_json, '$.tokens_total'),
                     'tokens_by_model', json_extract(s.session_json, '$.tokens_by_model'),
                     'category_totals', json(COALESCE(json_extract(s.session_json, '$.category_totals'), '{}'))),
                    json_array_length(s.session_json, '$.tokens_history')
             FROM durable_sessions d JOIN session_snapshots s
               ON s.session_key = d.session_key AND s.version = d.current_snapshot_version
             WHERE d.session_key = ?1",
        )?;
        let mut out = HashMap::new();
        let mut bytes = 0usize;
        for key in fetched_keys {
            control.consume_row()?;
            let (raw, history_count): (String, i64) = statement
                .query_row([key], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|_| AccountingIntegrityError::Unverified)?;
            bytes = bytes.saturating_add(raw.len());
            if bytes > control.max_output_bytes {
                return Err(AccountingIntegrityError::Unverified.into());
            }
            let fields: AccountingSummary =
                serde_json::from_str(&raw).map_err(|_| AccountingIntegrityError::Unverified)?;
            let harness =
                crate::query::provider_for_key(key).ok_or(AccountingIntegrityError::Unverified)?;
            let buckets = if history_count == 0 {
                let mut buckets: Vec<_> = fields
                    .tokens_by_model
                    .into_iter()
                    .map(|(model, tokens)| TierBucket {
                        model,
                        service_tier: None,
                        tokens,
                    })
                    .collect();
                buckets.sort_by(|a, b| a.model.cmp(&b.model));
                buckets
            } else {
                ranges
                    .get(key)
                    .map(|range| range.buckets.clone())
                    .unwrap_or_default()
            };
            out.insert(
                key.clone(),
                crate::query::SummaryPricing {
                    tokens: fields.tokens_total,
                    pricing: crate::query::price_surfaces(&buckets, harness.as_str(), rates, now),
                    categories: fields
                        .category_totals
                        .clone()
                        .into_iter()
                        .map(|(category, metric)| {
                            (
                                serde_json::to_value(category)
                                    .expect("category serializes")
                                    .as_str()
                                    .expect("category string")
                                    .to_owned(),
                                crate::query::price_surfaces(
                                    &metric.buckets,
                                    harness.as_str(),
                                    rates,
                                    now,
                                ),
                            )
                        })
                        .collect(),
                    category_totals: fields.category_totals,
                },
            );
        }
        control
            .check()
            .map_err(|_| AccountingIntegrityError::Unverified)?;
        Ok(out)
    }
    fn accounting_sessions_on(
        &self,
        conn: &Connection,
        keys: &[String],
        control: &QueryControl,
    ) -> Result<Vec<std::sync::Arc<Session>>> {
        if keys.len() > control.max_sessions
            || keys.iter().collect::<std::collections::HashSet<_>>().len() != keys.len()
        {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        self.bounded_snapshots_on(conn, keys)?;
        let mut sessions = Vec::new();
        for key in keys {
            control
                .consume_row()
                .map_err(|_| AccountingIntegrityError::Unverified)?;
            let bytes:i64=conn.query_row("SELECT length(CAST(s.session_json AS BLOB)) FROM durable_sessions d JOIN session_snapshots s ON s.session_key=d.session_key AND s.version=d.current_snapshot_version WHERE d.session_key=?1",[key],|row|row.get(0)).map_err(|_| AccountingIntegrityError::Unverified)?;
            control
                .consume_snapshot_bytes(bytes.try_into()?)
                .map_err(|_| AccountingIntegrityError::Unverified)?;
            sessions.push(std::sync::Arc::new(
                load_one_controlled(conn, key, Some(control))
                    .map_err(|_| AccountingIntegrityError::Unverified)?
                    .session,
            ));
        }
        Ok(sessions)
    }
    pub(crate) fn accounting_tool_comparison(
        &self,
        keys: &[String],
        kind: crate::tool_impact::ToolImpactTargetKind,
        target: &str,
        from: Option<chrono::DateTime<chrono::Utc>>,
        to: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<crate::tool_impact::ToolImpactResult> {
        let (conn, control) = self.accounting_reader()?;
        // Tool cohorts count complete overlapping turn usage, so authority
        // covers the contributing sessions' cumulative accounting lifetime.
        self.prove_accounting_on(&conn, keys, &[(None, None)], &control)?;
        self.prove_cumulative_on(&conn, keys, &control)?;
        let sessions = self.accounting_sessions_on(&conn, keys, &control)?;
        let result = crate::tool_impact::compare(&sessions, kind, target, from, to);
        control
            .check()
            .map_err(|_| AccountingIntegrityError::Unverified)?;
        Ok(result)
    }
    pub(crate) fn accounting_correlation(
        &self,
        keys: &[String],
        query: crate::correlation::CorrelationQuery,
    ) -> Result<crate::correlation::CorrelationResult> {
        if query.events.len() > 2000
            || !(-365..=365).contains(&query.before_days)
            || !(-365..=365).contains(&query.after_days)
        {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        let (conn, mut control) = self.accounting_reader()?;
        control.max_windows = 4000;
        let sessions = self.accounting_sessions_on(&conn, keys, &control)?;
        let summaries: Vec<_> = sessions
            .iter()
            .map(|session| SessionSummary::of(session))
            .collect();
        let windows = crate::correlation::event_windows(&query);
        let (_, excluded) = crate::correlation::event_exclusions(&query, &windows);
        for (index, event) in query.events.iter().enumerate() {
            if excluded[index] {
                continue;
            }
            let selected = crate::correlation::candidate_session_keys(
                summaries
                    .iter()
                    .map(|summary| (summary.storage_id.as_str(), summary)),
                &crate::correlation::CorrelationQuery {
                    events: vec![event.clone()],
                    exclude_confounded: false,
                    ..query.clone()
                },
            );
            self.prove_accounting_on(
                &conn,
                &selected,
                &[windows[index].0, windows[index].1],
                &control,
            )?;
        }
        let result = crate::correlation::correlate(&sessions, query);
        control
            .check()
            .map_err(|_| AccountingIntegrityError::Unverified)?;
        Ok(result)
    }
}
