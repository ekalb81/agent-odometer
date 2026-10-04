//! Bounded direct-server verification shared by CLI and Integration Center.
//!
//! #57 is a P0 release gate for the MCP delivery in #47, and it states the
//! bar plainly: *"A configuration file or running process alone must never
//! be presented as proof that the integration works."*
//!
//! This proves the server round trip, not fresh-task use by a supported client.
//! Every check performs the real
//! operation and reports what came back:
//!
//! - the server binary launches as a child process,
//! - it completes an MCP `initialize` handshake,
//! - it advertises the tools it is supposed to advertise,
//! - a real `tools/call` returns a real answer from the ledger,
//! - and the ledger has data recent enough to be worth querying.
//!
//! A check that cannot be performed reports as such rather than passing by
//! default. "Not verified" and "verified working" must never render the
//! same, which is the failure mode this whole issue exists to prevent.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::history_store::HistoryStore;

/// Outcome of one verification step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// The operation was performed and behaved correctly.
    Pass,
    /// The operation was performed and did not behave correctly.
    Fail,
    /// The operation could not be performed, so nothing was proven. Never
    /// treated as a pass — an unverifiable integration is not a working one.
    Unknown,
}

/// One verification step and the evidence for its outcome.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub status: CheckStatus,
    /// What was actually observed, in a form a person can act on. Never a
    /// bare "ok" — the point of this command is that the evidence is
    /// visible, not that a green tick is.
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct VerifyReport {
    pub schema_version: u32,
    pub checks: Vec<Check>,
    /// True only when every check passed. An `Unknown` does not qualify.
    pub ok: bool,
}

impl VerifyReport {
    fn from_checks(checks: Vec<Check>) -> Self {
        let ok = checks.iter().all(|check| check.status == CheckStatus::Pass);
        Self {
            schema_version: VERIFY_SCHEMA_VERSION,
            checks,
            ok,
        }
    }
}

pub const VERIFY_SCHEMA_VERSION: u32 = 1;

/// Tools the MCP server is expected to expose. Verified against what the
/// server actually advertises, so a rename that breaks an agent's tool
/// selection fails here rather than in the agent.
const EXPECTED_TOOLS: &[&str] = &[
    "odometer_status",
    "usage_report",
    "model_report",
    "project_report",
    "workflow_metrics",
    "session_report",
    "activity_report",
    "category_report",
    "tools_report",
    "context_report",
    "findings_report",
    "diagnostics_report",
    "quota_status",
    "quota_report",
    "mirrored_sessions",
    "ledger_status",
    "statusline",
];

/// How stale the ledger may be before its recency is worth flagging.
///
/// Not a failure: an install that has not been used for a fortnight is
/// idle, not broken. It is reported so "the integration works but there is
/// nothing recent to query" is distinguishable from "the integration works
/// and has current data".
const RECENT_ACTIVITY_DAYS: i64 = 14;

/// Runs every verification step.
///
/// `executable` is the binary to launch for the MCP round trip — the
/// running executable in production, and a test can point it elsewhere.
pub fn verify(executable: &std::path::Path, now: DateTime<Utc>) -> VerifyReport {
    verify_in_directory(executable, None, now)
}

pub fn verify_in_directory(
    executable: &std::path::Path,
    directory: Option<&std::path::Path>,
    now: DateTime<Utc>,
) -> VerifyReport {
    let mut checks = Vec::new();

    let mcp = verify_mcp_round_trip_in_directory(executable, directory);
    checks.extend(mcp);
    checks.push(verify_ledger(now));

    VerifyReport::from_checks(checks)
}

/// Launches the MCP server and drives a real session against it.
///
/// This proves the direct server path. The UI separately reports client
/// configuration and real client activity; those claims cannot be inferred here.
#[cfg(test)]
fn verify_mcp_round_trip(executable: &std::path::Path) -> Vec<Check> {
    verify_mcp_round_trip_in_directory(executable, None)
}
fn verify_mcp_round_trip_in_directory(
    executable: &std::path::Path,
    directory: Option<&std::path::Path>,
) -> Vec<Check> {
    let mut checks = Vec::new();

    let mut command = Command::new(executable);
    command
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(directory) = directory {
        command.current_dir(directory);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let child = command.spawn();

    let mut child = match child {
        Ok(child) => child,
        Err(_) => {
            // Every later check depended on this one, and none of them ran.
            // Reporting them as `Unknown` rather than omitting them keeps
            // the list a stable shape for anything reading it.
            checks.push(Check {
                id: "mcp_launch",
                status: CheckStatus::Fail,
                detail: diagnostic_detail(
                    crate::integration_status::DiagnosticCode::ServerLaunchFailed,
                ),
            });
            for id in ["mcp_initialize", "mcp_tools", "mcp_query"] {
                checks.push(Check {
                    id,
                    status: CheckStatus::Unknown,
                    detail: "not attempted: the server did not launch".into(),
                });
            }
            return checks;
        }
    };
    checks.push(Check {
        id: "mcp_launch",
        status: CheckStatus::Pass,
        detail: format!("launched '{} mcp'", executable.display()),
    });

    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = (|| {
            let mut stdin = stdin.context("no stdin on the server")?;
            let mut reader = BufReader::new(stdout.context("no stdout on the server")?);
            drive_session(&mut stdin, &mut reader)
        })();
        let _ = send.send(result);
    });
    let result = receive
        .recv_timeout(std::time::Duration::from_secs(20))
        .context("MCP verification exceeded its 20-second deadline")
        .and_then(|result| result);
    // Always reaped: a verification command that leaves a stray server
    // process behind has made the system slightly worse for having run.
    let _ = child.kill();
    let _ = child.wait();
    let _ = worker.join();

    match result {
        Ok(session) => checks.extend(session),
        Err(error) => {
            for id in ["mcp_initialize", "mcp_tools", "mcp_query"] {
                checks.push(Check {
                    id,
                    status: CheckStatus::Unknown,
                    detail: format!("could not complete the session: {error}"),
                });
            }
        }
    }
    checks
}

fn drive_session<W: Write, R: BufRead>(mut stdin: W, mut reader: R) -> Result<Vec<Check>> {
    let mut checks = Vec::new();

    let initialize = request(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"odometer-verifier","version":"1"}}}"#,
    )?;
    let protocol = initialize["result"]["protocolVersion"]
        .as_str()
        .unwrap_or_default();
    checks.push(if protocol != crate::mcp_server::PROTOCOL_VERSION {
        Check {
            id: "mcp_initialize",
            status: CheckStatus::Fail,
            detail: diagnostic_detail(
                crate::integration_status::DiagnosticCode::ProtocolVersionMismatch,
            ),
        }
    } else {
        Check {
            id: "mcp_initialize",
            status: CheckStatus::Pass,
            detail: format!("handshake completed, protocol {protocol}"),
        }
    });

    let listed = request(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    )?;
    let advertised: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .map(|tools| {
            tools
                .iter()
                .filter_map(|tool| tool["name"].as_str())
                .collect()
        })
        .unwrap_or_default();
    let schemas_match = listed["result"]["tools"].as_array().is_some_and(|tools| {
        let expected = crate::mcp_server::tool_descriptors();
        tools.len() == expected.len()
            && expected.iter().all(|required| {
                tools
                    .iter()
                    .filter(|tool| {
                        tool["name"] == required["name"]
                            && tool["inputSchema"] == required["inputSchema"]
                    })
                    .count()
                    == 1
            })
    });
    checks.push(
        if schemas_match && advertised.len() == EXPECTED_TOOLS.len() {
            Check {
                id: "mcp_tools",
                status: CheckStatus::Pass,
                detail: format!(
                    "all {} expected tools and input schemas advertised",
                    EXPECTED_TOOLS.len()
                ),
            }
        } else {
            Check {
                id: "mcp_tools",
                status: CheckStatus::Fail,
                detail: diagnostic_detail(
                    crate::integration_status::DiagnosticCode::ToolCatalogMismatch,
                ),
            }
        },
    );

    // A real query, not a ping: this is what proves the server can reach the
    // ledger and produce an answer, which is the whole claim being verified.
    let called = request(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"odometer_status","arguments":{}}}"#,
    )?;
    let is_error = called["result"]["isError"].as_bool().unwrap_or(true);
    let text = called["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    let status = serde_json::from_str::<Value>(text).ok();
    let usable = status.as_ref().is_some_and(|status| {
        status["schema_version"].as_u64()
            == Some(u64::from(crate::integration_status::SCHEMA_VERSION))
            && status["ledger_available"] == true
            && status["sessions"].as_u64().is_some()
    });
    checks.push(if is_error || !usable {
        Check {
            id: "mcp_query",
            status: CheckStatus::Fail,
            detail: diagnostic_detail(crate::integration_status::DiagnosticCode::LedgerNotReady),
        }
    } else {
        let sessions = status.and_then(|value| value["sessions"].as_u64());
        Check {
            id: "mcp_query",
            status: CheckStatus::Pass,
            detail: match sessions {
                Some(count) => format!("odometer_status returned a usable schema and readable ledger over {count} session(s); scan freshness remains unknown"),
                None => "a live query returned a result".into(),
            },
        }
    });

    Ok(checks)
}

/// Sends one request and reads one response line.
fn request<W: Write, R: BufRead>(input: &mut W, output: &mut R, line: &str) -> Result<Value> {
    writeln!(input, "{line}").context("could not write to the server")?;
    input.flush().context("could not flush to the server")?;
    let mut response = String::new();
    const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
    let read = output
        .take(MAX_RESPONSE_BYTES as u64 + 1)
        .read_line(&mut response)
        .context("could not read from the server")?;
    if read > MAX_RESPONSE_BYTES {
        anyhow::bail!("the server response exceeds 8 MiB");
    }
    if read == 0 {
        anyhow::bail!("the server closed its output before replying");
    }
    let response: Value =
        serde_json::from_str(&response).context("the server's reply was not JSON")?;
    let request: Value = serde_json::from_str(line)?;
    if response["jsonrpc"] != "2.0" || response["id"] != request["id"] {
        anyhow::bail!("the server response did not match the request");
    }
    Ok(response)
}

fn diagnostic_detail(code: crate::integration_status::DiagnosticCode) -> String {
    let diagnostic = code.diagnostic();
    format!(
        "{}: {} {}",
        serde_json::to_value(code)
            .expect("static diagnostic code serializes")
            .as_str()
            .expect("diagnostic is a string"),
        diagnostic.evidence,
        diagnostic.next_action
    )
}

/// Confirms the ledger opens and reports how current its data is.
fn verify_ledger(now: DateTime<Utc>) -> Check {
    let path = match HistoryStore::default_path() {
        Ok(path) => path,
        Err(error) => {
            return Check {
                id: "ledger",
                status: CheckStatus::Unknown,
                detail: format!("could not resolve the ledger location: {error}"),
            }
        }
    };
    let store =
        match HistoryStore::open_read_only(&path, crate::query_control::QueryControl::default()) {
            Ok(store) => store,
            Err(error) => {
                return Check {
                    id: "ledger",
                    status: CheckStatus::Fail,
                    detail: format!("could not open {}: {error}", path.display()),
                }
            }
        };
    let cutoff = now - Duration::days(RECENT_ACTIVITY_DAYS);
    match store.session_keys_since(cutoff.timestamp_millis()) {
        Ok(keys) if !keys.is_empty() => Check {
            id: "ledger",
            status: CheckStatus::Pass,
            detail: format!(
                "{} session(s) seen in the last {RECENT_ACTIVITY_DAYS} days",
                keys.len()
            ),
        },
        // Idle, not broken — and said in those words, because "0 recent
        // sessions" alongside a failure would read as a symptom of one.
        Ok(_) => Check {
            id: "ledger",
            status: CheckStatus::Pass,
            detail: format!(
                "the ledger is queryable but has no activity in the last {RECENT_ACTIVITY_DAYS} days"
            ),
        },
        Err(error) => Check {
            id: "ledger",
            status: CheckStatus::Fail,
            detail: format!("the ledger opened but could not be queried: {error}"),
        },
    }
}

/// Renders a report for a terminal.
pub fn render(report: &VerifyReport) -> String {
    let mut out = String::new();
    for check in &report.checks {
        let marker = match check.status {
            CheckStatus::Pass => "ok  ",
            CheckStatus::Fail => "FAIL",
            CheckStatus::Unknown => "?   ",
        };
        out.push_str(&format!("{marker} {:<16} {}\n", check.id, check.detail));
    }
    out.push_str(if report.ok {
        "\ndirect server round trip verified; fresh client task use is not proven\n"
    } else {
        // Never "mostly working": a partially verified integration is one an
        // agent will fail against in a way nobody expects.
        "\nintegration NOT verified — see the checks above\n"
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer_responses(
        protocol: &str,
        tools: Value,
        status: Value,
        is_error: bool,
    ) -> std::io::Cursor<Vec<u8>> {
        let responses = [
            serde_json::json!({"jsonrpc":"2.0", "id":1, "result":{"protocolVersion":protocol}}),
            serde_json::json!({"jsonrpc":"2.0", "id":2, "result":{"tools":tools}}),
            serde_json::json!({"jsonrpc":"2.0", "id":3, "result":{"isError":is_error,"content":[{"type":"text", "text":status.to_string()}]}}),
        ];
        std::io::Cursor::new(
            responses
                .iter()
                .map(|value| format!("{value}\n"))
                .collect::<String>()
                .into_bytes(),
        )
    }

    #[test]
    fn verifier_rejects_stale_protocol_schema_and_fabricated_canary() {
        let catalog = serde_json::to_value(crate::mcp_server::tool_descriptors()).unwrap();
        let status = serde_json::json!({"schema_version":1,"ledger_available":true,"sessions":0});
        let mut reader = peer_responses("2025-06-18", catalog.clone(), status.clone(), false);
        assert!(drive_session(Vec::new(), &mut reader)
            .unwrap()
            .iter()
            .all(|check| check.status == CheckStatus::Pass));
        let mut reader = peer_responses("unsupported", catalog.clone(), status.clone(), false);
        assert_eq!(
            drive_session(Vec::new(), &mut reader).unwrap()[0].status,
            CheckStatus::Fail
        );
        let mut stale = catalog.clone();
        stale[0]["inputSchema"]["additionalProperties"] = Value::Bool(true);
        let mut reader = peer_responses("2025-06-18", stale, status.clone(), false);
        assert_eq!(
            drive_session(Vec::new(), &mut reader).unwrap()[1].status,
            CheckStatus::Fail
        );
        for invalid in [
            serde_json::json!({"sessions":0}),
            serde_json::json!({"schema_version":1,"ledger_available":false,"sessions":null}),
            serde_json::json!({"schema_version":99,"ledger_available":true,"sessions":3}),
        ] {
            let mut reader = peer_responses("2025-06-18", catalog.clone(), invalid, false);
            assert_eq!(
                drive_session(Vec::new(), &mut reader).unwrap()[2].status,
                CheckStatus::Fail
            );
        }
        let secret = serde_json::json!({"private_prompt":"synthetic secret response text"});
        let mut reader = peer_responses("2025-06-18", catalog, secret, true);
        let checks = drive_session(Vec::new(), &mut reader).unwrap();
        assert!(!serde_json::to_string(&checks)
            .unwrap()
            .contains("synthetic secret"));
    }

    #[test]
    fn verifier_rejects_a_response_for_another_request() {
        let mut reader = std::io::Cursor::new(b"{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}\n");
        assert!(request(
            &mut Vec::new(),
            &mut reader,
            r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#
        )
        .unwrap_err()
        .to_string()
        .contains("did not match"));
    }

    #[test]
    fn verification_requires_every_check_to_pass() {
        for status in [CheckStatus::Pass, CheckStatus::Fail, CheckStatus::Unknown] {
            let report = VerifyReport::from_checks(vec![
                Check {
                    id: "mcp_initialize",
                    status: CheckStatus::Pass,
                    detail: "synthetic handshake succeeded".into(),
                },
                Check {
                    id: "ledger",
                    status,
                    detail: "synthetic ledger observation".into(),
                },
            ]);
            assert_eq!(report.ok, status == CheckStatus::Pass);
            assert_eq!(render(&report).contains("NOT verified"), !report.ok);
        }
    }

    #[test]
    fn verification_rejects_oversized_peer_output() {
        let oversized = vec![b'x'; 8 * 1024 * 1024 + 1];
        let mut reader = std::io::Cursor::new(oversized);
        let error = request(&mut Vec::new(), &mut reader, "{}").unwrap_err();
        assert!(error.to_string().contains("exceeds 8 MiB"));
    }

    #[test]
    fn a_launch_failure_marks_the_dependent_checks_unknown_not_failed() {
        // The later checks did not run, so they proved nothing. Reporting
        // them as failures would be as wrong as reporting them as passes —
        // and reporting nothing at all would change the shape of the list
        // for anything parsing it.
        let checks = verify_mcp_round_trip(std::path::Path::new(
            "a-binary-that-does-not-exist-anywhere",
        ));

        assert_eq!(checks[0].id, "mcp_launch");
        assert_eq!(checks[0].status, CheckStatus::Fail);
        for check in &checks[1..] {
            assert_eq!(
                check.status,
                CheckStatus::Unknown,
                "{} ran without a server",
                check.id
            );
        }
    }

    #[test]
    fn an_unknown_check_never_counts_as_a_pass() {
        let report = VerifyReport {
            schema_version: VERIFY_SCHEMA_VERSION,
            checks: vec![
                Check {
                    id: "mcp_launch",
                    status: CheckStatus::Pass,
                    detail: "launched".into(),
                },
                Check {
                    id: "mcp_query",
                    status: CheckStatus::Unknown,
                    detail: "not attempted".into(),
                },
            ],
            ok: false,
        };

        // The rendering must not congratulate on an unverified integration.
        let rendered = render(&report);
        assert!(rendered.contains("NOT verified"), "{rendered}");
    }

    #[test]
    fn the_rendering_shows_evidence_not_just_a_verdict() {
        let report = VerifyReport {
            schema_version: VERIFY_SCHEMA_VERSION,
            checks: vec![Check {
                id: "mcp_initialize",
                status: CheckStatus::Pass,
                detail: "handshake completed, protocol 2025-06-18".into(),
            }],
            ok: true,
        };

        let rendered = render(&report);

        // A green tick with no evidence is exactly what #57 says must not
        // count as proof.
        assert!(rendered.contains("2025-06-18"), "{rendered}");
        assert!(
            rendered.contains("direct server round trip verified"),
            "{rendered}"
        );
        assert!(
            rendered.contains("fresh client task use is not proven"),
            "{rendered}"
        );
    }

    #[test]
    fn the_expected_tool_list_matches_what_the_server_advertises() {
        // Guards a rename on either side: the server's tool names and this
        // list must not drift apart silently, because the failure would show
        // up as an agent choosing no tool at all.
        let advertised: Vec<String> = crate::mcp_server::advertised_tool_names();
        for expected in EXPECTED_TOOLS {
            assert!(
                advertised.iter().any(|name| name == expected),
                "{expected} is expected by verification but not advertised"
            );
        }
        assert_eq!(advertised.len(), EXPECTED_TOOLS.len());
    }
}
