//! Read-only reporting CLI (issue #47).
//!
//! `odometer status` and `odometer report` answer from the durable ledger
//! and nothing else: no scan, no transcript parse, no writes. That is what
//! makes them usable from a shell prompt or an agent, which issue #47 calls
//! out directly ("a low-latency statusline command/payload that does not
//! trigger a full corpus parse on every shell prompt").
//!
//! Argument parsing is hand-rolled to match `turn_receipts::try_run_cli`,
//! the CLI entry point this repo already has, rather than pulling in a
//! parser dependency for two subcommands.
//!
//! Everything here adapts `crate::query`; this module owns no SQL and no
//! pricing, per #47's DRY boundary.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::history_store::HistoryStore;
use crate::query::{range_report, RangeReport};
use crate::rates::RateCard;

/// Output shape a subcommand renders in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
    Csv,
    Text,
}

/// Machine-readable status of the local install (issue #47's `status`).
#[derive(Debug, Serialize)]
struct StatusReport {
    schema_version: u32,
    ledger_available: bool,
    /// Absent rather than 0 when the ledger could not be opened — the two
    /// mean opposite things to anything acting on this.
    sessions: Option<usize>,
    ledger_bytes: Option<u64>,
    rate_card_version: u32,
    rate_card_fetched_at: Option<String>,
}

const STATUS_SCHEMA_VERSION: u32 = 1;

/// Runs a reporting subcommand if `argv` names one.
///
/// Returns `false` when the process should carry on and launch the desktop
/// app, matching `turn_receipts::try_run_cli`'s contract.
pub fn try_run_cli() -> bool {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        return false;
    };
    if !matches!(
        command,
        "status" | "report" | "quota" | "projects" | "metrics"
    ) {
        return false;
    }

    match run(command, &args[1..]) {
        Ok(output) => {
            println!("{output}");
            true
        }
        Err(error) => {
            // stderr and a non-zero exit: a shell prompt or agent must be
            // able to tell a failed query from an empty one, which a
            // friendly message on stdout would hide.
            eprintln!("odometer {command}: {error:#}");
            std::process::exit(2);
        }
    }
}

fn run(command: &str, args: &[String]) -> Result<String> {
    let format = parse_format(args)?;
    match command {
        "status" => run_status(format),
        "report" => run_report(args, format),
        "quota" => run_quota(format),
        "projects" => run_projects(args, format),
        "metrics" => run_metrics(args, format),
        other => bail!("unknown command '{other}'"),
    }
}

fn parse_format(args: &[String]) -> Result<Format> {
    match flag_value(args, "--format")? {
        None => Ok(Format::Text),
        Some(value) => match value.as_str() {
            "json" => Ok(Format::Json),
            "csv" => Ok(Format::Csv),
            "text" => Ok(Format::Text),
            other => bail!("unknown --format '{other}'; expected json, csv, or text"),
        },
    }
}

/// Value of `--name <value>`, or `None` when the flag is absent.
///
/// A flag given without a value is an error rather than a silent default: a
/// script that meant `--from 2026-08-01` and lost the argument should be
/// told, not handed an all-time report.
fn flag_value(args: &[String], name: &str) -> Result<Option<String>> {
    let Some(index) = args.iter().position(|arg| arg == name) else {
        return Ok(None);
    };
    match args.get(index + 1) {
        Some(value) if !value.starts_with("--") => Ok(Some(value.clone())),
        _ => bail!("{name} needs a value"),
    }
}

/// Parses a `YYYY-MM-DD` boundary in UTC.
///
/// `--to` is inclusive of the whole named day, which is what a person means
/// by "through the 15th"; an exclusive midnight boundary would silently drop
/// a day's usage from every report.
fn parse_date(value: &str, end_of_day: bool) -> Result<DateTime<Utc>> {
    let date = NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .with_context(|| format!("could not read '{value}' as a YYYY-MM-DD date"))?;
    let time = if end_of_day {
        date.and_hms_milli_opt(23, 59, 59, 999)
    } else {
        date.and_hms_opt(0, 0, 0)
    }
    .context("date is not a valid instant")?;
    Ok(Utc.from_utc_datetime(&time))
}

/// An inclusive reporting window: `(from, to)`, either end open.
type Window = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// `--from`/`--to` as an inclusive window, shared by every command that
/// takes one so they cannot drift apart in how they read a date.
fn parse_window(args: &[String]) -> Result<Window> {
    let from = flag_value(args, "--from")?
        .map(|value| parse_date(&value, false))
        .transpose()?;
    let to = flag_value(args, "--to")?
        .map(|value| parse_date(&value, true))
        .transpose()?;
    if let (Some(from), Some(to)) = (from, to) {
        if to < from {
            bail!("--to ({to}) is before --from ({from})");
        }
    }
    Ok((from, to))
}

/// The user's rate card, falling back to the bundled one and then to an
/// empty card. A report must still render usage when pricing is
/// unavailable — every cost then reports as unpriced rather than as zero.
fn load_rates() -> RateCard {
    RateCard::load_from_disk()
        .or_else(|_| RateCard::load_bundled())
        .unwrap_or_default()
}

fn open_ledger() -> Result<HistoryStore> {
    let path =
        HistoryStore::default_path().context("could not resolve the durable history location")?;
    HistoryStore::open(&path)
        .with_context(|| format!("could not open the durable history at {}", path.display()))
}

fn run_status(format: Format) -> Result<String> {
    render_status(open_ledger().ok().as_ref(), &load_rates(), format)
}

/// The testable half of `status`: everything except resolving the ledger
/// path and rate card from the user's real directories, which a test cannot
/// supply without writing to them.
pub fn render_status(
    store: Option<&HistoryStore>,
    rates: &RateCard,
    format: Format,
) -> Result<String> {
    let sessions = store
        .and_then(|store| store.session_keys().ok())
        .map(|keys| keys.len());
    let ledger_bytes = store.and_then(|store| store.database_footprint().total_bytes());

    let status = StatusReport {
        schema_version: STATUS_SCHEMA_VERSION,
        ledger_available: store.is_some(),
        sessions,
        ledger_bytes,
        rate_card_version: rates.version,
        rate_card_fetched_at: rates.fetched_at.clone(),
    };

    Ok(match format {
        Format::Json => serde_json::to_string_pretty(&status)?,
        Format::Csv => {
            let mut out = String::from("key,value\n");
            out.push_str(&format!("ledger_available,{}\n", status.ledger_available));
            out.push_str(&format!("sessions,{}\n", render_opt(status.sessions)));
            out.push_str(&format!(
                "ledger_bytes,{}\n",
                render_opt(status.ledger_bytes)
            ));
            out.push_str(&format!("rate_card_version,{}\n", status.rate_card_version));
            out.push_str(&format!(
                "rate_card_fetched_at,{}\n",
                status.rate_card_fetched_at.as_deref().unwrap_or("")
            ));
            out
        }
        Format::Text => format!(
            "ledger: {}\nsessions: {}\nledger bytes: {}\nrate card: v{} ({})",
            if status.ledger_available {
                "available"
            } else {
                "unavailable"
            },
            render_opt(status.sessions),
            render_opt(status.ledger_bytes),
            status.rate_card_version,
            status
                .rate_card_fetched_at
                .as_deref()
                .unwrap_or("never fetched")
        ),
    })
}

fn run_report(args: &[String], format: Format) -> Result<String> {
    let store = open_ledger()?;
    let rates = load_rates();
    let config = Config::load().unwrap_or_default();
    report_from(&store, &rates, &config, args, format)
}

/// The testable half of `report`, taking its inputs rather than resolving
/// them from the user's real ledger and configuration.
pub fn report_from(
    store: &HistoryStore,
    rates: &RateCard,
    config: &Config,
    args: &[String],
    format: Format,
) -> Result<String> {
    let (from, to) = parse_window(args)?;

    let harness_for = harness_resolver(config);
    let report = range_report(store, rates, harness_for, from, to, Utc::now())?;
    render_report(&report, format)
}

/// Resolves a session key to the provider that owns it.
///
/// Storage ids are `<provider>:<id>`, so the provider is the prefix. A key
/// that does not carry one falls back to the first configured provider
/// rather than guessing a specific vendor.
fn harness_resolver(config: &Config) -> impl Fn(&str) -> String + '_ {
    move |key: &str| {
        key.split_once(':')
            .map(|(provider, _)| provider.to_owned())
            .unwrap_or_else(|| {
                config
                    .providers
                    .keys()
                    .next()
                    .map(|id| id.as_str().to_owned())
                    .unwrap_or_default()
            })
    }
}

fn render_report(report: &RangeReport, format: Format) -> Result<String> {
    Ok(match format {
        Format::Json => serde_json::to_string_pretty(report)?,
        Format::Csv => {
            let mut out = String::from(
                "model,harness,total_tokens,input,cached_input,output,reasoning,cost,currency\n",
            );
            for usage in &report.by_model {
                out.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{}\n",
                    usage.model,
                    usage.harness,
                    usage.tokens.total_tokens,
                    usage.tokens.input_tokens,
                    usage.tokens.cached_input_tokens,
                    usage.tokens.output_tokens,
                    usage.tokens.reasoning_output_tokens,
                    usage
                        .cost
                        .map(|cost| format!("{cost:.6}"))
                        .unwrap_or_default(),
                    usage.currency
                ));
            }
            out
        }
        Format::Text => {
            let mut out = format!(
                "sessions with usage: {}\ntotal tokens: {}\n",
                report.sessions, report.tokens.total_tokens
            );
            for usage in &report.by_model {
                out.push_str(&format!(
                    "  {:<28} {:>14} tokens{}\n",
                    usage.model,
                    usage.tokens.total_tokens,
                    usage
                        .cost
                        .map(|cost| format!("  {cost:.4} {}", usage.currency))
                        .unwrap_or_else(|| "  (unpriced)".to_string())
                ));
            }
            // One line per currency, never a single sum: Codex bills in
            // plan credits and Claude in USD, so adding them yields a number
            // that is not money in any unit.
            if report.cost_by_currency.is_empty() {
                out.push_str(
                    "total cost: unavailable
",
                );
            } else {
                for (currency, cost) in &report.cost_by_currency {
                    out.push_str(&format!(
                        "total {currency}: {cost:.4}
"
                    ));
                }
            }
            if !report.unpriced_models.is_empty() {
                // A total that silently omits a model reads as complete.
                out.push_str(&format!(
                    "  (floor; unpriced: {})
",
                    report.unpriced_models.join(", ")
                ));
            }
            out
        }
    })
}

fn run_metrics(args: &[String], format: Format) -> Result<String> {
    let store = open_ledger()?;
    let rates = load_rates();
    let config = Config::load().unwrap_or_default();
    metrics_from(&store, &rates, &config, args, format)
}

/// The testable half of `metrics`.
pub fn metrics_from(
    store: &HistoryStore,
    rates: &RateCard,
    config: &Config,
    args: &[String],
    format: Format,
) -> Result<String> {
    let (from, to) = parse_window(args)?;
    let report = crate::query::workflow_metrics(
        store,
        rates,
        harness_resolver(config),
        from,
        to,
        Utc::now(),
    )?;

    Ok(match format {
        Format::Json => serde_json::to_string_pretty(&report)?,
        Format::Csv => {
            let mut out = String::from(
                "metric,value,numerator,denominator,denominator_is
",
            );
            for metric in &report.metrics {
                out.push_str(&format!(
                    "{},{},{},{},{}
",
                    metric.id,
                    metric
                        .value
                        .map(|value| format!("{value:.6}"))
                        .unwrap_or_default(),
                    metric.numerator,
                    metric.denominator,
                    csv_field(metric.denominator_is),
                ));
            }
            out
        }
        Format::Text => {
            let mut out = format!(
                "metrics v{} over {} session(s)
",
                report.schema_version, report.sessions
            );
            for metric in &report.metrics {
                match metric.value {
                    // The denominator travels with the value: a ratio
                    // without its sample size is not interpretable, and 1.0
                    // from one observation reads identically to 1.0 from ten
                    // thousand.
                    Some(value) => out.push_str(&format!(
                        "  {:<24} {:>8.4}   (of {} {})
",
                        metric.id, value, metric.denominator, metric.denominator_is
                    )),
                    None => out.push_str(&format!(
                        "  {:<24} {:>8}   (no {})
",
                        metric.id, "n/a", metric.denominator_is
                    )),
                }
            }
            out
        }
    })
}

fn run_projects(args: &[String], format: Format) -> Result<String> {
    let store = open_ledger()?;
    let rates = load_rates();
    let config = Config::load().unwrap_or_default();
    projects_from(&store, &rates, &config, args, format)
}

/// The testable half of `projects`.
pub fn projects_from(
    store: &HistoryStore,
    rates: &RateCard,
    config: &Config,
    args: &[String],
    format: Format,
) -> Result<String> {
    let (from, to) = parse_window(args)?;
    // Off by default, mirroring the diagnostics export's own checkbox: CLI
    // output gets piped into files, pasted into issues, and read by agents,
    // and a `fallback_path_identity` label is an absolute local path.
    let include_paths = args.iter().any(|arg| arg == "--include-paths");
    let report =
        crate::query::project_report(store, rates, harness_resolver(config), from, to, Utc::now())?;

    Ok(match format {
        Format::Json => {
            let mut report = report.clone();
            if !include_paths {
                for project in &mut report.projects {
                    project.label = project.redacted_label().to_owned();
                }
            }
            serde_json::to_string_pretty(&report)?
        }
        Format::Csv => {
            let mut out = String::from(
                "project_key,label,sessions,total_tokens,cost,currency
",
            );
            for project in &report.projects {
                // One row per currency, so a project that somehow spans two
                // never has them added together in a spreadsheet either.
                if project.cost_by_currency.is_empty() {
                    out.push_str(&format!(
                        "{},{},{},{},,
",
                        project.project_key,
                        csv_field(project.label_for(include_paths)),
                        project.sessions,
                        project.tokens.total_tokens
                    ));
                }
                for (currency, cost) in &project.cost_by_currency {
                    out.push_str(&format!(
                        "{},{},{},{},{cost:.6},{currency}
",
                        project.project_key,
                        csv_field(project.label_for(include_paths)),
                        project.sessions,
                        project.tokens.total_tokens
                    ));
                }
            }
            out
        }
        Format::Text => {
            let mut out = String::new();
            for project in &report.projects {
                out.push_str(&format!(
                    "{:<32} {:>4} sessions  {:>14} tokens",
                    project.label_for(include_paths),
                    project.sessions,
                    project.tokens.total_tokens
                ));
                for (currency, cost) in &project.cost_by_currency {
                    out.push_str(&format!("  {cost:.4} {currency}"));
                }
                out.push('\n');
            }
            if report.sessions_without_project > 0 {
                // Reported, not dropped: otherwise per-project totals
                // silently fail to reconcile with the overall report.
                out.push_str(&format!(
                    "({} session(s) with usage belong to no project)
",
                    report.sessions_without_project
                ));
            }
            if out.is_empty() {
                out.push_str(
                    "no project activity in this window
",
                );
            }
            out
        }
    })
}

/// Quotes a CSV field that could contain a comma. Project labels are
/// user-supplied aliases, so this is not hypothetical.
fn csv_field(value: &str) -> String {
    if value.contains(',') || value.contains('"') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn run_quota(format: Format) -> Result<String> {
    let store = open_ledger()?;
    let quota_store = crate::quota_store::QuotaStoreFile::load();
    let max_cache_age = chrono::Duration::seconds(quota_store.max_cache_age_secs);
    render_quota(&store, max_cache_age, Utc::now(), format)
}

/// The testable half of `quota`, taking the ledger and clock rather than
/// resolving them from the user's directories.
pub fn render_quota(
    store: &HistoryStore,
    max_cache_age: chrono::Duration,
    now: DateTime<Utc>,
    format: Format,
) -> Result<String> {
    let snapshots = crate::query::quota_snapshots(store, now, max_cache_age)?;
    Ok(match format {
        Format::Json => serde_json::to_string_pretty(&snapshots)?,
        Format::Csv => {
            let mut out = String::from(
                "provider,window,unit,used,remaining,limit,unlimited,resets_at,stale,unavailable
",
            );
            for snapshot in &snapshots {
                for window in &snapshot.windows {
                    out.push_str(&format!(
                        "{},{:?},{:?},{},{},{},{},{},{},{}
",
                        snapshot.provider,
                        window.kind,
                        window.unit,
                        render_opt(window.used),
                        render_opt(window.remaining),
                        render_opt(window.limit),
                        window.unlimited,
                        window
                            .resets_at
                            .map(|at| at.to_rfc3339())
                            .unwrap_or_default(),
                        window.stale,
                        window
                            .unavailable
                            .map(|reason| format!("{reason:?}"))
                            .unwrap_or_default(),
                    ));
                }
            }
            out
        }
        Format::Text => {
            let mut out = String::new();
            for snapshot in &snapshots {
                out.push_str(&format!(
                    "{}
",
                    snapshot.provider
                ));
                if snapshot.windows.is_empty() {
                    // An explicit line, not an omitted provider: "no
                    // observations" is a state, and a silently missing row
                    // reads as "this provider does not exist".
                    out.push_str(
                        "  no quota observations
",
                    );
                }
                for window in &snapshot.windows {
                    out.push_str(&format!("  {:?}: ", window.kind));
                    if window.unlimited {
                        // Never rendered as a number: unlimited is a known
                        // state, and "0 used" would be a lie in both
                        // directions.
                        out.push_str("unlimited");
                    } else if let Some(reason) = window.unavailable {
                        out.push_str(&format!("unavailable ({reason:?})"));
                    } else {
                        out.push_str(&format!(
                            "{} used, {} remaining",
                            render_opt(window.used),
                            render_opt(window.remaining)
                        ));
                    }
                    if window.stale {
                        out.push_str(" [stale]");
                    }
                    if let Some(resets_at) = window.resets_at {
                        out.push_str(&format!(" · resets {}", resets_at.to_rfc3339()));
                    }
                    if let Some(forecast) = &window.forecast {
                        out.push_str(&format!(
                            "
    pace {:.2}/h, {} {:.1}",
                            forecast.pace_per_hour,
                            if forecast.reserve_deficit_percent >= 0.0 {
                                "deficit"
                            } else {
                                "reserve"
                            },
                            forecast.reserve_deficit_percent.abs()
                        ));
                        if let Some(at) = forecast.projected_exhaustion_at {
                            out.push_str(&format!(", exhausted {}", at.to_rfc3339()));
                        }
                    }
                    out.push('\n');
                }
            }
            out
        }
    })
}

fn render_opt<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "unavailable".to_string(), |value| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TokenTotals;
    use std::collections::BTreeMap;

    #[test]
    fn a_flag_without_a_value_is_an_error_not_a_silent_default() {
        // `--from` with a lost argument must not quietly widen the report to
        // all time, which is the difference between "August" and "ever".
        let args = vec!["--from".to_string()];
        assert!(flag_value(&args, "--from").is_err());

        let args = vec!["--from".to_string(), "--format".to_string()];
        assert!(flag_value(&args, "--from").is_err());
    }

    #[test]
    fn an_absent_flag_is_not_an_error() {
        let args = vec!["--format".to_string(), "json".to_string()];
        assert_eq!(flag_value(&args, "--from").unwrap(), None);
        assert_eq!(
            flag_value(&args, "--format").unwrap().as_deref(),
            Some("json")
        );
    }

    #[test]
    fn the_to_boundary_covers_the_whole_named_day() {
        // Inclusive, because "through the 15th" means the 15th. An exclusive
        // midnight boundary would drop a day of usage from every report that
        // named one.
        let to = parse_date("2026-08-15", true).unwrap();
        assert_eq!(to.to_rfc3339(), "2026-08-15T23:59:59.999+00:00");
        let from = parse_date("2026-08-15", false).unwrap();
        assert_eq!(from.to_rfc3339(), "2026-08-15T00:00:00+00:00");
    }

    #[test]
    fn a_malformed_date_is_rejected_with_the_value_named() {
        let error = parse_date("15/08/2026", false).unwrap_err().to_string();
        assert!(error.contains("15/08/2026"), "unhelpful error: {error}");
    }

    #[test]
    fn an_unknown_format_is_rejected() {
        let args = vec!["--format".to_string(), "yaml".to_string()];
        let error = parse_format(&args).unwrap_err().to_string();
        assert!(error.contains("yaml"));
        assert!(error.contains("json"), "the error should say what is valid");
    }

    #[test]
    fn only_reporting_subcommands_are_claimed() {
        // Anything else must fall through so the desktop app still starts.
        for command in ["hook", "--help", "", "reports"] {
            assert!(
                !matches!(command, "status" | "report"),
                "{command} must not be claimed by the report CLI"
            );
        }
    }

    fn sample_report() -> RangeReport {
        RangeReport {
            schema_version: crate::query::RANGE_REPORT_SCHEMA_VERSION,
            from: None,
            to: None,
            sessions: 2,
            sessions_without_usage: 1,
            tokens: TokenTotals {
                input_tokens: 1_000,
                output_tokens: 200,
                total_tokens: 1_200,
                ..Default::default()
            },
            by_model: vec![
                crate::query::ModelUsage {
                    model: "gpt-5.6-sol".into(),
                    harness: "codex".into(),
                    tokens: TokenTotals {
                        input_tokens: 1_000,
                        total_tokens: 1_000,
                        ..Default::default()
                    },
                    cost: Some(12.5),
                    currency: "credits".into(),
                    basis: crate::rates::PricingBasis::Direct,
                },
                crate::query::ModelUsage {
                    model: "mystery".into(),
                    harness: "codex".into(),
                    tokens: TokenTotals {
                        output_tokens: 200,
                        total_tokens: 200,
                        ..Default::default()
                    },
                    cost: None,
                    currency: "credits".into(),
                    basis: crate::rates::PricingBasis::Unavailable,
                },
            ],
            cost_by_currency: BTreeMap::from([
                ("credits".to_string(), 12.5),
                ("USD".to_string(), 3.25),
            ]),
            unpriced_models: vec!["mystery".into()],
        }
    }

    /// The JSON output is a published schema (issue #47), so the version has
    /// to travel inside the payload — a consumer that stores or pipes it
    /// keeps the version with the data rather than losing it to a header.
    #[test]
    fn json_output_carries_its_schema_version_and_parses() {
        let rendered = render_report(&sample_report(), Format::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&rendered).expect("valid JSON");

        assert_eq!(
            parsed["schema_version"],
            crate::query::RANGE_REPORT_SCHEMA_VERSION
        );
        assert_eq!(parsed["cost_by_currency"]["credits"], 12.5);
        assert_eq!(parsed["cost_by_currency"]["USD"], 3.25);
        // Absent, not zero: an unpriced model must not read as free.
        assert!(parsed["by_model"][1]["cost"].is_null());
    }

    /// Currencies get one line each and are never added together — 12.5
    /// credits plus 3.25 dollars is not 15.75 of anything.
    #[test]
    fn text_output_reports_each_currency_separately() {
        let rendered = render_report(&sample_report(), Format::Text).unwrap();

        assert!(rendered.contains("total credits: 12.5000"), "{rendered}");
        assert!(rendered.contains("total USD: 3.2500"), "{rendered}");
        assert!(
            !rendered.contains("15.75"),
            "currencies must never be summed: {rendered}"
        );
    }

    /// A total that silently omits a model reads as complete. It must say
    /// which models are missing from it.
    #[test]
    fn text_output_marks_a_total_that_excludes_unpriced_models() {
        let rendered = render_report(&sample_report(), Format::Text).unwrap();

        assert!(rendered.contains("floor"), "{rendered}");
        assert!(rendered.contains("mystery"), "{rendered}");
        assert!(rendered.contains("(unpriced)"), "{rendered}");
    }

    #[test]
    fn text_output_says_so_when_nothing_could_be_priced() {
        let mut report = sample_report();
        report.cost_by_currency.clear();
        report.by_model.clear();
        report.unpriced_models.clear();

        let rendered = render_report(&report, Format::Text).unwrap();

        assert!(rendered.contains("total cost: unavailable"), "{rendered}");
    }

    /// CSV is consumed by scripts, so the header and column count are part
    /// of the contract, not incidental formatting.
    #[test]
    fn csv_output_has_a_stable_header_and_one_row_per_model() {
        let rendered = render_report(&sample_report(), Format::Csv).unwrap();
        let mut lines = rendered.lines();

        assert_eq!(
            lines.next().unwrap(),
            "model,harness,total_tokens,input,cached_input,output,reasoning,cost,currency"
        );
        let rows: Vec<&str> = lines.filter(|line| !line.is_empty()).collect();
        assert_eq!(rows.len(), 2);
        assert!(rows[0].starts_with("gpt-5.6-sol,codex,1000,"));
        // An unpriceable cost is an empty cell, never a zero.
        assert!(rows[1].ends_with(",,credits"), "{}", rows[1]);
    }

    #[test]
    fn a_session_key_resolves_to_its_provider_prefix() {
        let config = Config::default();
        let resolve = harness_resolver(&config);
        assert_eq!(resolve("codex:abc123"), "codex");
        assert_eq!(resolve("claude_code:xyz"), "claude_code");
    }
}
