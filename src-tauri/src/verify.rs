//! End-to-end integration verification (issue #57).
//!
//! #57 is a P0 release gate for the MCP delivery in #47, and it states the
//! bar plainly: *"A configuration file or running process alone must never
//! be presented as proof that the integration works."*
//!
//! So nothing here inspects configuration. Every check performs the real
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

use std::io::{BufRead, BufReader, Write};
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

pub const VERIFY_SCHEMA_VERSION: u32 = 1;

/// Tools the MCP server is expected to expose. Verified against what the
/// server actually advertises, so a rename that breaks an agent's tool
/// selection fails here rather than in the agent.
const EXPECTED_TOOLS: &[&str] = &[
    "usage_report",
    "project_report",
    "workflow_metrics",
    "quota_status",
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
    let mut checks = Vec::new();

    let mcp = verify_mcp_round_trip(executable);
    checks.extend(mcp);
    checks.push(verify_ledger(now));

    let ok = checks.iter().all(|check| check.status == CheckStatus::Pass);
    VerifyReport {
        schema_version: VERIFY_SCHEMA_VERSION,
        checks,
        ok,
    }
}

/// Launches the MCP server and drives a real session against it.
///
/// This is the check #57 is really about. Reading a client config file
/// would prove a file exists; spawning the server and getting a real answer
/// back proves the integration works.
fn verify_mcp_round_trip(executable: &std::path::Path) -> Vec<Check> {
    let mut checks = Vec::new();

    let child = Command::new(executable)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();

    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            // Every later check depended on this one, and none of them ran.
            // Reporting them as `Unknown` rather than omitting them keeps
            // the list a stable shape for anything reading it.
            checks.push(Check {
                id: "mcp_launch",
                status: CheckStatus::Fail,
                detail: format!("could not launch '{} mcp': {error}", executable.display()),
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

    let result = drive_session(&mut child);
    // Always reaped: a verification command that leaves a stray server
    // process behind has made the system slightly worse for having run.
    let _ = child.kill();
    let _ = child.wait();

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

fn drive_session(child: &mut std::process::Child) -> Result<Vec<Check>> {
    let mut stdin = child.stdin.take().context("no stdin on the server")?;
    let stdout = child.stdout.take().context("no stdout on the server")?;
    let mut reader = BufReader::new(stdout);
    let mut checks = Vec::new();

    let initialize = request(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#,
    )?;
    let protocol = initialize["result"]["protocolVersion"]
        .as_str()
        .unwrap_or_default();
    checks.push(if protocol.is_empty() {
        Check {
            id: "mcp_initialize",
            status: CheckStatus::Fail,
            detail: "the server did not report a protocol version".into(),
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
    let missing: Vec<&str> = EXPECTED_TOOLS
        .iter()
        .copied()
        .filter(|expected| !advertised.contains(expected))
        .collect();
    checks.push(if missing.is_empty() {
        Check {
            id: "mcp_tools",
            status: CheckStatus::Pass,
            detail: format!("all {} expected tools advertised", EXPECTED_TOOLS.len()),
        }
    } else {
        Check {
            id: "mcp_tools",
            status: CheckStatus::Fail,
            detail: format!("missing tools: {}", missing.join(", ")),
        }
    });

    // A real query, not a ping: this is what proves the server can reach the
    // ledger and produce an answer, which is the whole claim being verified.
    let called = request(
        &mut stdin,
        &mut reader,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"workflow_metrics","arguments":{}}}"#,
    )?;
    let is_error = called["result"]["isError"].as_bool().unwrap_or(true);
    let text = called["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    checks.push(if is_error || text.is_empty() {
        Check {
            id: "mcp_query",
            status: CheckStatus::Fail,
            detail: if text.is_empty() {
                "the tool returned no content".into()
            } else {
                format!("the tool reported an error: {text}")
            },
        }
    } else {
        let sessions = serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|value| value["sessions"].as_u64());
        Check {
            id: "mcp_query",
            status: CheckStatus::Pass,
            detail: match sessions {
                Some(count) => format!("a live query returned metrics over {count} session(s)"),
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
    let read = output
        .read_line(&mut response)
        .context("could not read from the server")?;
    if read == 0 {
        anyhow::bail!("the server closed its output before replying");
    }
    serde_json::from_str(&response).context("the server's reply was not JSON")
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
    let store = match HistoryStore::open(&path) {
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
        "\nintegration verified end to end\n"
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
        assert!(rendered.contains("verified end to end"), "{rendered}");
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
