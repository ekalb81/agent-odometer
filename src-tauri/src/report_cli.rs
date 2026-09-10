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
use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::headless::{QueryKind, Request};
use crate::history_store::HistoryStore;
use crate::query::{range_report, RangeReport};
use crate::query_control::QueryControl;
use crate::rates::RateCard;
use crate::report_output;

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
        "status"
            | "report"
            | "quota"
            | "projects"
            | "metrics"
            | "mirrors"
            | "sessions"
            | "activity"
            | "verify"
            | "models"
            | "categories"
            | "tools"
            | "context"
            | "findings"
            | "diagnostics"
            | "statusline"
            | "export"
            | "help"
            | "--help"
            | "-h"
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
    if matches!(command, "help" | "--help" | "-h") || args == ["--help"] {
        return Ok(help().into());
    }
    validate_arguments(command, args)?;
    let report = if command == "export" {
        flag_value(args, "--report")?.unwrap_or_else(|| "report".into())
    } else {
        command.to_owned()
    };
    let version = flag_value(args, "--schema-version")?
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(if command == "export" { 2 } else { 1 });
    if !matches!(version, 1 | 2) {
        bail!("unsupported --schema-version {version}; expected 1 or 2");
    }
    let format = if command == "export" && flag_value(args, "--format")?.is_none() {
        Format::Json
    } else {
        parse_format(args)?
    };
    let markdown = flag_value(args, "--format")?.as_deref() == Some("markdown");
    let output = if version == 2 || markdown {
        let data: serde_json::Value =
            serde_json::from_str(&run_native(&report, args, Format::Json)?)?;
        let value = if version == 2 {
            report_output::envelope(&report, data)
        } else {
            data
        };
        if markdown || format == Format::Text {
            report_output::markdown(&report, &value, version)
        } else if format == Format::Csv {
            report_output::csv(&report, &value)
        } else {
            serde_json::to_string_pretty(&value)?
        }
    } else {
        run_native(&report, args, format)?
    };
    report_output::check_size(output, QueryControl::default().max_output_bytes())
}

fn run_native(command: &str, args: &[String], format: Format) -> Result<String> {
    match command {
        "status" => run_status(format),
        "report" | "models" => run_report(args, format),
        "quota" => run_quota(format),
        "projects" => run_projects(args, format),
        "metrics" => run_metrics(args, format),
        "mirrors" => run_mirrors(format),
        "sessions" => run_sessions(args, format),
        "activity" => run_activity(args, format),
        "verify" => run_verify(format),
        "categories" | "tools" | "context" | "findings" | "diagnostics" | "statusline" => {
            let control = if command == "statusline" {
                QueryControl::with_timeout(std::time::Duration::from_millis(250))
            } else {
                QueryControl::default()
            };
            let rates = load_rates();
            let config = Config::load_read_only()?;
            let (from, to) = parse_window(args)?;
            control.check()?;
            let store = HistoryStore::default_path()
                .and_then(|path| HistoryStore::open_read_only(&path, control.clone()));
            let value = if command == "diagnostics" {
                serde_json::to_value(crate::query::diagnostics_report(
                    store.as_ref().ok(),
                    &config,
                    &rates,
                    Utc::now(),
                    args.iter().any(|arg| arg == "--include-paths"),
                )?)?
            } else {
                crate::headless::execute(
                    QueryKind::parse(command).context("unknown report")?,
                    &store?,
                    &rates,
                    &config,
                    &Request {
                        from,
                        to,
                        utc_offset: *Local::now().offset(),
                        ..Default::default()
                    },
                    Utc::now(),
                )?
            };
            control.check()?;
            match format {
                Format::Json => Ok(serde_json::to_string_pretty(&value)?),
                Format::Csv => Ok(report_output::csv(command, &value)),
                Format::Text if command == "statusline" => Ok(statusline_text(&value)),
                Format::Text => Ok(report_output::markdown(command, &value, 1)),
            }
        }
        other => bail!("unknown command '{other}'"),
    }
}

fn help() -> &'static str {
    "Odometer read-only reports\n\n\
Commands: status, report, models, projects, sessions, categories, tools, context,\n\
          findings, diagnostics, metrics, activity, quota, mirrors, statusline, verify\n\
Export:   export --report <command> [--format json|csv|markdown|text]\n\n\
Options:  --from YYYY-MM-DD --to YYYY-MM-DD (inclusive UTC dates)\n\
          --limit N (sessions only; default 20; 0 means all within query limits)\n\
          --include-paths (projects/diagnostics only)\n\
          --schema-version 1|2 (existing reports default 1; export defaults 2)\n\n\
Version 1 preserves existing JSON/CSV contracts. Version 2 carries a common\n\
export envelope and lossless typed CSV fields. No command parses transcripts,\n\
migrates the ledger, or reaches the network. Open the desktop app to update history.\n\
Queries have a 10-second deadline and 8 MiB output limit; statusline has 250 ms."
}

/// Reject malformed requests before opening a ledger, rather than silently
/// widening a report when a date or limit was misspelled.
fn validate_arguments(command: &str, args: &[String]) -> Result<()> {
    let report = if command == "export" {
        flag_value(args, "--report")?.unwrap_or_else(|| "report".into())
    } else {
        command.to_owned()
    };
    if QueryKind::parse(&report).is_none() && report != "verify" {
        bail!("unknown report '{report}'");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        if !seen.insert(flag) {
            bail!("duplicate option '{flag}'");
        }
        let takes_value = match flag {
            "--format" | "--schema-version" => true,
            "--report" if command == "export" => true,
            "--from" | "--to"
                if matches!(
                    report.as_str(),
                    "report"
                        | "models"
                        | "projects"
                        | "sessions"
                        | "categories"
                        | "tools"
                        | "context"
                        | "findings"
                        | "metrics"
                        | "activity"
                ) =>
            {
                true
            }
            "--limit" if report == "sessions" => true,
            "--include-paths" if matches!(report.as_str(), "projects" | "diagnostics") => false,
            _ => bail!("unknown or unsupported option '{flag}' for {report}"),
        };
        if takes_value {
            let value = args
                .get(index + 1)
                .filter(|value| !value.starts_with('-'))
                .with_context(|| format!("{flag} needs a value"))?;
            if flag == "--limit" {
                let limit = value
                    .parse::<usize>()
                    .context("--limit must be a nonnegative integer")?;
                if limit > 50_000 {
                    bail!("--limit exceeds the 50000-session query limit");
                }
            }
            index += 1;
        }
        index += 1;
    }
    parse_window(args)?;
    Ok(())
}

fn statusline_text(value: &serde_json::Value) -> String {
    let tokens = value
        .pointer("/tokens/total_tokens")
        .and_then(serde_json::Value::as_i64);
    let mut output = format!(
        "Odometer · {} tokens today",
        tokens
            .map(|n| n.to_string())
            .unwrap_or_else(|| "unavailable".into())
    );
    if let Some(totals) = value
        .get("cost_by_currency")
        .and_then(serde_json::Value::as_object)
    {
        for (currency, amount) in totals {
            if let Some(amount) = amount.as_f64() {
                output.push_str(&format!(
                    " · {amount:.4} {}",
                    currency.replace(['\r', '\n', '\u{1b}'], " ")
                ));
            }
        }
    }
    let providers = value.get("providers").and_then(serde_json::Value::as_array);
    if value
        .get("pricing_complete")
        .and_then(serde_json::Value::as_bool)
        == Some(false)
        || providers.is_some_and(|providers| {
            providers.iter().any(|provider| {
                provider
                    .get("pricing")
                    .is_none_or(serde_json::Value::is_null)
                    || provider
                        .pointer("/pricing/plan/unpriced_models")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|models| !models.is_empty())
                    || provider
                        .pointer("/pricing/plan/by_model")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|models| {
                            models.iter().any(|model| {
                                model.get("basis").and_then(serde_json::Value::as_str)
                                    == Some("unavailable")
                            })
                        })
            })
        })
    {
        output.push_str(" · incomplete pricing");
    }
    if providers.is_some_and(|providers| {
        providers.iter().any(|provider| {
            provider
                .pointer("/pricing/plan/missing_models")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|models| !models.is_empty())
        })
    }) {
        output.push_str(" · estimated");
    }
    output
}

fn parse_format(args: &[String]) -> Result<Format> {
    match flag_value(args, "--format")? {
        None => Ok(Format::Text),
        Some(value) => match value.as_str() {
            "json" => Ok(Format::Json),
            "csv" => Ok(Format::Csv),
            "text" => Ok(Format::Text),
            "markdown" => Ok(Format::Json),
            other => bail!("unknown --format '{other}'; expected json, csv, markdown, or text"),
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
    if date.format("%Y-%m-%d").to_string() != value {
        bail!("'{value}' must use YYYY-MM-DD date format");
    }
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
    HistoryStore::open_read_only(&path, QueryControl::default())
        .context("could not open durable history; open the desktop app to prepare it")
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
        .and_then(|store| store.session_count().ok())
        .and_then(|count| usize::try_from(count).ok());
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
    let config = Config::load_read_only()?;
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
/// Only a registered, validated provider prefix establishes identity.
fn harness_resolver(_config: &Config) -> impl Fn(&str) -> String + '_ {
    |key: &str| {
        crate::query::provider_for_key(key)
            .map(|provider| provider.as_str().to_owned())
            .unwrap_or_default()
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
                    csv_field(&usage.model),
                    csv_field(&usage.harness),
                    usage.tokens.total_tokens,
                    usage.tokens.input_tokens,
                    usage.tokens.cached_input_tokens,
                    usage.tokens.output_tokens,
                    usage.tokens.reasoning_output_tokens,
                    usage
                        .cost
                        .map(|cost| format!("{cost:.6}"))
                        .unwrap_or_default(),
                    csv_field(&usage.currency)
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
            for converted in &report.converted {
                // Shown beneath the original, never instead of it, and
                // always with the rate's date and where it came from —
                // Odometer never fetched it (issue #42).
                out.push_str(&format!(
                    "  = {:.4} {} (from {} at {:.6}, {} as of {})
",
                    converted.amount,
                    converted.target_currency,
                    converted.from_currency,
                    converted.rate,
                    converted.source,
                    converted.as_of.format("%Y-%m-%d")
                ));
            }
            out
        }
    })
}

/// Default listing length. Bounded because an unbounded list over a
/// thousands-session corpus is unusable in a terminal; `--limit 0` shows
/// everything for a script that wants it.
const DEFAULT_SESSION_LIMIT: usize = 20;

fn run_verify(format: Format) -> Result<String> {
    // The running binary verifies itself: what a client would launch is
    // exactly what gets launched, so the check cannot pass against a
    // different build than the one installed (issue #57).
    let executable = std::env::current_exe().context("could not locate this executable")?;
    let report = crate::verify::verify(&executable, Utc::now());
    Ok(match format {
        Format::Json => serde_json::to_string_pretty(&report)?,
        Format::Csv => {
            let mut out = String::from(
                "check,status,detail
",
            );
            for check in &report.checks {
                out.push_str(&format!(
                    "{},{:?},{}
",
                    check.id,
                    check.status,
                    csv_field(&check.detail)
                ));
            }
            out
        }
        Format::Text => crate::verify::render(&report),
    })
}

fn run_activity(args: &[String], format: Format) -> Result<String> {
    let store = open_ledger()?;
    // The machine's own zone: a heatmap answers "when do I work", and
    // hour-of-day in UTC answers a question nobody asked.
    let offset = *Local::now().offset();
    activity_from(&store, offset, args, format)
}

/// The testable half of `activity`, taking the ledger and zone.
pub fn activity_from(
    store: &HistoryStore,
    offset: chrono::FixedOffset,
    args: &[String],
    format: Format,
) -> Result<String> {
    let (from, to) = parse_window(args)?;
    let heatmap = crate::query::activity_heatmap(store, offset, from, to)?;

    Ok(match format {
        Format::Json => serde_json::to_string_pretty(&heatmap)?,
        Format::Csv => {
            let mut out = String::from(
                "date,hour,total_tokens,sessions
",
            );
            for cell in &heatmap.cells {
                out.push_str(&format!(
                    "{},{},{},{}
",
                    cell.date, cell.hour, cell.total_tokens, cell.sessions
                ));
            }
            out
        }
        Format::Text => {
            let Some(peak) = heatmap.peak_total_tokens else {
                return Ok("no activity in this window
"
                .to_string());
            };
            // One row per day, 24 cells, shaded by share of the busiest
            // hour. Scaling by the peak rather than an absolute threshold is
            // what makes a quiet week readable instead of uniformly blank.
            let mut by_day: std::collections::BTreeMap<&str, [u64; 24]> =
                std::collections::BTreeMap::new();
            for cell in &heatmap.cells {
                let row = by_day.entry(cell.date.as_str()).or_insert([0; 24]);
                row[cell.hour.min(23) as usize] += cell.total_tokens;
            }
            const SHADES: [char; 5] = [' ', '.', ':', '*', '#'];
            let mut out = String::from(
                "       0h                      12h                     23h
",
            );
            for (date, hours) in by_day {
                out.push_str(&format!("{date}  "));
                for tokens in hours {
                    let shade = if tokens == 0 {
                        0
                    } else {
                        // At least one step for any activity: an hour with
                        // real work must never render as empty.
                        1 + ((tokens as f64 / peak as f64) * 3.0).round() as usize
                    };
                    out.push(SHADES[shade.min(SHADES.len() - 1)]);
                }
                out.push('\n');
            }
            out.push_str(&format!(
                "busiest hour: {peak} tokens
"
            ));
            out
        }
    })
}

fn run_sessions(args: &[String], format: Format) -> Result<String> {
    let store = open_ledger()?;
    let rates = load_rates();
    let config = Config::load_read_only()?;
    sessions_from(&store, &rates, &config, args, format)
}

/// The testable half of `sessions`.
pub fn sessions_from(
    store: &HistoryStore,
    rates: &RateCard,
    config: &Config,
    args: &[String],
    format: Format,
) -> Result<String> {
    let (from, to) = parse_window(args)?;
    let limit = match flag_value(args, "--limit")? {
        None => Some(DEFAULT_SESSION_LIMIT),
        Some(raw) => {
            let parsed: usize = raw
                .parse()
                .with_context(|| format!("--limit needs a whole number, got '{raw}'"))?;
            // 0 means "no limit" rather than "show nothing", which is the
            // only reading that is ever useful.
            (parsed > 0).then_some(parsed)
        }
    };
    let report = crate::query::session_report(
        store,
        rates,
        harness_resolver(config),
        from,
        to,
        limit,
        Utc::now(),
    )?;

    Ok(match format {
        Format::Json => serde_json::to_string_pretty(&report)?,
        Format::Csv => {
            let mut out = String::from(
                "session_key,harness,total_tokens,input,output,cost,currency
",
            );
            for session in &report.sessions {
                out.push_str(&format!(
                    "{},{},{},{},{},{},{}
",
                    session.session_key,
                    session.harness,
                    session.tokens.total_tokens,
                    session.tokens.input_tokens,
                    session.tokens.output_tokens,
                    session
                        .cost
                        .map(|cost| format!("{cost:.6}"))
                        .unwrap_or_default(),
                    session.currency
                ));
            }
            out
        }
        Format::Text => {
            let mut out = String::new();
            for session in &report.sessions {
                out.push_str(&format!(
                    "{:<44} {:>14} tokens",
                    session.session_key, session.tokens.total_tokens
                ));
                match session.cost {
                    Some(cost) => out.push_str(&format!("  {cost:.4} {}", session.currency)),
                    None => out.push_str("  (unpriced)"),
                }
                if !session.unpriced_models.is_empty() {
                    out.push_str(&format!(
                        "  [floor; unpriced: {}]",
                        session.unpriced_models.join(", ")
                    ));
                }
                out.push('\n');
            }
            if out.is_empty() {
                out.push_str(
                    "no session activity in this window
",
                );
            }
            if let Some(limit) = report.truncated_to {
                // Never silent: a truncated list that looks complete is how
                // a total gets read as the whole picture.
                out.push_str(&format!(
                    "(showing the {limit} heaviest; pass --limit 0 for all)
"
                ));
            }
            out
        }
    })
}

fn run_mirrors(format: Format) -> Result<String> {
    render_mirrors(&open_ledger()?, format)
}

/// The testable half of `mirrors`.
pub fn render_mirrors(store: &HistoryStore, format: Format) -> Result<String> {
    let groups = store.mirrored_session_groups()?;
    Ok(match format {
        Format::Json => serde_json::to_string_pretty(&groups)?,
        Format::Csv => {
            let mut out = String::from(
                "fingerprint,providers,session_key
",
            );
            for group in &groups {
                for key in &group.session_keys {
                    out.push_str(&format!(
                        "{},{},{}
",
                        group.fingerprint,
                        csv_field(&group.providers.join(" ")),
                        key
                    ));
                }
            }
            out
        }
        Format::Text => {
            if groups.is_empty() {
                // An explicit answer: "none found" is a result, and a blank
                // response reads as a broken command.
                return Ok("no mirrored sessions detected
"
                .to_string());
            }
            let mut out = format!(
                "{} group(s) of sessions look like the same run recorded by more than one tool.
                 Totals count each copy, so usage in these groups is double counted.
",
                groups.len()
            );
            for group in &groups {
                out.push_str(&format!(
                    "  {} ({})
",
                    group.fingerprint,
                    group.providers.join(", ")
                ));
                for key in &group.session_keys {
                    out.push_str(&format!(
                        "    {key}
"
                    ));
                }
            }
            out
        }
    })
}

fn run_metrics(args: &[String], format: Format) -> Result<String> {
    let store = open_ledger()?;
    let rates = load_rates();
    let config = Config::load_read_only()?;
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
    let config = Config::load_read_only()?;
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
                if !project.pricing_complete {
                    out.push_str("  [incomplete pricing; subtotal only]");
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
    report_output::csv_field(value)
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
            converted: Vec::new(),
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
        assert_eq!(resolve("missing-prefix"), "");
        assert_eq!(resolve("codex:"), "");
        assert_eq!(resolve("unknown:xyz"), "");
    }

    #[test]
    fn bad_options_are_rejected_before_opening_any_history() {
        for (command, raw) in [
            ("report", vec!["--form", "json"]),
            (
                "report",
                vec!["--from", "2026-08-01", "--from", "2026-08-02"],
            ),
            ("report", vec!["--from"]),
            ("report", vec!["--from", "2026-8-1"]),
            ("report", vec!["--from", "2026-08-02", "--to", "2026-08-01"]),
            ("statusline", vec!["--from", "2026-08-01"]),
            ("tools", vec!["--include-paths"]),
            ("sessions", vec!["--limit", "-1"]),
            ("sessions", vec!["--limit", "50001"]),
            ("export", vec!["--report", "unknown"]),
        ] {
            let args = raw.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(
                validate_arguments(command, &args).is_err(),
                "{command} {args:?}"
            );
        }
        assert!(validate_arguments("sessions", &["--limit".into(), "0".into()]).is_ok());
        assert!(validate_arguments(
            "export",
            &[
                "--report".into(),
                "categories".into(),
                "--format".into(),
                "markdown".into()
            ]
        )
        .is_ok());
    }

    #[test]
    fn export_schema_rejects_unknown_versions_without_reading_history() {
        assert!(run("export", &["--schema-version".into(), "3".into()])
            .unwrap_err()
            .to_string()
            .contains("unsupported"));
        assert!(run("report", &["--format".into(), "yaml".into()]).is_err());
        assert!(run("help", &[]).unwrap().contains("--schema-version"));
    }

    #[test]
    fn statusline_keeps_currencies_and_incomplete_pricing_visible() {
        let rendered = statusline_text(&serde_json::json!({
            "tokens": {"total_tokens": 1200},
            "cost_by_currency": {"credits": 2.0, "USD": 0.25},
            "providers": [{"pricing": null}, {"pricing": {"plan": {
                "unpriced_models": [], "missing_models": ["future-model"], "by_model": []
            }}}]
        }));
        assert!(rendered.contains("1200 tokens today"));
        assert!(rendered.contains("2.0000 credits"));
        assert!(rendered.contains("0.2500 USD"));
        assert!(rendered.contains("incomplete pricing"));
        assert!(rendered.contains("estimated"));
        assert_eq!(rendered.lines().count(), 1);
    }

    #[test]
    fn export_envelope_preserves_pricing_provenance_without_changing_v1() {
        let native: serde_json::Value =
            serde_json::from_str(&render_report(&sample_report(), Format::Json).unwrap()).unwrap();
        let exported = report_output::envelope("models", native.clone());
        assert_eq!(exported["schema_version"], 2);
        assert_eq!(exported["data"], native);
        assert_eq!(
            exported["data"]["by_model"][1]["cost"],
            serde_json::Value::Null
        );
        assert_eq!(exported["data"]["by_model"][1]["basis"], "unavailable");
        let csv = report_output::csv("models", &exported);
        assert!(csv.contains("/data/by_model/1/cost,null,null"));
        assert!(csv.contains("/data/by_model/1/basis,string,"));
        assert!(report_output::markdown("models", &exported, 2).contains("/data/by_model/1/cost"));
    }
}
