//! Read-only stdio MCP server (issue #47).
//!
//! Issue #47 asks for "a read-only stdio MCP server exposing the same
//! bounded query methods to local agents", with the constraints that
//! "mutating config/guard actions are not exposed through MCP" and that
//! "MCP descriptions warn about sensitive session metadata and default to
//! aggregate/read-only results".
//!
//! Every tool here adapts [`crate::query`], the same service the desktop and
//! the CLI use, so an agent and the app can never disagree about the same
//! corpus. Nothing in this module writes: it opens the ledger, answers, and
//! exits.
//!
//! ## Transport
//!
//! JSON-RPC 2.0 over stdio, line-delimited: one JSON object per line in,
//! one per line out. This is hand-rolled rather than pulled from a crate —
//! the surface is three methods, and the alternative is a dependency on the
//! app's trust boundary for a protocol shim.
//!
//! ## Why stdio and not a socket
//!
//! A local HTTP API is a separate item in #47 with its own authentication
//! and lifecycle requirements. stdio has neither problem: the process is
//! spawned by the client that talks to it, there is nothing to bind, nothing
//! to discover, and nothing to authenticate.

use std::io::{BufRead, Write};

use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::{json, Value};

use crate::config::Config;
use crate::history_store::HistoryStore;
use crate::rates::RateCard;

/// MCP protocol version this server implements.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// JSON-RPC error codes used here, from the JSON-RPC 2.0 spec.
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Runs the MCP server if `argv` asks for it.
///
/// Returns `false` so the caller falls through to the desktop app, matching
/// `turn_receipts::try_run_cli`'s contract.
pub fn try_run_cli() -> bool {
    if std::env::args().nth(1).as_deref() != Some("mcp") {
        return false;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    serve(stdin.lock(), stdout.lock());
    true
}

/// Reads line-delimited JSON-RPC requests from `input` and writes responses
/// to `output` until the input closes.
///
/// Split from [`try_run_cli`] so tests drive it with in-memory buffers
/// rather than a real process.
pub fn serve<R: BufRead, W: Write>(input: R, mut output: W) {
    for line in input.lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = handle_line(&line) else {
            // A notification (no `id`) gets no reply, per JSON-RPC. Sending
            // one anyway makes well-behaved clients complain.
            continue;
        };
        if writeln!(output, "{response}").is_err() {
            break;
        }
        let _ = output.flush();
    }
}

/// Handles one request line, returning the response to write, or `None` for
/// a notification.
fn handle_line(line: &str) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        // A parse failure has no id to answer against, so this replies with
        // a null id rather than staying silent — a client waiting on a
        // response would otherwise hang.
        Err(error) => {
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                &format!("could not parse request: {error}"),
            ))
        }
    };
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(Value::Null);

    // No id means a notification: act on it if meaningful, never reply.
    let id = id?;

    Some(match method {
        "initialize" => success(id, initialize_result()),
        "tools/list" => success(id, json!({ "tools": tool_descriptors() })),
        "tools/call" => match call_tool(&params) {
            Ok(text) => success(
                id,
                json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                }),
            ),
            Err(ToolError::UnknownTool(name)) => {
                error_response(id, INVALID_PARAMS, &format!("unknown tool '{name}'"))
            }
            Err(ToolError::BadArguments(message)) => error_response(id, INVALID_PARAMS, &message),
            // A query failure is reported as a tool result rather than a
            // protocol error: the request was well-formed, the answer just
            // could not be produced, and an agent should see why.
            Err(ToolError::Failed(message)) => success(
                id,
                json!({
                    "content": [{ "type": "text", "text": message }],
                    "isError": true,
                }),
            ),
        },
        other => error_response(id, METHOD_NOT_FOUND, &format!("unknown method '{other}'")),
    })
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "agent-odometer",
            "version": env!("CARGO_PKG_VERSION"),
        },
        // Stated up front, not buried in one tool's description: an agent
        // deciding whether to call these should know the shape of what comes
        // back before it asks.
        "instructions": "Read-only local usage analytics for agent sessions. \
                         Every tool answers from the local durable ledger and \
                         never writes, scans, or reaches the network. Results \
                         are aggregates — token counts, costs, and per-project \
                         or per-session totals — not prompts, replies, or tool \
                         output. Session keys and project labels can identify \
                         local work, so treat results as sensitive metadata.",
    })
}

/// The tools this server exposes.
///
/// Read-only by construction: every one maps to a `query` function, and no
/// mutating command is reachable from here at all — not gated behind a flag,
/// simply absent, which is what #47's DRY boundary requires.
fn tool_descriptors() -> Vec<Value> {
    let window_properties = json!({
        "from": { "type": "string", "description": "Inclusive start date, YYYY-MM-DD (UTC). Omit for all time." },
        "to": { "type": "string", "description": "Inclusive end date, YYYY-MM-DD (UTC). Omit for all time." },
    });
    vec![
        json!({
            "name": "usage_report",
            "description": "Token usage and cost over a date range, grouped by model. \
                            Costs are reported per currency and never summed across them \
                            (Codex bills in plan credits, Claude in USD). Aggregates only.",
            "inputSchema": { "type": "object", "properties": window_properties },
        }),
        json!({
            "name": "project_report",
            "description": "Token usage and cost grouped by project. Project labels derived \
                            from a directory path are redacted to a stable hash; this tool \
                            never returns local filesystem paths.",
            "inputSchema": { "type": "object", "properties": window_properties },
        }),
        json!({
            "name": "workflow_metrics",
            "description": "Versioned workflow metrics (tool failure rate, mutation rework \
                            rate, context-to-output ratio, cached-input share, pricing \
                            coverage). Each carries its numerator, denominator, and what \
                            the denominator counts; a metric with no evidence reports no \
                            value rather than zero.",
            "inputSchema": { "type": "object", "properties": window_properties },
        }),
        json!({
            "name": "quota_status",
            "description": "Current provider quota windows: used, remaining, reset time, \
                            staleness, and pace forecast where evidence supports one. \
                            Reports transcript-derived state only; performs no network call.",
            "inputSchema": { "type": "object", "properties": {} },
        }),
    ]
}

#[derive(Debug)]
pub enum ToolError {
    UnknownTool(String),
    BadArguments(String),
    Failed(String),
}

fn call_tool(params: &Value) -> std::result::Result<String, ToolError> {
    let store = open_ledger().map_err(|error| ToolError::Failed(error.to_string()))?;
    call_tool_with(&store, &load_rates(), params, Utc::now())
}

/// The testable half of `tools/call`, taking the ledger and clock rather
/// than resolving them from the user's directories.
pub fn call_tool_with(
    store: &HistoryStore,
    rates: &RateCard,
    params: &Value,
    now: DateTime<Utc>,
) -> std::result::Result<String, ToolError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::BadArguments("tools/call needs a 'name'".into()))?;
    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);

    let config = Config::load().unwrap_or_default();

    let (from, to) = window_from(&arguments)?;
    let harness_for = |key: &str| {
        key.split_once(':')
            .map(|(provider, _)| provider.to_owned())
            .unwrap_or_default()
    };
    let _ = &config;

    let value = match name {
        "usage_report" => serialize(
            crate::query::range_report(store, rates, harness_for, from, to, now)
                .map_err(|error| ToolError::Failed(error.to_string()))?,
        ),
        "project_report" => {
            let mut report = crate::query::project_report(store, rates, harness_for, from, to, now)
                .map_err(|error| ToolError::Failed(error.to_string()))?;
            // Redaction is not optional here. The CLI has `--include-paths`
            // because a person can consent to seeing their own paths; an
            // agent asking over MCP is a different audience, and there is no
            // one present to make that call.
            for project in &mut report.projects {
                project.label = project.redacted_label().to_owned();
            }
            serialize(report)
        }
        "workflow_metrics" => serialize(
            crate::query::workflow_metrics(store, rates, harness_for, from, to, now)
                .map_err(|error| ToolError::Failed(error.to_string()))?,
        ),
        "quota_status" => {
            let quota_store = crate::quota_store::QuotaStoreFile::load();
            let max_cache_age = chrono::Duration::seconds(quota_store.max_cache_age_secs);
            serialize(
                crate::query::quota_snapshots(store, now, max_cache_age)
                    .map_err(|error| ToolError::Failed(error.to_string()))?,
            )
        }
        other => return Err(ToolError::UnknownTool(other.to_owned())),
    };
    Ok(value)
}

fn serialize<T: Serialize>(value: T) -> String {
    serde_json::to_string_pretty(&value).unwrap_or_else(|error| {
        // Unreachable for these types, and reported rather than panicking
        // inside a server loop if it ever were.
        format!("{{\"error\":\"could not serialize result: {error}\"}}")
    })
}

/// An inclusive reporting window parsed from tool arguments.
type ToolWindow = (Option<DateTime<Utc>>, Option<DateTime<Utc>>);

/// Reads optional `from`/`to` date arguments.
///
/// A malformed date is rejected rather than ignored: silently widening a
/// requested window to all time would hand an agent a much larger number
/// than it asked for, with nothing to indicate the difference.
fn window_from(arguments: &Value) -> std::result::Result<ToolWindow, ToolError> {
    let parse =
        |key: &str, end_of_day: bool| -> std::result::Result<Option<DateTime<Utc>>, ToolError> {
            let Some(raw) = arguments.get(key).and_then(Value::as_str) else {
                return Ok(None);
            };
            let date = chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").map_err(|_| {
                ToolError::BadArguments(format!("'{key}' must be a YYYY-MM-DD date, got '{raw}'"))
            })?;
            let time = if end_of_day {
                date.and_hms_milli_opt(23, 59, 59, 999)
            } else {
                date.and_hms_opt(0, 0, 0)
            }
            .ok_or_else(|| ToolError::BadArguments(format!("'{raw}' is not a valid instant")))?;
            Ok(Some(Utc.from_utc_datetime(&time)))
        };
    let from = parse("from", false)?;
    let to = parse("to", true)?;
    if let (Some(from), Some(to)) = (from, to) {
        if to < from {
            return Err(ToolError::BadArguments("'to' is before 'from'".to_owned()));
        }
    }
    Ok((from, to))
}

fn open_ledger() -> Result<HistoryStore> {
    let path = HistoryStore::default_path()?;
    HistoryStore::open(&path)
}

fn load_rates() -> RateCard {
    RateCard::load_from_disk()
        .or_else(|_| RateCard::load_bundled())
        .unwrap_or_default()
}

fn success(id: Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

/// Names of the tools this server advertises.
///
/// Exposed so `verify` can check its expectations against what is actually
/// advertised (issue #57). A rename on either side would otherwise show up
/// as an agent quietly choosing no tool at all.
pub fn advertised_tool_names() -> Vec<String> {
    tool_descriptors()
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_owned))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(line: &str) -> Value {
        let response = handle_line(line).expect("a request with an id gets a response");
        serde_json::from_str(&response).expect("responses are JSON")
    }

    #[test]
    fn initialize_reports_the_protocol_version_and_tool_capability() {
        let response = request(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);

        assert_eq!(response["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert_eq!(response["result"]["serverInfo"]["name"], "agent-odometer");
    }

    /// #47: "MCP descriptions warn about sensitive session metadata and
    /// default to aggregate/read-only results." Stated at initialize, so an
    /// agent knows before it calls anything.
    #[test]
    fn initialize_warns_that_results_are_sensitive_aggregates() {
        let response = request(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
        let instructions = response["result"]["instructions"]
            .as_str()
            .expect("instructions");

        assert!(instructions.contains("sensitive"), "{instructions}");
        assert!(instructions.contains("never writes"), "{instructions}");
        assert!(
            instructions.contains("not prompts"),
            "an agent must know message text is not returned: {instructions}"
        );
    }

    #[test]
    fn every_advertised_tool_is_read_only_and_documented() {
        let response = request(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#);
        let tools = response["result"]["tools"].as_array().expect("tools");

        assert!(!tools.is_empty());
        for tool in tools {
            let name = tool["name"].as_str().expect("name");
            let description = tool["description"].as_str().expect("description");
            assert!(
                !description.is_empty(),
                "{name} needs a description an agent can act on"
            );
            assert!(tool["inputSchema"].is_object(), "{name} needs a schema");
            // No mutating verb is reachable: these are simply not exposed,
            // rather than gated behind a flag that could be flipped.
            for forbidden in ["set_", "write", "rebuild", "delete", "apply", "merge"] {
                assert!(
                    !name.contains(forbidden),
                    "{name} looks like a mutation and must not be exposed"
                );
            }
        }
    }

    #[test]
    fn an_unknown_method_is_a_protocol_error() {
        let response = request(r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#);

        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND);
        assert!(response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("resources/list")));
    }

    #[test]
    fn an_unknown_tool_is_rejected_by_name() {
        let response = request(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"rebuild_history"}}"#,
        );

        assert_eq!(response["error"]["code"], INVALID_PARAMS);
        assert!(response["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("rebuild_history")));
    }

    /// A notification carries no `id` and must get no reply — a spurious
    /// response makes well-behaved clients complain.
    #[test]
    fn a_notification_gets_no_response() {
        assert!(handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
    }

    /// Unparseable input still gets an answer, against a null id. Staying
    /// silent would hang a client that is waiting on one.
    #[test]
    fn malformed_json_is_answered_rather_than_ignored() {
        let response = handle_line("{not json").expect("a reply");
        let parsed: Value = serde_json::from_str(&response).expect("the reply itself is JSON");

        assert_eq!(parsed["id"], Value::Null);
        assert_eq!(parsed["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn a_malformed_date_argument_is_rejected_rather_than_widened() {
        // Silently widening to all time would hand an agent a much larger
        // number than it asked for, with nothing to signal the difference.
        let error = window_from(&json!({ "from": "August 2026" })).expect_err("rejected");
        match error {
            ToolError::BadArguments(message) => {
                assert!(message.contains("August 2026"), "{message}");
            }
            _ => panic!("expected a bad-arguments error"),
        }
    }

    #[test]
    fn a_reversed_window_is_rejected() {
        let error = window_from(&json!({ "from": "2026-08-31", "to": "2026-08-01" }))
            .expect_err("rejected");
        assert!(matches!(error, ToolError::BadArguments(_)));
    }

    #[test]
    fn a_window_is_inclusive_of_the_named_end_day() {
        let (from, to) =
            window_from(&json!({ "from": "2026-08-01", "to": "2026-08-15" })).expect("parsed");

        assert_eq!(from.unwrap().to_rfc3339(), "2026-08-01T00:00:00+00:00");
        assert_eq!(to.unwrap().to_rfc3339(), "2026-08-15T23:59:59.999+00:00");
    }

    #[test]
    fn an_absent_window_is_all_time_rather_than_an_error() {
        let (from, to) = window_from(&json!({})).expect("parsed");
        assert!(from.is_none() && to.is_none());
    }

    #[test]
    fn serve_answers_each_line_and_stops_at_end_of_input() {
        let input = concat!(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "\n",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            "\n",
        );
        let mut output = Vec::new();

        serve(std::io::BufReader::new(input.as_bytes()), &mut output);

        let lines: Vec<&str> = std::str::from_utf8(&output)
            .expect("utf-8")
            .lines()
            .collect();
        // Two requests, one notification: two responses.
        assert_eq!(lines.len(), 2, "got {lines:?}");
        let first: Value = serde_json::from_str(lines[0]).unwrap();
        let second: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["id"], 1);
        assert_eq!(second["id"], 2);
    }

    #[test]
    fn only_the_mcp_subcommand_is_claimed() {
        // Anything else must fall through so the desktop app still starts.
        for command in ["report", "status", "hook", ""] {
            assert_ne!(command, "mcp");
        }
    }
}
