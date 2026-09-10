//! Read-only stdio MCP adapter over the shared bounded query service.
//! Input lines, output responses, active queries, and ledger work are bounded.
//! The input thread remains available for cancellation while workers query.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::config::Config;
use crate::headless::{self, QueryKind, Request};
use crate::history_store::HistoryStore;
use crate::query_control::QueryControl;
use crate::rates::RateCard;

const PROTOCOL_VERSION: &str = "2025-06-18";
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const SERVER_BUSY: i64 = -32001;
pub const MAX_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CONCURRENT_QUERIES: usize = 2;

pub fn try_run_cli() -> bool {
    if std::env::args().nth(1).as_deref() != Some("mcp") {
        return false;
    }
    serve(std::io::stdin().lock(), std::io::stdout());
    true
}

/// Drain accepted work on EOF. At most two queries run, with no pending
/// queue; excess calls receive an explicit busy error and may be retried.
pub fn serve<R: BufRead, W: Write + Send>(input: R, output: W) {
    serve_with_executor(input, output, execute_local);
}

#[derive(Debug, Clone)]
struct PreparedCall {
    kind: QueryKind,
    request: Request,
    envelope_key: Option<&'static str>,
}

#[derive(Debug)]
pub enum ToolError {
    UnknownTool(String),
    BadArguments(String),
    Failed(String),
}

impl ToolError {
    fn failed(error: impl std::fmt::Display) -> Self {
        Self::Failed(error.to_string())
    }
}

fn serve_with_executor<R, W, F>(mut input: R, output: W, executor: F)
where
    R: BufRead,
    W: Write + Send,
    F: Fn(PreparedCall, QueryControl) -> Result<String, ToolError> + Sync,
{
    let output = Mutex::new(output);
    let active = Arc::new(Mutex::new(HashMap::<String, QueryControl>::new()));
    std::thread::scope(|scope| {
        let mut workers: Vec<std::thread::ScopedJoinHandle<'_, ()>> = Vec::new();
        loop {
            // Finished workers are reaped continuously, so even an unlimited
            // number of sequential requests cannot grow the handle list.
            let mut index = 0;
            while index < workers.len() {
                if workers[index].is_finished() {
                    let _ = workers.swap_remove(index).join();
                } else {
                    index += 1;
                }
            }
            let line = match read_bounded_line(&mut input) {
                Ok(Some(line)) => line,
                Ok(None) | Err(_) => break,
            };
            let line = match line {
                Ok(line) => line,
                Err(message) => {
                    if !write_response(
                        &output,
                        &error_response(Value::Null, INVALID_REQUEST, message),
                    ) {
                        cancel_all(&active);
                        break;
                    }
                    continue;
                }
            };
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let request: Value = match serde_json::from_slice(&line) {
                Ok(request) => request,
                Err(_) => {
                    if !write_response(
                        &output,
                        &error_response(Value::Null, PARSE_ERROR, "invalid JSON"),
                    ) {
                        cancel_all(&active);
                        break;
                    }
                    continue;
                }
            };
            let valid_id = request
                .get("id")
                .is_none_or(|id| id.is_null() || id.is_string() || id.is_number());
            let valid = request.is_object()
                && request.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
                && request.get("method").and_then(Value::as_str).is_some()
                && request
                    .get("params")
                    .is_none_or(|value| value.is_object() || value.is_array())
                && valid_id;
            if !valid {
                let id = if valid_id {
                    request.get("id").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                if !write_response(
                    &output,
                    &error_response(id, INVALID_REQUEST, "invalid JSON-RPC request"),
                ) {
                    cancel_all(&active);
                    break;
                }
                continue;
            }
            let method = request["method"].as_str().expect("validated method");
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            let Some(id) = request.get("id").cloned() else {
                if method == "notifications/cancelled" {
                    if let Some(cancel_id) = params.get("requestId") {
                        if let Some(control) = active
                            .lock()
                            .expect("active queries")
                            .get(&cancel_id.to_string())
                        {
                            control.cancel();
                        }
                    }
                }
                continue;
            };
            let immediate = match method {
                "initialize" => Some(success(id.clone(), initialize_result())),
                "ping" => Some(success(id.clone(), json!({}))),
                "tools/list" => Some(success(id.clone(), json!({"tools": tool_descriptors()}))),
                "tools/call" => match prepare_call(&params) {
                    Err(error) => Some(tool_response(id.clone(), Err(error))),
                    Ok(call) => {
                        let key = id.to_string();
                        let mut active_guard = active.lock().expect("active queries");
                        if active_guard.contains_key(&key) {
                            Some(error_response(
                                id.clone(),
                                INVALID_REQUEST,
                                "request id is already active",
                            ))
                        } else if active_guard.len() >= MAX_CONCURRENT_QUERIES {
                            Some(error_response(
                                id.clone(),
                                SERVER_BUSY,
                                "query capacity reached; retry after an active query completes",
                            ))
                        } else {
                            let control = if call.kind == QueryKind::Statusline {
                                QueryControl::with_timeout(std::time::Duration::from_millis(250))
                            } else {
                                QueryControl::default()
                            };
                            active_guard.insert(key.clone(), control.clone());
                            drop(active_guard);
                            let active = Arc::clone(&active);
                            let executor = &executor;
                            let output = &output;
                            workers.push(scope.spawn(move || {
                                let limit = control.max_output_bytes().min(MAX_OUTPUT_BYTES);
                                let result = control.check().map_err(ToolError::failed)
                                    .and_then(|()| executor(call, control.clone()))
                                    .and_then(|result| control.check().map(|()| result).map_err(ToolError::failed));
                                let mut response = tool_response(id.clone(), result);
                                if response.len() > limit {
                                    response = tool_response(id, Err(ToolError::Failed("query output exceeds the response limit; request a narrower range".into())));
                                }
                                if !write_response(output, &response) {
                                    cancel_all(&active);
                                }
                                active.lock().expect("active queries").remove(&key);
                            }));
                            None
                        }
                    }
                },
                _ => Some(error_response(
                    id.clone(),
                    METHOD_NOT_FOUND,
                    "unknown method",
                )),
            };
            if immediate.is_some_and(|response| !write_response(&output, &response)) {
                cancel_all(&active);
                break;
            }
        }
        for worker in workers {
            let _ = worker.join();
        }
    });
}

type BoundedLine = Result<Vec<u8>, &'static str>;

/// Discard an oversized line through its delimiter without allocating it.
/// A following valid line remains usable; a huge line cannot become a queue.
fn read_bounded_line<R: BufRead>(input: &mut R) -> std::io::Result<Option<BoundedLine>> {
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return Ok(if oversized {
                Some(Err("request exceeds 64 KiB line limit"))
            } else if line.is_empty() {
                None
            } else {
                Some(Ok(line))
            });
        }
        let end = available.iter().position(|byte| *byte == b'\n');
        let consumed = end.map_or(available.len(), |index| index + 1);
        let data = &available[..end.unwrap_or(consumed)];
        if !oversized {
            if line.len().saturating_add(data.len()) > MAX_INPUT_BYTES {
                oversized = true;
                line.clear();
            } else {
                line.extend_from_slice(data);
            }
        }
        input.consume(consumed);
        if end.is_some() {
            return Ok(Some(if oversized {
                Err("request exceeds 64 KiB line limit")
            } else {
                Ok(line)
            }));
        }
    }
}

fn cancel_all(active: &Mutex<HashMap<String, QueryControl>>) {
    for control in active.lock().expect("active queries").values() {
        control.cancel();
    }
}

fn write_response<W: Write>(output: &Mutex<W>, response: &str) -> bool {
    let mut output = output.lock().expect("response writer");
    writeln!(output, "{response}")
        .and_then(|()| output.flush())
        .is_ok()
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": "agent-odometer", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "Read-only local usage analytics. Every tool answers from the durable ledger and never changes application records or settings, scans transcripts, or reaches the network. SQLite may maintain WAL coordination sidecars. Results are aggregates, not prompts, replies, or tool output. Session keys and project labels are sensitive metadata. Project paths are always redacted. At most two queries run concurrently; requests are limited to 64 KiB and responses to 8 MiB. Cancel running work with notifications/cancelled and requestId. Activity and statusline use UTC.",
    })
}

fn tool_specs() -> &'static [(&'static str, QueryKind, &'static str)] {
    &[
        ("usage_report", QueryKind::Report, "Token usage and cost by model, with separate currency totals and pricing provenance. Aggregates only."),
        ("model_report", QueryKind::Models, "Model usage, tier-aware cost, and pricing provenance. Aggregates only."),
        ("project_report", QueryKind::Projects, "Usage and costs by project. Local filesystem paths are always redacted to stable identifiers; labels can still identify sensitive work."),
        ("workflow_metrics", QueryKind::Metrics, "Workflow ratios with their numerators, denominators, and coverage. Absent evidence is unavailable rather than zero."),
        ("session_report", QueryKind::Sessions, "Bounded usage by session, default 20 and maximum 1000 rows. Session keys are sensitive local metadata; no prompts, titles, paths, or transcript contents are returned."),
        ("activity_report", QueryKind::Activity, "Aggregate hourly usage in UTC over an optional date range."),
        ("category_report", QueryKind::Categories, "Aggregate usage and prices by task category, with separate currencies."),
        ("tools_report", QueryKind::Tools, "Aggregate tool calls, failures, and tool dimensions. No tool arguments or output."),
        ("context_report", QueryKind::Context, "Aggregate recorded context-source dimensions without message content."),
        ("findings_report", QueryKind::Findings, "Aggregate optimization finding counts by rule, severity, and provider; no evidence text or transcript content."),
        ("diagnostics_report", QueryKind::Diagnostics, "Read-only provider and ledger availability diagnostics. Local paths are redacted and no credentials or response bodies are returned."),
        ("quota_status", QueryKind::Quota, "Legacy quota snapshot array. Transcript-derived provider quotas and forecasts only; no network call."),
        ("quota_report", QueryKind::Quota, "Versioned quota snapshot envelope. Transcript-derived state with staleness and provenance; no network call."),
        ("mirrored_sessions", QueryKind::Mirrors, "Versioned mirrored-session metadata. Session keys can identify sensitive local work; usage is not silently deduplicated."),
        ("ledger_status", QueryKind::Status, "Read-only ledger availability, count, size, and rate-card version; no session metadata."),
        ("statusline", QueryKind::Statusline, "Small UTC-day usage summary read from durable aggregates without scanning transcripts."),
    ]
}

fn tool_descriptors() -> Vec<Value> {
    tool_specs().iter().map(|(name, kind, description)| {
        let mut properties = serde_json::Map::new();
        if kind.accepts_window() {
            properties.insert("from".into(), json!({"type":"string", "pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}$", "description":"Inclusive start date in UTC; omit for all time."}));
            properties.insert("to".into(), json!({"type":"string", "pattern":"^[0-9]{4}-[0-9]{2}-[0-9]{2}$", "description":"Inclusive end date in UTC; omit for all time."}));
        }
        if *kind == QueryKind::Sessions {
            properties.insert("limit".into(), json!({"type":"integer", "minimum":1, "maximum":1000, "default":20}));
        }
        json!({"name":name, "description":description, "inputSchema":{"type":"object", "properties":properties, "additionalProperties":false}, "annotations":{"readOnlyHint":true, "destructiveHint":false, "idempotentHint":true, "openWorldHint":false}})
    }).collect()
}

fn prepare_call(params: &Value) -> Result<PreparedCall, ToolError> {
    let params = params
        .as_object()
        .ok_or_else(|| ToolError::BadArguments("tools/call params must be an object".into()))?;
    if params
        .keys()
        .any(|key| !matches!(key.as_str(), "name" | "arguments" | "_meta"))
    {
        return Err(ToolError::BadArguments(
            "unknown tools/call parameter".into(),
        ));
    }
    if params.get("_meta").is_some_and(|value| !value.is_object()) {
        return Err(ToolError::BadArguments("'_meta' must be an object".into()));
    }
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::BadArguments("tools/call needs a string 'name'".into()))?;
    let kind = tool_specs()
        .iter()
        .find_map(|(tool, kind, _)| (*tool == name).then_some(*kind))
        .ok_or_else(|| ToolError::UnknownTool(name.into()))?;
    let mut request = Request::default();
    if let Some(arguments) = params.get("arguments") {
        let arguments = arguments
            .as_object()
            .ok_or_else(|| ToolError::BadArguments("'arguments' must be an object".into()))?;
        for (key, value) in arguments {
            match key.as_str() {
                "from" | "to" if kind.accepts_window() => {
                    let raw = value.as_str().ok_or_else(|| {
                        ToolError::BadArguments(format!("'{key}' must be a YYYY-MM-DD string"))
                    })?;
                    let parsed = headless::parse_date(raw, key == "to")
                        .map_err(|error| ToolError::BadArguments(error.to_string()))?;
                    if key == "from" {
                        request.from = Some(parsed);
                    } else {
                        request.to = Some(parsed);
                    }
                }
                "limit" if kind == QueryKind::Sessions => {
                    let limit = value
                        .as_u64()
                        .and_then(|value| usize::try_from(value).ok())
                        .ok_or_else(|| {
                            ToolError::BadArguments(
                                "'limit' must be an integer from 1 to 1000".into(),
                            )
                        })?;
                    request.limit = Some(limit);
                }
                _ => {
                    return Err(ToolError::BadArguments(format!(
                        "unsupported argument '{key}' for '{name}'"
                    )))
                }
            }
        }
    }
    request
        .validate_for(kind)
        .map_err(|error| ToolError::BadArguments(error.to_string()))?;
    Ok(PreparedCall {
        kind,
        request,
        envelope_key: match name {
            "quota_report" => Some("snapshots"),
            "mirrored_sessions" => Some("groups"),
            _ => None,
        },
    })
}

fn execute_local(call: PreparedCall, control: QueryControl) -> Result<String, ToolError> {
    // Validation happened before this point: invalid tools or arguments never
    // touch a user's ledger, config, or rate card.
    control.check().map_err(ToolError::failed)?;
    let path = HistoryStore::default_path().map_err(ToolError::failed)?;
    let store = HistoryStore::open_read_only(&path, control).map_err(|_| ToolError::Failed("durable history is unavailable; open the desktop app to prepare it, or retry when it is ready".into()))?;
    let rates = RateCard::load_from_disk()
        .or_else(|_| RateCard::load_bundled())
        .unwrap_or_default();
    let config = Config::load_read_only()
        .map_err(|_| ToolError::Failed("local configuration is unreadable or malformed".into()))?;
    execute_with(&store, &rates, &config, call, Utc::now())
}

fn execute_with(
    store: &HistoryStore,
    rates: &RateCard,
    config: &Config,
    call: PreparedCall,
    now: DateTime<Utc>,
) -> Result<String, ToolError> {
    let result = headless::execute(call.kind, store, rates, config, &call.request, now)
        .map_err(ToolError::failed)?;
    let result = if let Some(key) = call.envelope_key {
        json!({"schema_version":1, key:result})
    } else {
        result
    };
    serde_json::to_string_pretty(&result).map_err(ToolError::failed)
}

/// Execute a validated tool with supplied data, without resolving the ledger
/// or configuration from a user's directories. Kept for existing consumers.
pub fn call_tool_with(
    store: &HistoryStore,
    rates: &RateCard,
    params: &Value,
    now: DateTime<Utc>,
) -> Result<String, ToolError> {
    call_tool_with_config(store, rates, &Config::default(), params, now)
}

pub fn call_tool_with_config(
    store: &HistoryStore,
    rates: &RateCard,
    config: &Config,
    params: &Value,
    now: DateTime<Utc>,
) -> Result<String, ToolError> {
    execute_with(store, rates, config, prepare_call(params)?, now)
}

fn tool_response(id: Value, result: Result<String, ToolError>) -> String {
    match result {
        Ok(text) => success(
            id,
            json!({"content":[{"type":"text", "text":text}], "isError":false}),
        ),
        Err(ToolError::UnknownTool(name)) => {
            error_response(id, INVALID_PARAMS, &format!("unknown tool '{name}'"))
        }
        Err(ToolError::BadArguments(message)) => error_response(id, INVALID_PARAMS, &message),
        Err(ToolError::Failed(message)) => success(
            id,
            json!({"content":[{"type":"text", "text":message}], "isError":true}),
        ),
    }
}

fn success(id: Value, result: Value) -> String {
    json!({"jsonrpc":"2.0", "id":id, "result":result}).to_string()
}

fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc":"2.0", "id":id, "error":{"code":code, "message":message}}).to_string()
}

pub fn advertised_tool_names() -> Vec<String> {
    tool_specs()
        .iter()
        .map(|(name, _, _)| (*name).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    /// At the first cancellation line, wait until both workers are actually
    /// running. This proves interruption during execution, not just before it.
    struct GatedInput {
        inner: std::io::Cursor<Vec<u8>>,
        boundary: usize,
        started: mpsc::Receiver<()>,
        released: bool,
    }

    impl std::io::Read for GatedInput {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buffer)
        }
    }

    impl BufRead for GatedInput {
        fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
            if !self.released && self.inner.position() as usize >= self.boundary {
                for _ in 0..2 {
                    self.started
                        .recv_timeout(Duration::from_secs(2))
                        .expect("worker started");
                }
                self.released = true;
            }
            self.inner.fill_buf()
        }
        fn consume(&mut self, amount: usize) {
            self.inner.consume(amount);
        }
    }

    fn transport(input: &str) -> Vec<Value> {
        let mut output = Vec::new();
        serve_with_executor(input.as_bytes(), &mut output, |_, _| {
            panic!("invalid input must not execute")
        });
        String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn initializes_lists_read_only_tools_and_ignores_notifications() {
        let replies = transport(concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\"}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n"
        ));
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["result"]["protocolVersion"], PROTOCOL_VERSION);
        let instructions = replies[0]["result"]["instructions"].as_str().unwrap();
        for warning in [
            "sensitive",
            "never changes application records",
            "not prompts",
        ] {
            assert!(instructions.contains(warning));
        }
        for tool in replies[1]["result"]["tools"].as_array().unwrap() {
            assert_eq!(tool["annotations"]["readOnlyHint"], true);
            assert_eq!(tool["inputSchema"]["additionalProperties"], false);
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
        for old in [
            "usage_report",
            "project_report",
            "workflow_metrics",
            "quota_status",
        ] {
            assert!(advertised_tool_names().iter().any(|name| name == old));
        }
    }

    #[test]
    fn malformed_protocol_and_arguments_never_dispatch() {
        for input in [
            "{bad",
            "[]",
            "null",
            "{\"id\":1,\"method\":\"initialize\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":{},\"method\":\"initialize\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":true,\"method\":\"initialize\"}",
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":42}",
        ] {
            assert!(transport(input)[0]["error"].is_object(), "{input}");
        }
        for args in [
            json!(null),
            json!([]),
            json!({"from":false}),
            json!({"from":null}),
            json!({"from":"2026-9-01"}),
            json!({"from":"2026-02-30"}),
            json!({"from":"2026-09-10", "to":"2026-09-01"}),
            json!({"include_paths":true}),
            json!({"limit":10}),
        ] {
            let input = json!({"jsonrpc":"2.0", "id":1, "method":"tools/call", "params":{"name":"usage_report", "arguments":args}}).to_string();
            assert_eq!(
                transport(&input)[0]["error"]["code"],
                INVALID_PARAMS,
                "{args}"
            );
        }
        for limit in [json!(0), json!(1001), json!(-1), json!(1.5), json!("2")] {
            assert!(
                prepare_call(&json!({"name":"session_report", "arguments":{"limit":limit}}))
                    .is_err()
            );
        }
        assert!(matches!(
            prepare_call(&json!({"name":"delete_history"})),
            Err(ToolError::UnknownTool(_))
        ));
    }

    #[test]
    fn dates_are_inclusive_and_absent_windows_remain_all_time() {
        let call = prepare_call(
            &json!({"name":"usage_report", "arguments":{"from":"2026-08-01", "to":"2026-08-15"}}),
        )
        .unwrap();
        assert_eq!(
            call.request.from.unwrap().to_rfc3339(),
            "2026-08-01T00:00:00+00:00"
        );
        assert_eq!(
            call.request.to.unwrap().to_rfc3339(),
            "2026-08-15T23:59:59.999+00:00"
        );
        let call = prepare_call(&json!({"name":"usage_report"})).unwrap();
        assert!(call.request.from.is_none() && call.request.to.is_none());
    }

    #[test]
    fn oversized_input_is_discarded_and_next_request_is_answered() {
        let input = format!(
            "{}\n{{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}}\n",
            "x".repeat(MAX_INPUT_BYTES + 10)
        );
        let replies = transport(&input);
        assert_eq!(replies.len(), 2);
        assert_eq!(replies[0]["error"]["code"], INVALID_REQUEST);
        assert_eq!(replies[1]["id"], 7);
    }

    #[test]
    fn cancellation_interrupts_work_and_capacity_never_exceeds_two() {
        // Input holds both worker slots until cancellation notifications have
        // arrived. A third call is rejected immediately, never queued.
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"usage_report"}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"usage_report"}}),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"usage_report"}}),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}),
        ]
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>();
        let boundary = requests[..2].iter().map(|line| line.len() + 1).sum();
        let (started, receiver) = mpsc::channel();
        let input = GatedInput {
            inner: std::io::Cursor::new(requests.join("\n").into_bytes()),
            boundary,
            started: receiver,
            released: false,
        };
        let active = AtomicUsize::new(0);
        let maximum = AtomicUsize::new(0);
        let mut output = Vec::new();
        serve_with_executor(input, &mut output, |_, control| {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            maximum.fetch_max(count, Ordering::SeqCst);
            started.send(()).unwrap();
            loop {
                if let Err(error) = control.check() {
                    active.fetch_sub(1, Ordering::SeqCst);
                    return Err(ToolError::failed(error));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let replies: Vec<Value> = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(replies.len(), 3);
        assert_eq!(maximum.load(Ordering::SeqCst), 2);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert_eq!(
            replies.iter().find(|reply| reply["id"] == 3).unwrap()["error"]["code"],
            SERVER_BUSY
        );
        for id in [1, 2] {
            let reply = replies.iter().find(|reply| reply["id"] == id).unwrap();
            assert_eq!(reply["result"]["isError"], true);
            assert!(reply["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("cancelled"));
        }
    }

    #[test]
    fn eof_drains_work_and_oversized_results_are_explicit_errors() {
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"usage_report\"}}";
        for oversized in [false, true] {
            let mut output = Vec::new();
            serve_with_executor(input.as_bytes(), &mut output, |_, _| {
                Ok(if oversized {
                    "x".repeat(MAX_OUTPUT_BYTES)
                } else {
                    "{}".into()
                })
            });
            assert!(output.len() < MAX_OUTPUT_BYTES);
            let reply: Value = serde_json::from_slice(&output).unwrap();
            assert_eq!(reply["result"]["isError"], oversized);
        }
    }
}
