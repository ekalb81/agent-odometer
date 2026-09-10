//! Shared dispatch for read-only CLI and MCP adapters. Pricing and aggregation
//! stay in `query`; adapters select a report and format its result.

use anyhow::{bail, Result};
use chrono::{DateTime, FixedOffset, TimeZone, Utc};
use serde_json::{json, Value};

use crate::{config::Config, history_store::HistoryStore, query, rates::RateCard};

pub const LEDGER_UNAVAILABLE: &str =
    "durable history is unavailable; open the desktop app to prepare it, or retry when it is ready";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    Status,
    Report,
    Models,
    Projects,
    Metrics,
    Sessions,
    Activity,
    Categories,
    Tools,
    Context,
    Findings,
    Diagnostics,
    Quota,
    Mirrors,
    Statusline,
}

impl QueryKind {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "status" => Self::Status,
            "report" => Self::Report,
            "models" => Self::Models,
            "projects" => Self::Projects,
            "metrics" => Self::Metrics,
            "sessions" => Self::Sessions,
            "activity" => Self::Activity,
            "categories" => Self::Categories,
            "tools" => Self::Tools,
            "context" => Self::Context,
            "findings" => Self::Findings,
            "diagnostics" => Self::Diagnostics,
            "quota" => Self::Quota,
            "mirrors" => Self::Mirrors,
            "statusline" => Self::Statusline,
            _ => return None,
        })
    }

    pub fn accepts_window(self) -> bool {
        !matches!(
            self,
            Self::Status | Self::Diagnostics | Self::Quota | Self::Mirrors | Self::Statusline
        )
    }
}

#[derive(Debug, Clone)]
pub struct Request {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<usize>,
    pub include_paths: bool,
    /// UTC by default. A CLI may explicitly use its current local offset.
    pub utc_offset: FixedOffset,
}

impl Default for Request {
    fn default() -> Self {
        Self {
            from: None,
            to: None,
            limit: None,
            include_paths: false,
            utc_offset: FixedOffset::east_opt(0).expect("UTC is a valid offset"),
        }
    }
}

impl Request {
    pub fn validate(&self) -> Result<()> {
        if self.from.zip(self.to).is_some_and(|(from, to)| from > to) {
            bail!("'to' is before 'from'");
        }
        if self.limit.is_some_and(|limit| !(1..=1000).contains(&limit)) {
            bail!("'limit' must be an integer from 1 to 1000");
        }
        Ok(())
    }

    pub fn validate_for(&self, kind: QueryKind) -> Result<()> {
        self.validate()?;
        if !kind.accepts_window() && (self.from.is_some() || self.to.is_some()) {
            bail!("this query does not accept a date window");
        }
        if kind != QueryKind::Sessions && self.limit.is_some() {
            bail!("only sessions accepts 'limit'");
        }
        if self.include_paths && !matches!(kind, QueryKind::Projects | QueryKind::Diagnostics) {
            bail!("only projects and diagnostics accept 'include_paths'");
        }
        Ok(())
    }
}

/// Strict UTC calendar-date parser shared by the wire adapter and its tests.
pub fn parse_date(raw: &str, end_of_day: bool) -> Result<DateTime<Utc>> {
    if raw.len() != 10 || raw.as_bytes()[4] != b'-' || raw.as_bytes()[7] != b'-' {
        bail!("'{raw}' must be a YYYY-MM-DD date");
    }
    let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .map_err(|_| anyhow::anyhow!("'{raw}' must be a YYYY-MM-DD date"))?;
    let instant = if end_of_day {
        date.and_hms_milli_opt(23, 59, 59, 999)
    } else {
        date.and_hms_opt(0, 0, 0)
    };
    Ok(Utc.from_utc_datetime(&instant.ok_or_else(|| anyhow::anyhow!("invalid date"))?))
}

/// Execute against an already-open, bounded read-only ledger. This function
/// does not open a ledger, scan transcripts, perform network access, or render.
pub fn execute(
    kind: QueryKind,
    store: &HistoryStore,
    rates: &RateCard,
    config: &Config,
    request: &Request,
    now: DateTime<Utc>,
) -> Result<Value> {
    execute_optional(kind, Some(store), rates, config, request, now, None)
}

/// Availability reports remain useful before a ledger is ready. Every other
/// report requires a ready ledger; unavailable usage never becomes an empty result.
pub fn execute_optional(
    kind: QueryKind,
    store: Option<&HistoryStore>,
    rates: &RateCard,
    config: &Config,
    request: &Request,
    now: DateTime<Utc>,
    control: Option<&crate::query_control::QueryControl>,
) -> Result<Value> {
    request.validate_for(kind)?;
    if let Some(control) = control {
        control.check()?;
    }
    if let Some(store) = store {
        store.check_query()?;
    }
    let Request { from, to, .. } = *request;
    let provider = |key: &str| {
        query::provider_for_key(key)
            .map(|id| id.to_string())
            .unwrap_or_default()
    };
    let required_store = || store.ok_or_else(|| anyhow::anyhow!(LEDGER_UNAVAILABLE));
    let result = match kind {
        QueryKind::Status => json!({
            "schema_version": 1,
            "ledger_available": store.is_some(),
            "sessions": store.map(HistoryStore::session_count).transpose()?,
            "ledger_bytes": store.and_then(|store| store.database_footprint().total_bytes()),
            "rate_card_version": rates.version,
            "rate_card_fetched_at": rates.fetched_at,
        }),
        QueryKind::Report | QueryKind::Models => serde_json::to_value(query::range_report(
            required_store()?,
            rates,
            provider,
            from,
            to,
            now,
        )?)?,
        QueryKind::Projects => {
            let mut report =
                query::project_report(required_store()?, rates, provider, from, to, now)?;
            if !request.include_paths {
                for project in &mut report.projects {
                    project.label = project.redacted_label().to_owned();
                }
            }
            serde_json::to_value(report)?
        }
        QueryKind::Metrics => serde_json::to_value(query::workflow_metrics(
            required_store()?,
            rates,
            provider,
            from,
            to,
            now,
        )?)?,
        QueryKind::Sessions => serde_json::to_value(query::session_report(
            required_store()?,
            rates,
            provider,
            from,
            to,
            Some(request.limit.unwrap_or(20)),
            now,
        )?)?,
        QueryKind::Activity => serde_json::to_value(query::activity_heatmap(
            required_store()?,
            request.utc_offset,
            from,
            to,
        )?)?,
        QueryKind::Categories => serde_json::to_value(query::category_report(
            required_store()?,
            rates,
            from,
            to,
            now,
        )?)?,
        QueryKind::Tools => {
            serde_json::to_value(query::tools_report(required_store()?, from, to)?)?
        }
        QueryKind::Context => {
            serde_json::to_value(query::context_report(required_store()?, from, to)?)?
        }
        QueryKind::Findings => {
            serde_json::to_value(query::findings_report(required_store()?, from, to)?)?
        }
        QueryKind::Diagnostics => serde_json::to_value(query::diagnostics_report_controlled(
            store,
            config,
            rates,
            now,
            request.include_paths,
            control,
        )?)?,
        // Preserve the original unwrapped array contracts for existing CLI
        // scripts and quota_status clients. New adapters wrap explicitly.
        QueryKind::Quota => {
            let store = required_store()?;
            let policy = crate::quota_store::QuotaStoreFile::load();
            serde_json::to_value(query::quota_snapshots(
                store,
                now,
                chrono::Duration::seconds(policy.max_cache_age_secs),
            )?)?
        }
        QueryKind::Mirrors => serde_json::to_value(required_store()?.mirrored_session_groups()?)?,
        QueryKind::Statusline => serde_json::to_value(query::statusline_report(
            required_store()?,
            rates,
            now,
            request.utc_offset,
        )?)?,
    };
    if let Some(store) = store {
        store.check_query()?;
    }
    if let Some(control) = control {
        control.check()?;
    }
    Ok(result)
}
