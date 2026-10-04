//! Opt-in Codex quota reads through the user's installed Codex app-server.
//!
//! This module never reads Codex credential files or calls provider HTTP
//! endpoints directly. Its caller must obtain explicit consent before calling
//! either public function. It launches `codex app-server` over bounded stdio,
//! asks only for account/rate-limit metadata, and returns fixed error codes
//! rather than process or provider response text.

use chrono::{DateTime, Utc};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

const MAX_PIPE_READERS: usize = 4;
static PIPE_READERS: AtomicUsize = AtomicUsize::new(0);
struct ReaderPermit<'a>(&'a AtomicUsize);
impl<'a> ReaderPermit<'a> {
    fn acquire(counter: &'a AtomicUsize) -> Result<Self, LiveQuotaError> {
        let mut active = counter.load(Ordering::Acquire);
        loop {
            if active >= MAX_PIPE_READERS {
                return Err(LiveQuotaError::LaunchFailed);
            }
            match counter.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => active = current,
            }
        }
        Ok(Self(counter))
    }
}
impl Drop for ReaderPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

const TOTAL_DEADLINE: Duration = Duration::from_secs(15);
const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024;
const MAX_SERVER_MESSAGES: usize = 32;
const MAX_LIMIT_ID_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 256;
const MAX_ACCOUNT_ID_BYTES: usize = 256;
const MAX_DURATION_MINUTES: i64 = 366 * 24 * 60;
const MAX_CREDIT_BALANCE: f64 = 1_000_000_000_000.0;
const MIN_RESET_EPOCH: i64 = 946_684_800; // 2000-01-01 UTC
const MAX_RESET_EPOCH: i64 = 4_102_444_800; // 2100-01-01 UTC

/// A deliberately small, serializable account identity. Email is not read or
/// retained; callers can ask the user to provide a local display label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DiscoveredQuotaAccount {
    pub account_id: String,
    pub plan_type: Option<String>,
}

/// One live read, scoped to the exact account id returned with the rate-limit
/// response. `observed_at` is local fetch time, not a provider timestamp.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveQuotaReading {
    pub account_id: String,
    pub ordinary_usage_allowed: Option<bool>,
    pub observed_at: DateTime<Utc>,
    pub limit_buckets: Vec<LiveQuotaBucket>,
}

/// A distinct rate-limit bucket. Different `limit_id`s are never combined.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveQuotaBucket {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub spend_control_reached: Option<bool>,
    pub primary: Option<LiveQuotaWindow>,
    pub secondary: Option<LiveQuotaWindow>,
    pub credits: Option<LiveQuotaCredits>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveQuotaWindow {
    pub used_percent: f64,
    pub window_minutes: Option<u64>,
    pub resets_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LiveQuotaCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    /// `Some(0.0)` is an observed zero; `None` means no balance was reported.
    pub balance: Option<f64>,
}

/// Stable, payload-free failures suitable for UI and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveQuotaError {
    Offline,
    LaunchFailed,
    Timeout,
    AuthExpired,
    RateLimited,
    ProviderOutage,
    Unsupported,
    AccountChanged,
    ProtocolMismatch,
    InvalidResponse,
    NoObservation,
}

impl LiveQuotaError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::LaunchFailed => "launch_failed",
            Self::Timeout => "timeout",
            Self::AuthExpired => "auth_expired",
            Self::RateLimited => "rate_limited",
            Self::ProviderOutage => "provider_outage",
            Self::Unsupported => "unsupported",
            Self::AccountChanged => "account_changed",
            Self::ProtocolMismatch => "protocol_mismatch",
            Self::InvalidResponse => "invalid_response",
            Self::NoObservation => "no_observation",
        }
    }
}

/// One explicit identity-discovery action. The caller is responsible for
/// gating this with the user's consent-to-identify action.
pub fn discover_codex_account(executable: &Path) -> Result<DiscoveredQuotaAccount, LiveQuotaError> {
    let deadline = Instant::now() + TOTAL_DEADLINE;
    let mut server = AppServer::spawn(executable, deadline)?;
    server.initialize(deadline)?;
    let account = server.read_account(deadline)?;
    let rates = server.read_rate_limits(deadline)?;
    let account_id = checked_account_id(rates.account_id.as_deref())?;
    Ok(DiscoveredQuotaAccount {
        account_id,
        plan_type: account.plan_type,
    })
}

/// Reads quota only after account-scoped consent has been recorded by the
/// caller. A missing or changed server-reported account id fails closed.
pub fn read_codex_quota(
    executable: &Path,
    approved_account_id: &str,
) -> Result<LiveQuotaReading, LiveQuotaError> {
    let approved_account_id = checked_account_id(Some(approved_account_id))?;
    let deadline = Instant::now() + TOTAL_DEADLINE;
    let mut server = AppServer::spawn(executable, deadline)?;
    server.initialize(deadline)?;
    let account = server.read_account(deadline)?;
    validate_approved_local_account(account.chatgpt_account_id.as_deref(), &approved_account_id)?;
    let response = server.read_rate_limits(deadline)?;
    let account_id = checked_account_id(response.account_id.as_deref())?;
    if account_id != approved_account_id {
        return Err(LiveQuotaError::AccountChanged);
    }
    let limit_buckets = parse_limit_buckets(&response)?;
    Ok(LiveQuotaReading {
        account_id,
        ordinary_usage_allowed: response.ordinary_usage_allowed,
        observed_at: Utc::now(),
        limit_buckets,
    })
}

struct AppServer {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    incoming: Receiver<Incoming>,
    next_id: u64,
    message_count: usize,
}

enum Incoming {
    Line(Vec<u8>),
    Eof,
    InvalidLine,
    ReadFailure,
}

impl AppServer {
    fn spawn(executable: &Path, deadline: Instant) -> Result<Self, LiveQuotaError> {
        let permit = ReaderPermit::acquire(&PIPE_READERS)?;
        let mut command = Command::new(executable);
        command
            .arg("app-server")
            .arg("--listen")
            .arg("stdio://")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Never retain stderr: it can contain provider or auth details.
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn().map_err(|error| match error.kind() {
            io::ErrorKind::NotFound => LiveQuotaError::Offline,
            _ => LiveQuotaError::LaunchFailed,
        })?;
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LiveQuotaError::Timeout);
        }

        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LiveQuotaError::LaunchFailed);
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LiveQuotaError::LaunchFailed);
        };
        let (send, incoming) = mpsc::channel();
        // A descendant retaining stdout must not make teardown wait beyond
        // the operation deadline, so this bounded reader is detached.
        let _reader = thread::Builder::new()
            .name("quota-pipe-reader".into())
            .spawn(move || {
                let _permit = permit;
                read_messages(stdout, send);
            });
        if _reader.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(LiveQuotaError::LaunchFailed);
        }
        Ok(Self {
            child,
            stdin: BufWriter::new(stdin),
            incoming,
            next_id: 1,
            message_count: 0,
        })
    }

    fn initialize(&mut self, deadline: Instant) -> Result<(), LiveQuotaError> {
        let result: EmptyObject = self.request(
            "initialize",
            serde_json::json!({
                "clientInfo": {
                    "name": "agent-odometer",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
            deadline,
        )?;
        let _ = result;
        self.notify("initialized", serde_json::json!({}))
    }

    fn read_account(&mut self, deadline: Instant) -> Result<AccountMetadata, LiveQuotaError> {
        let result: AccountReadResult = self.request(
            "account/read",
            serde_json::json!({ "refreshToken": false }),
            deadline,
        )?;
        account_metadata(result)
    }

    fn read_rate_limits(
        &mut self,
        deadline: Instant,
    ) -> Result<RateLimitsResponse, LiveQuotaError> {
        self.request(
            "account/rateLimits/read",
            // Do not advertise support for Luna Reserve; true can enroll an
            // eligible account in a backend experiment. Avoid unused detail
            // lookup while retaining the rate-limit snapshot and credit count.
            serde_json::json!({ "excludeResetCreditDetails": true }),
            deadline,
        )
    }

    fn notify(&mut self, method: &str, params: serde_json::Value) -> Result<(), LiveQuotaError> {
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        let bytes = serde_json::to_vec(&message).map_err(|_| LiveQuotaError::ProtocolMismatch)?;
        if bytes.len() > MAX_LINE_BYTES {
            return Err(LiveQuotaError::ProtocolMismatch);
        }
        self.stdin
            .write_all(&bytes)
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|_| LiveQuotaError::ProviderOutage)
    }

    fn request<T: DeserializeOwned>(
        &mut self,
        method: &str,
        params: serde_json::Value,
        deadline: Instant,
    ) -> Result<T, LiveQuotaError> {
        let id = self.next_id;
        self.next_id += 1;
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        let bytes = serde_json::to_vec(&message).map_err(|_| LiveQuotaError::ProtocolMismatch)?;
        if bytes.len() > MAX_LINE_BYTES {
            return Err(LiveQuotaError::ProtocolMismatch);
        }
        self.stdin
            .write_all(&bytes)
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|_| LiveQuotaError::ProviderOutage)?;

        loop {
            self.message_count += 1;
            if self.message_count > MAX_SERVER_MESSAGES {
                return Err(LiveQuotaError::ProtocolMismatch);
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|duration| !duration.is_zero())
                .ok_or(LiveQuotaError::Timeout)?;
            let incoming = self
                .incoming
                .recv_timeout(remaining)
                .map_err(|error| match error {
                    RecvTimeoutError::Timeout => LiveQuotaError::Timeout,
                    RecvTimeoutError::Disconnected => LiveQuotaError::ProviderOutage,
                })?;
            let Incoming::Line(line) = incoming else {
                return Err(match incoming {
                    Incoming::Eof => LiveQuotaError::ProviderOutage,
                    Incoming::InvalidLine => LiveQuotaError::ProtocolMismatch,
                    Incoming::ReadFailure => LiveQuotaError::ProviderOutage,
                    Incoming::Line(_) => unreachable!(),
                });
            };
            let header: RpcHeader =
                serde_json::from_slice(&line).map_err(|_| LiveQuotaError::ProtocolMismatch)?;
            validate_jsonrpc_version(header.jsonrpc.as_deref())?;
            match (header.id, header.method) {
                (None, Some(_)) => continue, // bounded unsolicited notification
                (Some(_), Some(_)) => return Err(LiveQuotaError::ProtocolMismatch),
                (Some(received_id), None) if received_id != id => {
                    return Err(LiveQuotaError::ProtocolMismatch);
                }
                (Some(_), None) => {
                    let response: RpcResponse<T> = serde_json::from_slice(&line)
                        .map_err(|_| LiveQuotaError::ProtocolMismatch)?;
                    validate_jsonrpc_version(response.jsonrpc.as_deref())?;
                    if response.id != Some(id) {
                        return Err(LiveQuotaError::ProtocolMismatch);
                    }
                    if let Some(error) = response.error {
                        return Err(map_rpc_error(error.code));
                    }
                    return response.result.ok_or(LiveQuotaError::InvalidResponse);
                }
                (None, None) => return Err(LiveQuotaError::ProtocolMismatch),
            }
        }
    }
}

impl Drop for AppServer {
    fn drop(&mut self) {
        // Closing the server is part of the operation, not an optimization:
        // no direct app-server child should outlive a quota read. The reader
        // is detached so an inherited stdout pipe cannot extend teardown.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn validate_jsonrpc_version(version: Option<&str>) -> Result<(), LiveQuotaError> {
    if version.is_some_and(|value| value != "2.0") {
        Err(LiveQuotaError::ProtocolMismatch)
    } else {
        Ok(())
    }
}

fn account_metadata(result: AccountReadResult) -> Result<AccountMetadata, LiveQuotaError> {
    // This describes the provider auth mechanism; a valid ChatGPT account
    // sets it, so it does not mean authentication expired.
    let _requires_openai_auth = result.requires_openai_auth;
    let account = result.account.ok_or(LiveQuotaError::AuthExpired)?;
    if account.kind != "chatgpt" {
        return Err(LiveQuotaError::Unsupported);
    }
    if account
        .plan_type
        .as_ref()
        .is_some_and(|plan| plan.len() > MAX_LABEL_BYTES || plan.chars().any(char::is_control))
    {
        return Err(LiveQuotaError::InvalidResponse);
    }
    Ok(AccountMetadata {
        plan_type: account.plan_type,
        chatgpt_account_id: result
            .workspace_routing
            .map(|routing| routing.chatgpt_account_id),
    })
}

fn validate_approved_local_account(
    local_account_id: Option<&str>,
    approved_account_id: &str,
) -> Result<(), LiveQuotaError> {
    let local_account_id = local_account_id.ok_or(LiveQuotaError::Unsupported)?;
    let local_account_id = checked_account_id(Some(local_account_id))?;
    if local_account_id == approved_account_id {
        Ok(())
    } else {
        Err(LiveQuotaError::AccountChanged)
    }
}

fn map_rpc_error(code: i64) -> LiveQuotaError {
    match code {
        401 | 403 => LiveQuotaError::AuthExpired,
        429 => LiveQuotaError::RateLimited,
        -32601 => LiveQuotaError::Unsupported,
        _ => LiveQuotaError::ProviderOutage,
    }
}

fn read_messages(stdout: impl io::Read, send: mpsc::Sender<Incoming>) {
    let mut reader = BufReader::new(stdout);
    let mut total = 0usize;
    loop {
        match read_bounded_line(&mut reader, &mut total) {
            Ok(Some(line)) => {
                if send.send(Incoming::Line(line)).is_err() {
                    return;
                }
            }
            Ok(None) => {
                let _ = send.send(Incoming::Eof);
                return;
            }
            Err(ReadLineFailure::Limit) => {
                let _ = send.send(Incoming::InvalidLine);
                return;
            }
            Err(ReadLineFailure::Io) => {
                let _ = send.send(Incoming::ReadFailure);
                return;
            }
        }
    }
}

enum ReadLineFailure {
    Limit,
    Io,
}

/// Reads a line without ever allocating more than the remaining hard byte
/// budget. `read_until` is deliberately avoided because checking afterward
/// would allow an untrusted child to allocate an unbounded line first.
fn read_bounded_line<R: BufRead>(
    reader: &mut R,
    total: &mut usize,
) -> Result<Option<Vec<u8>>, ReadLineFailure> {
    let mut line = Vec::with_capacity(256);
    loop {
        let available = reader.fill_buf().map_err(|_| ReadLineFailure::Io)?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(ReadLineFailure::Limit)
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let segment_len = newline.unwrap_or(available.len());
        let consumed = segment_len + usize::from(newline.is_some());
        if line.len().saturating_add(segment_len) > MAX_LINE_BYTES
            || total.saturating_add(consumed) > MAX_TOTAL_BYTES
        {
            return Err(ReadLineFailure::Limit);
        }
        line.extend_from_slice(&available[..segment_len]);
        *total += consumed;
        reader.consume(consumed);
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Some(line));
        }
    }
}

fn checked_account_id(value: Option<&str>) -> Result<String, LiveQuotaError> {
    let Some(value) = value else {
        return Err(LiveQuotaError::InvalidResponse);
    };
    if value.is_empty()
        || value.len() > MAX_ACCOUNT_ID_BYTES
        || value.chars().any(char::is_control)
        || value.trim() != value
    {
        return Err(LiveQuotaError::InvalidResponse);
    }
    Ok(value.to_owned())
}

fn parse_limit_buckets(
    response: &RateLimitsResponse,
) -> Result<Vec<LiveQuotaBucket>, LiveQuotaError> {
    let source: BTreeMap<String, RawLimitBucket> = match &response.rate_limits_by_limit_id {
        Some(map) => {
            if map.is_empty() {
                return Err(LiveQuotaError::NoObservation);
            }
            map.clone()
        }
        None => {
            let legacy = response
                .rate_limits
                .as_ref()
                .ok_or(LiveQuotaError::NoObservation)?;
            let id = legacy
                .limit_id
                .as_deref()
                .filter(|value| !value.is_empty())
                .ok_or(LiveQuotaError::InvalidResponse)?;
            BTreeMap::from([(id.to_owned(), legacy.clone())])
        }
    };

    let mut buckets = Vec::with_capacity(source.len());
    for (id, raw) in source {
        if id.is_empty()
            || id.len() > MAX_LIMIT_ID_BYTES
            || id.chars().any(char::is_control)
            || raw.limit_id.as_deref().is_some_and(|inner| inner != id)
        {
            return Err(LiveQuotaError::InvalidResponse);
        }
        if raw
            .limit_name
            .as_ref()
            .is_some_and(|name| name.len() > MAX_LABEL_BYTES || name.chars().any(char::is_control))
        {
            return Err(LiveQuotaError::InvalidResponse);
        }
        let primary = raw.primary.as_ref().map(parse_window).transpose()?;
        let secondary = raw.secondary.as_ref().map(parse_window).transpose()?;
        let credits = raw.credits.as_ref().map(parse_credits).transpose()?;
        if primary.is_none() && secondary.is_none() && credits.is_none() {
            continue;
        }
        buckets.push(LiveQuotaBucket {
            limit_id: id,
            limit_name: raw.limit_name,
            spend_control_reached: raw.spend_control_reached,
            primary,
            secondary,
            credits,
        });
    }
    if buckets.is_empty() {
        return Err(LiveQuotaError::NoObservation);
    }
    Ok(buckets)
}

fn parse_window(raw: &RawWindow) -> Result<LiveQuotaWindow, LiveQuotaError> {
    let used_percent = raw
        .used_percent
        .as_ref()
        .and_then(serde_json::Number::as_f64)
        .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
        .ok_or(LiveQuotaError::InvalidResponse)?;
    let window_minutes = raw
        .window_duration_mins
        .map(|minutes| {
            if minutes <= 0 || minutes > MAX_DURATION_MINUTES {
                Err(LiveQuotaError::InvalidResponse)
            } else {
                u64::try_from(minutes).map_err(|_| LiveQuotaError::InvalidResponse)
            }
        })
        .transpose()?;
    let resets_at = raw
        .resets_at
        .map(|seconds| {
            if !(MIN_RESET_EPOCH..=MAX_RESET_EPOCH).contains(&seconds) {
                return Err(LiveQuotaError::InvalidResponse);
            }
            DateTime::<Utc>::from_timestamp(seconds, 0).ok_or(LiveQuotaError::InvalidResponse)
        })
        .transpose()?;
    Ok(LiveQuotaWindow {
        used_percent,
        window_minutes,
        resets_at,
    })
}

fn parse_credits(raw: &RawCredits) -> Result<LiveQuotaCredits, LiveQuotaError> {
    let balance = raw
        .balance
        .as_deref()
        .map(|value| {
            let number = value
                .parse::<f64>()
                .map_err(|_| LiveQuotaError::InvalidResponse)?;
            if !number.is_finite() || !(0.0..=MAX_CREDIT_BALANCE).contains(&number) {
                return Err(LiveQuotaError::InvalidResponse);
            }
            Ok(number)
        })
        .transpose()?;
    Ok(LiveQuotaCredits {
        has_credits: raw.has_credits,
        unlimited: raw.unlimited,
        balance,
    })
}

#[derive(Deserialize)]
struct RpcHeader {
    jsonrpc: Option<String>,
    id: Option<u64>,
    method: Option<String>,
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    jsonrpc: Option<String>,
    id: Option<u64>,
    result: Option<T>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    code: i64,
}

#[derive(Deserialize)]
struct EmptyObject {}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountReadResult {
    requires_openai_auth: bool,
    account: Option<AccountInfo>,
    workspace_routing: Option<WorkspaceRouting>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AccountInfo {
    #[serde(rename = "type")]
    kind: String,
    plan_type: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceRouting {
    chatgpt_account_id: String,
}

struct AccountMetadata {
    plan_type: Option<String>,
    chatgpt_account_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RateLimitsResponse {
    ordinary_usage_allowed: Option<bool>,
    account_id: Option<String>,
    rate_limits_by_limit_id: Option<BTreeMap<String, RawLimitBucket>>,
    rate_limits: Option<RawLimitBucket>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLimitBucket {
    spend_control_reached: Option<bool>,
    limit_id: Option<String>,
    limit_name: Option<String>,
    primary: Option<RawWindow>,
    secondary: Option<RawWindow>,
    credits: Option<RawCredits>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawWindow {
    used_percent: Option<serde_json::Number>,
    window_duration_mins: Option<i64>,
    resets_at: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCredits {
    has_credits: bool,
    unlimited: bool,
    balance: Option<String>,
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn stalled_pipe_readers_cannot_accumulate_without_bound() {
        let counter = AtomicUsize::new(0);
        let mut permits = (0..MAX_PIPE_READERS)
            .map(|_| ReaderPermit::acquire(&counter).unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            ReaderPermit::acquire(&counter),
            Err(LiveQuotaError::LaunchFailed)
        ));
        permits.pop();
        assert!(ReaderPermit::acquire(&counter).is_ok());
        drop(permits);
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }

    fn response(value: serde_json::Value) -> RateLimitsResponse {
        serde_json::from_value(value).expect("synthetic response deserializes")
    }

    #[test]
    fn maps_keep_bucket_identity_windows_and_explicit_zero_credits() {
        let parsed = response(serde_json::json!({
            "accountId": "acct_test_01",
            "rateLimitsByLimitId": {
                "codex": {
                    "limitId": "codex",
                    "limitName": "Codex",
                    "primary": { "usedPercent": 0, "windowDurationMins": 300, "resetsAt": 1_800_000_000 },
                    "secondary": { "usedPercent": 47, "windowDurationMins": 10080, "resetsAt": 1_800_500_000 },
                    "credits": { "hasCredits": true, "unlimited": false, "balance": "0" }
                },
                "workspace": {
                    "limitId": "workspace",
                    "limitName": null,
                    "primary": null,
                    "secondary": null,
                    "credits": { "hasCredits": true, "unlimited": true, "balance": null }
                }
            }
        }));
        let buckets = parse_limit_buckets(&parsed).expect("buckets validate");
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].limit_id, "codex");
        assert_eq!(buckets[0].primary.as_ref().unwrap().used_percent, 0.0);
        assert_eq!(
            buckets[0].secondary.as_ref().unwrap().window_minutes,
            Some(10_080)
        );
        assert_eq!(buckets[0].credits.as_ref().unwrap().balance, Some(0.0));
        assert!(buckets[1].credits.as_ref().unwrap().unlimited);
    }

    #[test]
    fn present_empty_multibucket_map_is_not_replaced_by_legacy_bucket() {
        let parsed = response(serde_json::json!({
            "accountId": "acct_test_01",
            "rateLimitsByLimitId": {},
            "rateLimits": { "limitId": "unrelated", "primary": { "usedPercent": 5 } }
        }));
        assert_eq!(
            parse_limit_buckets(&parsed),
            Err(LiveQuotaError::NoObservation)
        );
    }

    #[test]
    fn legacy_single_bucket_is_used_only_when_multibucket_map_is_absent() {
        let parsed = response(serde_json::json!({
            "accountId": "acct_test_01",
            "rateLimits": {
                "limitId": "codex",
                "primary": { "usedPercent": 12, "windowDurationMins": 300 }
            }
        }));
        let buckets = parse_limit_buckets(&parsed).expect("legacy bucket validates");
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].limit_id, "codex");
        assert_eq!(buckets[0].primary.as_ref().unwrap().used_percent, 12.0);
    }

    #[test]
    fn missing_or_malformed_identity_never_falls_back_to_other_metadata() {
        assert_eq!(
            checked_account_id(None),
            Err(LiveQuotaError::InvalidResponse)
        );
        assert_eq!(
            checked_account_id(Some("  ")),
            Err(LiveQuotaError::InvalidResponse)
        );
        assert_eq!(
            checked_account_id(Some("acct\nleak")),
            Err(LiveQuotaError::InvalidResponse)
        );
    }

    #[test]
    fn invalid_percent_duration_reset_and_credit_values_fail_closed() {
        for value in [-0.1, 100.1, f64::NAN, f64::INFINITY] {
            let raw = RawWindow {
                used_percent: serde_json::Number::from_f64(value),
                window_duration_mins: Some(300),
                resets_at: None,
            };
            assert_eq!(parse_window(&raw), Err(LiveQuotaError::InvalidResponse));
        }
        let zero_duration = RawWindow {
            used_percent: Some(serde_json::Number::from(0)),
            window_duration_mins: Some(0),
            resets_at: None,
        };
        assert_eq!(
            parse_window(&zero_duration),
            Err(LiveQuotaError::InvalidResponse)
        );
        let bad_reset = RawWindow {
            used_percent: Some(serde_json::Number::from(50)),
            window_duration_mins: None,
            resets_at: Some(-1),
        };
        assert_eq!(
            parse_window(&bad_reset),
            Err(LiveQuotaError::InvalidResponse)
        );
        let bad_credits = RawCredits {
            has_credits: true,
            unlimited: false,
            balance: Some("NaN".into()),
        };
        assert_eq!(
            parse_credits(&bad_credits),
            Err(LiveQuotaError::InvalidResponse)
        );
    }

    #[test]
    fn payload_free_error_codes_are_stable() {
        assert_eq!(LiveQuotaError::AuthExpired.code(), "auth_expired");
        assert_eq!(LiveQuotaError::RateLimited.code(), "rate_limited");
        assert_eq!(LiveQuotaError::AccountChanged.code(), "account_changed");
        assert_eq!(map_rpc_error(-32601), LiveQuotaError::Unsupported);
        assert_eq!(map_rpc_error(429), LiveQuotaError::RateLimited);
        assert_eq!(map_rpc_error(-32000), LiveQuotaError::ProviderOutage);
    }

    #[test]
    fn jsonrpc_accepts_codex_omitted_version_and_rejects_other_versions() {
        assert_eq!(validate_jsonrpc_version(None), Ok(()));
        assert_eq!(validate_jsonrpc_version(Some("2.0")), Ok(()));
        assert_eq!(
            validate_jsonrpc_version(Some("1.0")),
            Err(LiveQuotaError::ProtocolMismatch)
        );
    }

    #[test]
    fn requires_openai_auth_does_not_reject_valid_chatgpt_account() {
        let result: AccountReadResult = serde_json::from_value(serde_json::json!({
            "requiresOpenaiAuth": true,
            "account": {
                "type": "chatgpt",
                "email": "intentionally ignored",
                "planType": "plus"
            },
            "workspaceRouting": {
                "accountRoutingOverride": "NO_CONSTRAINT",
                "backendOrigin": "ignored",
                "chatgptAccountId": "acct_synthetic"
            }
        }))
        .expect("synthetic app-server account response");
        let metadata = account_metadata(result).expect("authenticated account metadata");
        assert_eq!(metadata.plan_type.as_deref(), Some("plus"));
        assert_eq!(
            metadata.chatgpt_account_id.as_deref(),
            Some("acct_synthetic")
        );
    }

    #[test]
    fn quota_read_requires_local_account_identity_to_match_approval() {
        assert_eq!(
            validate_approved_local_account(Some("acct_b"), "acct_a"),
            Err(LiveQuotaError::AccountChanged)
        );
        assert_eq!(
            validate_approved_local_account(None, "acct_a"),
            Err(LiveQuotaError::Unsupported)
        );
        assert_eq!(
            validate_approved_local_account(Some("acct_a"), "acct_a"),
            Ok(())
        );
    }
    #[cfg(unix)]
    pub(crate) fn fake_app_server(
        directory: &Path,
        account: &str,
        rates: &str,
    ) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = directory.join("synthetic-codex");
        // Fixtures contain only test-owned JSON. The trace records method names,
        // not credentials; production never writes this trace.
        let script = format!(
            r#"#!/bin/sh
IFS= read -r line
printf '%s\n' '{{"id":1,"result":{{}}}}'
IFS= read -r line
IFS= read -r line
printf '%s\n' '{account}'
if IFS= read -r line; then
  printf '%s' "$line" > "$0.quota-request"
  printf '%s\n' '{rates}'
fi
"#
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn synthetic_stdio_driver_checks_local_identity_before_requesting_quota() {
        let dir = tempfile::tempdir().unwrap();
        let rates = r#"{"id":3,"result":{"accountId":"approved","rateLimits":{"limitId":"codex","primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":1893456000}},"rateLimitsByLimitId":null}}"#;
        for local in [None, Some("different"), Some("approved")] {
            let account = serde_json::json!({"id":2,"result":{"requiresOpenaiAuth":true,"account":{"type":"chatgpt","planType":"pro"},"workspaceRouting":local.map(|id| serde_json::json!({"chatgptAccountId":id}))}}).to_string();
            let executable = fake_app_server(dir.path(), &account, rates);
            let trace = executable.with_extension("quota-request");
            let result = read_codex_quota(&executable, "approved");
            match local {
                None => assert_eq!(result.unwrap_err(), LiveQuotaError::Unsupported),
                Some("different") => {
                    assert_eq!(result.unwrap_err(), LiveQuotaError::AccountChanged)
                }
                _ => {
                    assert_eq!(
                        result.unwrap().limit_buckets[0]
                            .primary
                            .as_ref()
                            .unwrap()
                            .used_percent,
                        25.0
                    );
                    assert!(std::fs::read_to_string(&trace)
                        .unwrap()
                        .contains("account/rateLimits/read"));
                }
            }
            if local != Some("approved") {
                assert!(
                    !trace.exists(),
                    "must not send a quota request with another account's credentials"
                );
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn synthetic_unresponsive_child_is_stopped_at_the_deadline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("unresponsive");
        std::fs::write(
            &executable,
            "#!/bin/sh\nIFS= read -r first\nIFS= read -r second\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_millis(100);
        let mut server = AppServer::spawn(&executable, deadline).unwrap();
        assert_eq!(
            server.initialize(deadline).unwrap_err(),
            LiveQuotaError::Timeout
        );
        drop(server);
        assert!(started.elapsed() < Duration::from_secs(3));
    }
}
