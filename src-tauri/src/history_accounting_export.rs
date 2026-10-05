//! Reviewed summary exports use one proof/projection snapshot and guarded publication.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummaryExportRequest {
    pub session_ids: Vec<String>,
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub format: String,
    #[serde(default)]
    pub include_working_directory: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedSessionSummaryExport {
    pub request: SessionSummaryExportRequest,
    pub as_of: chrono::DateTime<chrono::Utc>,
    pub digest: String,
    pub content: String,
    pub session_count: usize,
}
fn changed() -> anyhow::Error {
    anyhow!("accounting_export_changed: accounting authority changed; reload before exporting")
}
fn validate(request: &SessionSummaryExportRequest, control: &QueryControl) -> Result<()> {
    let unique: std::collections::HashSet<_> = request.session_ids.iter().collect();
    if !matches!(request.format.as_str(), "csv" | "json")
        || request.session_ids.is_empty()
        || request.session_ids.len() > control.max_sessions
        || unique.len() != request.session_ids.len()
        || request
            .session_ids
            .iter()
            .any(|id| id.is_empty() || id.len() > 1024)
        || matches!((request.from, request.to),(Some(a),Some(b)) if a>b)
        || serde_json::to_vec(request)?.len() > control.max_output_bytes
    {
        return Err(AccountingIntegrityError::Unverified.into());
    }
    Ok(())
}
fn cell(value: &Value) -> String {
    let text = match value {
        Value::Null => String::new(),
        Value::String(v) => v.clone(),
        other => other.to_string(),
    };
    if text.contains([',', '"', '\r', '\n']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}
fn canonical(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> = map
                .into_iter()
                .map(|(key, value)| (key, canonical(value)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
        other => other,
    }
}
fn available(surface: &crate::query::PricedSurface, require_direct: bool) -> Value {
    if !surface.total.is_finite()
        || !surface.unpriced_models.is_empty()
        || (require_direct && !surface.missing_models.is_empty())
    {
        Value::Null
    } else {
        json!(surface.total)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivitySummaryDay {
    pub from: chrono::DateTime<chrono::Utc>,
    pub to: chrono::DateTime<chrono::Utc>,
    pub tokens: u64,
    pub tool_calls: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivitySummaryExportRequest {
    pub session_ids: Vec<String>,
    pub days: Vec<ActivitySummaryDay>,
    pub coverage_complete: bool,
    pub svg: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDimensionExportRow {
    pub dimension_kind: String,
    pub dimension_value: String,
    pub calls: u64,
    pub failures: u64,
    pub output_bytes: u64,
    pub duration_ms: u64,
    pub tokens: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDimensionExportRequest {
    pub session_ids: Vec<String>,
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    pub format: String,
    pub rows: Vec<ToolDimensionExportRow>,
}
fn validate_scope(keys: &[String], control: &QueryControl) -> Result<()> {
    if keys.len() > control.max_sessions
        || keys.iter().collect::<std::collections::HashSet<_>>().len() != keys.len()
        || keys.iter().any(|key| key.is_empty() || key.len() > 1024)
        || serde_json::to_vec(keys)?.len() > control.max_output_bytes
    {
        return Err(AccountingIntegrityError::Unverified.into());
    }
    Ok(())
}
impl HistoryStore {
    pub(crate) fn prepare_session_summary_export(
        &self,
        request: SessionSummaryExportRequest,
        rates: &crate::rates::RateCard,
        as_of: chrono::DateTime<chrono::Utc>,
        rate_revision: u64,
    ) -> Result<PreparedSessionSummaryExport> {
        let (conn, control) = self.accounting_reader()?;
        self.summary_export_on(&conn, &control, request, rates, as_of, rate_revision)
    }
    fn summary_export_on(
        &self,
        conn: &Connection,
        control: &QueryControl,
        request: SessionSummaryExportRequest,
        rates: &crate::rates::RateCard,
        as_of: chrono::DateTime<chrono::Utc>,
        rate_revision: u64,
    ) -> Result<PreparedSessionSummaryExport> {
        validate(&request, control)?;
        let keys = &request.session_ids;
        self.bounded_snapshots_on(conn, keys)?;
        let date_scoped = request.from.is_some() || request.to.is_some();
        let summaries = if date_scoped {
            HashMap::new()
        } else {
            self.accounting_summary_prices_on(conn, control, keys, keys, rates, as_of)?
        };
        let ranges = if date_scoped {
            self.prove_accounting_on(conn, keys, &[(request.from, request.to)], control)?;
            self.range_totals_multi_on(conn, control, keys, &[(request.from, request.to)])?
                .pop()
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        let mut statement=conn.prepare("SELECT json_object(
          'id',json_extract(s.session_json,'$.id'),
          'name',CASE WHEN d.thread_name_overlay_set=1 THEN d.thread_name_overlay ELSE json_extract(s.session_json,'$.thread_name') END,
          'started_at',json_extract(s.session_json,'$.started_at'),
          'last_event_at',json_extract(s.session_json,'$.last_event_at'),
          'archived',json_extract(s.session_json,'$.archived'),
          'parent_thread_id',json_extract(s.session_json,'$.parent_thread_id'),
          'agent_path',json_extract(s.session_json,'$.agent_path'),
          'source',json_extract(s.session_json,'$.source'),
          'model',json_extract(s.session_json,'$.model'),
          'turns',json_extract(s.session_json,'$.total_turns'),
          'working_directory',CASE WHEN ?2 THEN json_extract(s.session_json,'$.working_directory') ELSE NULL END),
          d.lifecycle, EXISTS(SELECT 1 FROM source_locations WHERE session_key=d.session_key AND present=1),
          d.current_snapshot_version,d.current_snapshot_hash,d.first_event_fingerprint,
          COALESCE((SELECT json_group_array(json_array(path,artifact_key,present,last_seen_at_ms)) FROM (SELECT * FROM source_locations WHERE session_key=d.session_key ORDER BY path)), '[]')
          FROM durable_sessions d JOIN session_snapshots s ON s.session_key=d.session_key AND s.version=d.current_snapshot_version WHERE d.session_key=?1")?;
        let mut rows = Vec::<Value>::new();
        let mut authority = Vec::new();
        let mut bytes = 0usize;
        for key in keys {
            control.consume_row()?;
            let (raw, lifecycle, present, version, hash, fingerprint, locations): (
                String,
                String,
                bool,
                i64,
                Option<String>,
                String,
                String,
            ) = statement
                .query_row(params![key, request.include_working_directory], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                })
                .map_err(|_| AccountingIntegrityError::Unverified)?;
            bytes = bytes
                .saturating_add(raw.len())
                .saturating_add(locations.len());
            if bytes > control.max_output_bytes {
                return Err(AccountingIntegrityError::Unverified.into());
            }
            let meta: Value =
                serde_json::from_str(&raw).map_err(|_| AccountingIntegrityError::Unverified)?;
            let harness =
                crate::query::provider_for_key(key).ok_or(AccountingIntegrityError::Unverified)?;
            let provider = harness.as_str();
            let empty = TokenTotals::default();
            let (tokens, pricing) = if date_scoped {
                let range = ranges.get(key);
                let buckets = range.map(|v| v.buckets.as_slice()).unwrap_or_default();
                (
                    range.map(|v| &v.tokens).unwrap_or(&empty),
                    crate::query::price_surfaces(buckets, provider, rates, as_of),
                )
            } else {
                let summary = summaries
                    .get(key)
                    .ok_or(AccountingIntegrityError::Unverified)?;
                (&summary.tokens, summary.pricing.clone())
            };
            let codex = provider == "codex";
            let current = pricing.current.as_ref();
            let plan = current
                .map(|p| &p.purchased_credits)
                .unwrap_or(&pricing.plan);
            let api = current.map(|p| &p.api_estimate).or(pricing.api.as_ref());
            let use_api = codex && !rates.api_models.is_empty();
            let displayed = if use_api { api.unwrap_or(plan) } else { plan };
            let catalog_available = !rates.pricing_catalog.rate_periods.is_empty();
            let surface = if codex {
                "openai_api_usd"
            } else if provider == "gemini_cli" {
                "gemini_api_usd"
            } else {
                "anthropic_api_usd"
            };
            let dated = rates.pricing_catalog.rate_periods.iter().any(|period| {
                serde_json::to_value(period.surface)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .as_deref()
                    == Some(surface)
            });
            let time_status = if dated {
                json!("unavailable_requires_request_history")
            } else {
                Value::Null
            };
            let id = meta["id"]
                .as_str()
                .ok_or(AccountingIntegrityError::Unverified)?;
            let name = meta["name"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| id.chars().take(8).collect());
            let subagent = meta["parent_thread_id"]
                .as_str()
                .is_some_and(|v| !v.is_empty())
                || meta["agent_path"].as_str().is_some_and(|v| !v.is_empty())
                || meta["source"] == "subagent";
            let mut row = json!({
                "id":id,"storage_id":key,"harness":provider,"name":name,
                "started_at":meta["started_at"],"last_event_at":meta["last_event_at"],
                "archived":meta["archived"].as_bool().or_else(||meta["archived"].as_i64().map(|v|v!=0)).unwrap_or(false),
                "source_availability":if present {"present"} else {"missing"},"lifecycle":lifecycle,
                "subagent":subagent,"parent_thread_id":meta["parent_thread_id"],"model":meta["model"],"turns":meta["turns"],
                "input_tokens":tokens.input_tokens,"cached_input_tokens":tokens.cached_input_tokens,
                "cache_creation_input_tokens":tokens.cache_creation_input_tokens,"output_tokens":tokens.output_tokens,
                "reasoning_output_tokens":tokens.reasoning_output_tokens,"total_tokens":tokens.total_tokens,
                "codex_current_as_of":current.map(|v|v.as_of),
                "codex_included_allowance_basis":current.map(|v|v.included_allowance_basis.as_str()),
                "codex_purchased_missing_models":current.map(|v|v.purchased_credits.missing_models.join(";")),
                "codex_purchased_unpriced_models":current.map(|v|v.purchased_credits.unpriced_models.join(";")),
                "codex_included_missing_models":current.map(|v|v.included_allowance.missing_models.join(";")),
                "codex_included_unpriced_models":current.map(|v|v.included_allowance.unpriced_models.join(";")),
                "codex_api_missing_models":current.map(|v|v.api_estimate.missing_models.join(";")),
                "codex_api_unpriced_models":current.map(|v|v.api_estimate.unpriced_models.join(";")),
            });
            row.as_object_mut().expect("row object").extend(json!({
                "codex_purchased_credit_estimate":current.map(|v|available(&v.purchased_credits,false)),
                "codex_included_allowance_credit_equivalent_estimate":current.map(|v|available(&v.included_allowance,false)),
                "codex_api_base_estimate":current.map(|v|available(&v.api_estimate,false)),
                "codex_plan_surface":if codex {json!("legacy_reference")} else {Value::Null},
                "codex_credits":if codex && pricing.plan.total.is_finite() {json!(pricing.plan.total)} else {Value::Null},
                "codex_estimated_api_usd":if codex {api.map(|v|available(v,true)).unwrap_or(Value::Null)} else {Value::Null},
                "codex_time_aware_api_usd":Value::Null,"codex_time_aware_api_status":if codex {time_status.clone()} else {Value::Null},
                "time_aware_api_status":time_status,"pricing_catalog_available":catalog_available,
                "rate_card_version":rates.version,"rate_card_fetched_at":rates.fetched_at,
                "cache_write_pricing":if codex && catalog_available {json!("unmodeled_not_observed")} else {Value::Null},
                "claude_estimated_usd":if provider=="claude_code" && plan.total.is_finite() {json!(plan.total)} else {Value::Null},
                "display_currency":if use_api {"USD"} else if current.is_some() {"purchased credits"} else {rates.currencies.get(provider).map(String::as_str).unwrap_or(&rates.currency)},
                "fallback_models":displayed.missing_models.join(";"),"unpriced_models":displayed.unpriced_models.join(";")
            }).as_object().expect("row fields").clone());
            if request.include_working_directory {
                row["working_directory"] = meta["working_directory"].clone();
            }
            authority.push(json!([
                key,
                version,
                hash,
                fingerprint,
                locations,
                lifecycle,
                present
            ]));
            rows.push(row);
        }
        let content = if request.format == "json" {
            serde_json::to_string_pretty(&rows)? + "\n"
        } else {
            // Retain the established CSV column order for existing consumers.
            let mut headers = vec![
                "id",
                "storage_id",
                "harness",
                "name",
                "started_at",
                "last_event_at",
                "archived",
                "source_availability",
                "lifecycle",
                "subagent",
                "parent_thread_id",
                "model",
                "turns",
                "input_tokens",
                "cached_input_tokens",
                "cache_creation_input_tokens",
                "output_tokens",
                "reasoning_output_tokens",
                "total_tokens",
                "codex_current_as_of",
                "codex_included_allowance_basis",
                "codex_purchased_missing_models",
                "codex_purchased_unpriced_models",
                "codex_included_missing_models",
                "codex_included_unpriced_models",
                "codex_api_missing_models",
                "codex_api_unpriced_models",
                "codex_purchased_credit_estimate",
                "codex_included_allowance_credit_equivalent_estimate",
                "codex_api_base_estimate",
                "codex_plan_surface",
                "codex_credits",
                "codex_estimated_api_usd",
                "codex_time_aware_api_usd",
                "codex_time_aware_api_status",
                "time_aware_api_status",
                "pricing_catalog_available",
                "rate_card_version",
                "rate_card_fetched_at",
                "cache_write_pricing",
                "claude_estimated_usd",
                "display_currency",
                "fallback_models",
                "unpriced_models",
            ];
            if request.include_working_directory {
                headers.push("working_directory");
            }
            let mut lines = vec![headers
                .iter()
                .map(|h| cell(&json!(h)))
                .collect::<Vec<_>>()
                .join(",")];
            for row in &rows {
                lines.push(
                    headers
                        .iter()
                        .map(|h| cell(&row[*h]))
                        .collect::<Vec<_>>()
                        .join(","),
                );
            }
            lines.join("\r\n") + "\r\n"
        };
        if content.len() > control.max_output_bytes {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        control
            .check()
            .map_err(|_| AccountingIntegrityError::Unverified)?;
        // Value objects canonicalize map keys, including HashMaps inside the card.
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&canonical(json!([
                request,
                as_of,
                rates,
                rate_revision,
                authority,
                content
            ])))?)
        );
        Ok(PreparedSessionSummaryExport {
            session_count: keys.len(),
            request,
            as_of,
            digest,
            content,
        })
    }
    pub(crate) fn publish_session_summary_export(
        &self,
        prepared: &PreparedSessionSummaryExport,
        rates: &crate::rates::RateCard,
        now: chrono::DateTime<chrono::Utc>,
        rate_revision: u64,
        publish: impl FnOnce(&str) -> Result<()>,
    ) -> Result<()> {
        if prepared.digest.len() != 64
            || prepared.content.len() > 8 * 1024 * 1024
            || now.signed_duration_since(prepared.as_of) > chrono::Duration::minutes(15)
            || prepared.as_of > now + chrono::Duration::seconds(5)
        {
            return Err(changed());
        }
        self.with_accounting_publication(64, |tx, control| {
            let current = self.summary_export_on(
                tx,
                control,
                prepared.request.clone(),
                rates,
                prepared.as_of,
                rate_revision,
            )?;
            if current.digest != prepared.digest || current.session_count != prepared.session_count
            {
                return Err(changed());
            }
            publish(&current.content)
        })
    }
    pub(super) fn with_accounting_publication<T>(
        &self,
        max_windows: usize,
        publish: impl FnOnce(&Connection, &QueryControl) -> Result<T>,
    ) -> Result<T> {
        let mut control = QueryControl::default();
        control.max_windows = max_windows;
        let mut conn = self.connection()?;
        conn.busy_timeout(control.remaining().min(Duration::from_millis(50)))?;
        let progress = control.clone();
        if let Err(error) = conn.progress_handler(1000, Some(move || progress.check().is_err())) {
            conn.busy_timeout(Duration::from_secs(5))?;
            return Err(error.into());
        }
        let result = (|| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = publish(&tx, &control)?;
            tx.commit()?;
            Ok(value)
        })();
        // This is a persistent writer, unlike a disposable read connection.
        // Always remove this request's deadline and restore normal contention
        // policy on success, refused authority, rollback, or filesystem error.
        let progress_cleanup = conn.progress_handler(0, None::<fn() -> bool>);
        let timeout_cleanup = conn.busy_timeout(Duration::from_secs(5));
        progress_cleanup?;
        timeout_cleanup?;
        result
    }
    pub(crate) fn publish_activity_summary_export(
        &self,
        request: &ActivitySummaryExportRequest,
        scan_complete: bool,
        publish: impl FnOnce(&str) -> Result<()>,
    ) -> Result<()> {
        if request.days.is_empty()
            || request.days.len() > 366
            || request.svg.is_empty()
            || request.svg.len() > 1024 * 1024
            || request.days.iter().any(|day| {
                day.from > day.to
                    || day.to.signed_duration_since(day.from) > chrono::Duration::hours(26)
            })
            || request
                .days
                .windows(2)
                .any(|pair| pair[0].to >= pair[1].from)
        {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        self.exclusion_cache
            .lock()
            .unwrap()
            .verify()
            .map_err(|_| AccountingIntegrityError::Unverified)?;
        let recovered = self
            .recovery_receipt()
            .map_err(|_| AccountingIntegrityError::Unverified)?
            .is_some();
        self.with_accounting_publication(366, |tx, control| {
            validate_scope(&request.session_ids, control)?;
            let windows: Vec<_> = request
                .days
                .iter()
                .map(|day| (Some(day.from), Some(day.to)))
                .collect();
            self.prove_accounting_on(tx, &request.session_ids, &windows, control)?;
            let totals = self
                .range_totals_multi_on(tx, control, &request.session_ids, &windows)
                .map_err(AccountingIntegrityError::from_query)?;
            let coverage: bool = tx
                .query_row(
                    "SELECT value='1' FROM history_meta WHERE key='coverage_complete'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|_| AccountingIntegrityError::Unverified)?;
            if request.coverage_complete != (coverage && !recovered && scan_complete) {
                return Err(changed());
            }
            for (day, range) in request.days.iter().zip(totals) {
                let mut tokens = 0u64;
                let mut calls = 0u64;
                for value in range.values() {
                    tokens = tokens
                        .checked_add(value.tokens.total_tokens)
                        .ok_or(AccountingIntegrityError::Unverified)?;
                    calls = calls
                        .checked_add(value.tool_metrics.calls)
                        .ok_or(AccountingIntegrityError::Unverified)?;
                }
                if tokens != day.tokens || calls != day.tool_calls {
                    return Err(changed());
                }
            }
            control
                .check()
                .map_err(|_| AccountingIntegrityError::Unverified)?;
            publish(&request.svg)
        })
    }
    pub(crate) fn publish_tool_dimension_export(
        &self,
        request: &ToolDimensionExportRequest,
        publish: impl FnOnce(&str) -> Result<()>,
    ) -> Result<()> {
        if !matches!(request.format.as_str(), "csv" | "json")
            || request.rows.len() > 10_000
            || matches!((request.from,request.to),(Some(from),Some(to)) if from>to)
            || request
                .rows
                .iter()
                .any(|row| row.dimension_kind.len() > 64 || row.dimension_value.len() > 1024)
            || serde_json::to_vec(request)?.len() > 8 * 1024 * 1024
        {
            return Err(AccountingIntegrityError::Unverified.into());
        }
        self.with_accounting_publication(1, |tx, control| {
            validate_scope(&request.session_ids, control)?;
            let windows = [(request.from, request.to)];
            self.prove_accounting_on(tx, &request.session_ids, &windows, control)?;
            let range = self
                .range_totals_multi_on(tx, control, &request.session_ids, &windows)
                .map_err(AccountingIntegrityError::from_query)?
                .pop()
                .unwrap_or_default();
            let mut dimensions =
                BTreeMap::<(String, String), crate::model::ToolDimensionMetrics>::new();
            for facts in range.into_values() {
                for (kind, values) in facts.tool_dimensions {
                    for (value, metric) in values {
                        control
                            .consume_row()
                            .map_err(|_| AccountingIntegrityError::Unverified)?;
                        let target = dimensions.entry((kind.clone(), value)).or_default();
                        target.calls = target
                            .calls
                            .checked_add(metric.calls)
                            .ok_or(AccountingIntegrityError::Unverified)?;
                        target.failures = target
                            .failures
                            .checked_add(metric.failures)
                            .ok_or(AccountingIntegrityError::Unverified)?;
                        target.output_bytes = target
                            .output_bytes
                            .checked_add(metric.output_bytes)
                            .ok_or(AccountingIntegrityError::Unverified)?;
                        target.duration_ms = target
                            .duration_ms
                            .checked_add(metric.duration_ms)
                            .ok_or(AccountingIntegrityError::Unverified)?;
                        target.tokens = target
                            .tokens
                            .checked_add(metric.tokens)
                            .ok_or(AccountingIntegrityError::Unverified)?;
                    }
                }
            }
            let rows: Vec<_> = dimensions
                .into_iter()
                .map(|((kind, value), metric)| ToolDimensionExportRow {
                    dimension_kind: kind,
                    dimension_value: value,
                    calls: metric.calls,
                    failures: metric.failures,
                    output_bytes: metric.output_bytes,
                    duration_ms: metric.duration_ms,
                    tokens: metric.tokens,
                })
                .collect();
            if rows.len() > 10_000 {
                return Err(AccountingIntegrityError::Unverified.into());
            }
            let mut expected = request.rows.clone();
            expected.sort_by(|a, b| {
                (&a.dimension_kind, &a.dimension_value)
                    .cmp(&(&b.dimension_kind, &b.dimension_value))
            });
            if expected != rows {
                return Err(changed());
            }
            let content = if request.format == "json" {
                serde_json::to_string_pretty(&rows)? + "\n"
            } else if rows.is_empty() {
                String::new()
            } else {
                let mut lines = vec![
                    "dimension_kind,dimension_value,calls,failures,output_bytes,duration_ms,tokens"
                        .to_owned(),
                ];
                for row in &rows {
                    lines.push(
                        [
                            cell(&json!(row.dimension_kind)),
                            cell(&json!(row.dimension_value)),
                            row.calls.to_string(),
                            row.failures.to_string(),
                            row.output_bytes.to_string(),
                            row.duration_ms.to_string(),
                            row.tokens.to_string(),
                        ]
                        .join(","),
                    );
                }
                lines.join("\r\n") + "\r\n"
            };
            if content.len() > control.max_output_bytes {
                return Err(AccountingIntegrityError::Unverified.into());
            }
            control
                .check()
                .map_err(|_| AccountingIntegrityError::Unverified)?;
            publish(&content)
        })
    }
}
