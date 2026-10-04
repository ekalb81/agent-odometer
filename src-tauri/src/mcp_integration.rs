//! Explicit MCP-entry setup. Preview data never contains a client's full config.
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Mutex, OnceLock,
    },
    time::{Duration, Instant},
};
use toml_edit::{DocumentMut, Item, Table};

const OWNER: &str = "odometer-mcp-v1";
const MAX_PLANS: usize = 8;
const PLAN_TTL: Duration = Duration::from_secs(300);
static VERSION_READERS: AtomicUsize = AtomicUsize::new(0);
struct VersionReaderPermit;
impl Drop for VersionReaderPermit {
    fn drop(&mut self) {
        VERSION_READERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Client {
    Codex,
    ClaudeCode,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    User,
    Project,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    Install,
    Remove,
    Restore,
}

#[derive(Debug, Clone, Serialize)]
pub struct Preview {
    pub id: String,
    pub client: Client,
    pub scope: Scope,
    pub action: Change,
    pub configuration_path: String,
    pub entry_preview: String,
    pub warning: &'static str,
}
struct Plan {
    preview: Preview,
    original: Option<Vec<u8>>,
    updated: Vec<u8>,
    created: Instant,
}
struct Applied {
    original: Option<Vec<u8>>,
    updated: Vec<u8>,
    backup: Option<PathBuf>,
}
static PLANS: OnceLock<Mutex<HashMap<String, Plan>>> = OnceLock::new();
static APPLIED: OnceLock<Mutex<HashMap<PathBuf, Applied>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn config_path(client: Client, scope: Scope, project: Option<&Path>) -> Result<PathBuf> {
    match scope {
        Scope::User => match client {
            Client::Codex => Ok(crate::config::codex_home_dir().join("config.toml")),
            Client::ClaudeCode => {
                if let Some(root) =
                    std::env::var_os("CLAUDE_CONFIG_DIR").filter(|value| !value.is_empty())
                {
                    Ok(PathBuf::from(root).join(".claude.json"))
                } else {
                    Ok(dirs::home_dir()
                        .context("home directory unavailable")?
                        .join(".claude.json"))
                }
            }
        },
        Scope::Project => {
            let project = project.context("Select a project directory for project scope.")?;
            if !project.is_absolute() || !project.is_dir() {
                bail!("Project scope requires an existing absolute directory.");
            }
            Ok(match client {
                Client::Codex => project.join(".codex/config.toml"),
                Client::ClaudeCode => project.join(".mcp.json"),
            })
        }
    }
}

fn reject_link_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        match std::fs::symlink_metadata(parent) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => bail!("Configuration parent must be a regular directory; use manual setup for linked roots."),
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn entry(executable: &Path) -> Value {
    json!({"command":executable.to_string_lossy(),"args":["mcp"],"env":{"ODOMETER_MANAGED_INTEGRATION":OWNER}})
}
fn owned(entry: &Value) -> bool {
    entry["env"]["ODOMETER_MANAGED_INTEGRATION"] == OWNER
}

/// Read only the selected entry. The rest of the config never reaches IPC.
pub fn inspect(path: &Path, client: Client, executable: &Path) -> Result<(bool, bool)> {
    reject_link_parent(path)?;
    let bytes = crate::harness_integration::read_optional_config(path)?;
    let existing = read_entry(bytes.as_deref(), client)?;
    Ok((
        existing.as_ref().is_some_and(owned),
        existing.as_ref().is_some_and(|value| {
            value["command"] == executable.to_string_lossy().as_ref()
                && value["args"] == json!(["mcp"])
                && supported_options(value)
        }),
    ))
}

fn supported_options(value: &Value) -> bool {
    value.get("cwd").is_none()
        && value.get("env_vars").is_none()
        && value["unsupported_environment"] != true
        && value["enabled"] != false
        && value.get("env").is_none_or(|env| {
            env.as_object()
                .is_some_and(|env| env.keys().all(|key| key == "ODOMETER_MANAGED_INTEGRATION"))
        })
}

fn read_entry(bytes: Option<&[u8]>, client: Client) -> Result<Option<Value>> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    match client {
        Client::Codex => {
            let document: DocumentMut = std::str::from_utf8(bytes)?
                .parse()
                .context("Invalid Codex configuration TOML.")?;
            if document
                .get("mcp_servers")
                .is_some_and(|item| !item.is_table_like())
            {
                bail!("mcp_servers must be a table.");
            }
            let Some(selected) = document
                .get("mcp_servers")
                .and_then(|item| item.get("odometer"))
            else {
                return Ok(None);
            };
            if !selected.is_table_like() {
                bail!("Odometer MCP entry must be a table.");
            }
            let args = selected.get("args").and_then(Item::as_array).map(|values| {
                values
                    .iter()
                    .map(|value| value.as_str().map(Value::from).unwrap_or(Value::Null))
                    .collect::<Vec<_>>()
            });
            let unsupported_environment = selected.get("env").is_some_and(|env| {
                env.as_table_like().is_none_or(|table| {
                    table
                        .iter()
                        .any(|(key, _)| key != "ODOMETER_MANAGED_INTEGRATION")
                })
            }) || selected.get("env_vars").is_some()
                || selected.get("cwd").is_some();
            Ok(Some(
                json!({"command":selected.get("command").and_then(Item::as_str), "args":args, "enabled":selected.get("enabled").and_then(Item::as_bool), "unsupported_environment":unsupported_environment, "env":{"ODOMETER_MANAGED_INTEGRATION":selected.get("env").and_then(|env| env.get("ODOMETER_MANAGED_INTEGRATION")).and_then(Item::as_str)}}),
            ))
        }
        Client::ClaudeCode => {
            let root: Value =
                serde_json::from_slice(bytes).context("Invalid Claude Code configuration JSON.")?;
            if !root.is_object()
                || root
                    .get("mcpServers")
                    .is_some_and(|value| !value.is_object())
            {
                bail!("mcpServers must be an object.");
            }
            Ok(root["mcpServers"].get("odometer").cloned())
        }
    }
}

fn edit(bytes: Option<&[u8]>, client: Client, action: Change, expected: &Value) -> Result<Vec<u8>> {
    let existing = read_entry(bytes, client)?;
    if existing.as_ref().is_some_and(|value| !owned(value)) {
        bail!("The existing odometer entry is not managed by Odometer. Preserve it and use manual setup or choose another client/scope.");
    }
    if action == Change::Remove && existing.is_none() {
        bail!("No managed Odometer entry exists in this scope.");
    }
    match client {
        Client::Codex => {
            let mut document: DocumentMut = std::str::from_utf8(bytes.unwrap_or(b""))?.parse()?;
            if document.get("mcp_servers").is_none() {
                document["mcp_servers"] = Item::Table(Table::new());
            }
            let servers = document["mcp_servers"]
                .as_table_like_mut()
                .context("mcp_servers must be a table")?;
            if action == Change::Remove {
                servers.remove("odometer");
            } else {
                let mut table = servers
                    .get("odometer")
                    .cloned()
                    .unwrap_or_else(|| Item::Table(Table::new()));
                table["command"] =
                    toml_edit::value(expected["command"].as_str().context("missing executable")?);
                let mut args = toml_edit::Array::new();
                args.push("mcp");
                table["args"] = toml_edit::value(args);
                if table.get("env").is_none() {
                    table["env"] = Item::Table(Table::new());
                }
                table["env"]
                    .as_table_like_mut()
                    .context("Managed entry env must be a table")?
                    .insert("ODOMETER_MANAGED_INTEGRATION", toml_edit::value(OWNER));
                servers.insert("odometer", table);
            }
            Ok(document.to_string().into_bytes())
        }
        Client::ClaudeCode => {
            let mut root: Value = serde_json::from_slice(bytes.unwrap_or(b"{}"))?;
            let object = root
                .as_object_mut()
                .context("Client configuration must be an object")?;
            let servers = object
                .entry("mcpServers")
                .or_insert_with(|| json!({}))
                .as_object_mut()
                .context("mcpServers must be an object")?;
            if action == Change::Remove {
                servers.remove("odometer");
            } else {
                let mut value = servers
                    .get("odometer")
                    .cloned()
                    .unwrap_or_else(|| expected.clone());
                value["command"] = expected["command"].clone();
                value["args"] = expected["args"].clone();
                value["type"] = json!("stdio");
                servers.insert("odometer".into(), value);
            }
            let mut output = serde_json::to_vec_pretty(&root)?;
            output.push(b'\n');
            Ok(output)
        }
    }
}

pub fn preview(
    client: Client,
    scope: Scope,
    action: Change,
    project: Option<&Path>,
) -> Result<Preview> {
    let path = config_path(client, scope, project)?;
    preview_at(
        client,
        scope,
        action,
        &path,
        &crate::harness_integration::integration_executable()?,
    )
}
fn preview_at(
    client: Client,
    scope: Scope,
    action: Change,
    path: &Path,
    executable: &Path,
) -> Result<Preview> {
    reject_link_parent(path)?;
    let original = crate::harness_integration::read_optional_config(path)?;
    if action == Change::Install
        && read_entry(original.as_deref(), client)?
            .as_ref()
            .is_some_and(|value| owned(value) && !supported_options(value))
    {
        bail!("Managed entry has custom environment, working-directory or disabled options. Repair those manually, or preview removal and set up again. Odometer will not silently discard them.");
    }
    let expected = entry(executable);
    let (updated, shown) = if action == Change::Restore {
        let history = APPLIED
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("Integration history unavailable"))?;
        let applied = history.get(path).context("Restore is available only for this app session. The timestamped backup remains available for manual recovery after restart.")?;
        if original.as_ref() != Some(&applied.updated) {
            bail!("Configuration changed after Odometer applied it. Preserve newer edits and restore manually.");
        }
        let bytes = applied.original.clone().context(
            "There was no original configuration. Use Remove to undo the installed entry.",
        )?;
        if applied
            .backup
            .as_ref()
            .and_then(|path| {
                crate::harness_integration::read_optional_config(path)
                    .ok()
                    .flatten()
            })
            .as_ref()
            != Some(&bytes)
        {
            bail!("The original backup changed or is missing. Preserve newer edits and restore manually.");
        }
        (bytes, "Restore the unchanged timestamped configuration backup. Unrelated values are intentionally hidden.".into())
    } else {
        (
            edit(original.as_deref(), client, action, &expected)?,
            if action == Change::Remove {
                "Remove only the managed odometer MCP entry.".into()
            } else if client == Client::Codex {
                String::from_utf8(edit(None, client, Change::Install, &expected)?)?
            } else {
                serde_json::to_string_pretty(&expected)?
            },
        )
    };
    let preview = Preview { id: NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string(), client, scope, action, configuration_path: path.to_string_lossy().into_owned(), entry_preview: shown, warning: "Restart the client and start a fresh task. Codex project settings require project trust; Claude Code project MCP may require approval. A saved entry does not prove connection or real use." };
    let mut plans = PLANS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("Integration previews unavailable"))?;
    plans.retain(|_, plan| plan.created.elapsed() < PLAN_TTL);
    if plans.len() >= MAX_PLANS {
        bail!("Too many pending previews. Apply or refresh after five minutes.");
    }
    plans.insert(
        preview.id.clone(),
        Plan {
            preview: preview.clone(),
            original,
            updated,
            created: Instant::now(),
        },
    );
    Ok(preview)
}

#[derive(Debug, Serialize)]
pub struct ApplyResult {
    pub configuration_path: String,
    pub backup_path: Option<String>,
    pub restart_required: bool,
}

#[derive(Debug, Serialize)]
pub struct ClientCard {
    pub client: Client,
    pub scope: Scope,
    pub configuration_path: String,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub installed: bool,
    pub configured: bool,
    pub managed: bool,
    pub backup_path: Option<String>,
    pub restore_available: bool,
    pub diagnostic: Option<crate::integration_status::Diagnostic>,
    pub manual_command: String,
    pub instructions: String,
}

#[derive(Debug, Serialize)]
pub struct CenterReport {
    pub schema_version: u32,
    pub status: crate::integration_status::IntegrationStatus,
    pub cards: Vec<ClientCard>,
    pub activity: Vec<crate::integration_activity::Activity>,
    pub activity_available: bool,
    /// Self-reported MCP names/activity cannot attest an authenticated agent task.
    pub supported_client_task_proof: &'static str,
}

pub fn center(
    scope: Scope,
    project: Option<&Path>,
    desktop_scan_complete: bool,
    history_ready: bool,
) -> Result<CenterReport> {
    let config = crate::config::Config::load_read_only()?;
    let rates = crate::rates::RateCard::load_from_disk()
        .or_else(|_| crate::rates::RateCard::load_bundled())
        .unwrap_or_default();
    let control = crate::query_control::QueryControl::default();
    let store = history_ready
        .then(|| {
            crate::history_store::HistoryStore::default_path().and_then(|path| {
                crate::history_store::HistoryStore::open_read_only(&path, control.clone())
            })
        })
        .and_then(Result::ok);
    let mut status = crate::integration_status::status(
        store.as_ref(),
        &rates,
        &config,
        chrono::Utc::now(),
        Some(&control),
    )?;
    status.scan_status = if desktop_scan_complete {
        "desktop_scan_complete"
    } else {
        "scan_in_progress"
    };
    if !desktop_scan_complete {
        status
            .diagnostics
            .push(crate::integration_status::DiagnosticCode::ScanInProgress.diagnostic());
    }
    let cards = cards(scope, project)?;
    let activity = crate::integration_activity::recent();
    Ok(CenterReport {
        schema_version: 1,
        status,
        cards,
        activity_available: activity.is_ok(),
        supported_client_task_proof: "not_verified",
        activity: activity.unwrap_or_default(),
    })
}

pub fn test(
    client: Client,
    scope: Scope,
    project: Option<&Path>,
) -> Result<crate::verify::VerifyReport> {
    let path = config_path(client, scope, project)?;
    let executable = crate::harness_integration::integration_executable()?;
    let (_, configured) = inspect(&path, client, &executable)?;
    if !configured {
        bail!("integration_not_configured: Preview the selected scope and start a fresh client task after setup.");
    }
    Ok(crate::verify::verify_in_directory(
        &executable,
        project,
        chrono::Utc::now(),
    ))
}

pub fn cards(scope: Scope, project: Option<&Path>) -> Result<Vec<ClientCard>> {
    let server = crate::harness_integration::integration_executable()?;
    [Client::Codex, Client::ClaudeCode]
        .into_iter()
        .map(|client| {
            let path = config_path(client, scope, project)?;
            let inspected = inspect(&path, client, &server);
            let (managed, configured) = inspected.as_ref().copied().unwrap_or_default();
            let executable = discover(client);
            let version = executable
                .as_ref()
                .and_then(|path| client_version(path).ok());
            let history = APPLIED
                .get_or_init(Default::default)
                .lock()
                .map_err(|_| anyhow::anyhow!("Integration history unavailable"))?;
            let previous = history.get(&path);
            let instructions = instructions(client);
            let command = if cfg!(windows) {
                format!("& '{}' mcp", server.to_string_lossy().replace('\'', "''"))
            } else {
                format!(
                    "'{}' mcp",
                    server.to_string_lossy().replace('\'', "'\"'\"'")
                )
            };
            Ok(ClientCard {
                client,
                scope,
                configuration_path: path.to_string_lossy().into_owned(),
                executable: executable
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                installed: version.is_some(),
                version,
                configured,
                managed,
                backup_path: previous
                    .and_then(|value| value.backup.as_ref())
                    .map(|path| path.to_string_lossy().into_owned()),
                restore_available: previous.is_some_and(|value| value.original.is_some()),
                diagnostic: (!configured || inspected.is_err()).then(|| {
                    crate::integration_status::DiagnosticCode::IntegrationNotConfigured.diagnostic()
                }),
                manual_command: command,
                instructions,
            })
        })
        .collect()
}

fn discover(client: Client) -> Option<PathBuf> {
    let name = match client {
        Client::Codex => "codex",
        Client::ClaudeCode => "claude",
    };
    let mut candidates = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .filter(|root| root.is_absolute())
        .map(|root| {
            root.join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            })
        })
        .collect::<Vec<_>>();
    if cfg!(windows) {
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(PathBuf::from(root).join("Programs/OpenAI/Codex/bin/codex.exe"));
        }
        if let Some(root) = dirs::home_dir() {
            candidates.push(root.join(format!(".local/bin/{name}.exe")));
        }
    }
    candidates.into_iter().find(|path| {
        path.is_absolute()
            && path.is_file()
            && (client == Client::Codex || path.file_stem().is_some_and(|name| name == "claude"))
    })
}

fn client_version(path: &Path) -> Result<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    let mut readers = VERSION_READERS.load(Ordering::Acquire);
    loop {
        if readers >= 4 {
            bail!("Client version reader capacity unavailable");
        }
        match VERSION_READERS.compare_exchange_weak(
            readers,
            readers + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => break,
            Err(current) => readers = current,
        }
    }
    let permit = VersionReaderPermit;
    let mut command = Command::new(path);
    command
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    let output = child
        .stdout
        .take()
        .context("Client version output unavailable")?;
    let (send, receive) = std::sync::mpsc::channel();
    // A descendant may retain stdout after the direct child exits. Detach the
    // bounded reader rather than joining forever; the permit caps orphan readers.
    std::thread::spawn(move || {
        let _permit = permit;
        let mut bytes = Vec::new();
        let result = output.take(4097).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    let output = receive.recv_timeout(Duration::from_secs(2));
    let _ = child.kill();
    let status = child.wait();
    let bytes = output.context("Client version check timed out")??;
    if bytes.len() > 4096 || !status?.success() {
        bail!("Client version check failed");
    }
    let text = std::str::from_utf8(&bytes)?;
    let version = text
        .split_whitespace()
        .find(|word| {
            word.len() <= 32
                && word.contains('.')
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || byte == b'.' || byte == b'-')
        })
        .context("Client version was not recognized")?;
    Ok(version.to_owned())
}

pub fn instructions(client: Client) -> String {
    let tailored = match client {
        Client::Codex => {
            "Codex: installation guide https://developers.openai.com/codex/cli . Restart and start a new task; project config loads only after project trust."
        }
        Client::ClaudeCode => {
            "Claude Code: installation guide https://code.claude.com/docs/en/quickstart . Restart and start a new task; approve project MCP when requested."
        }
    };
    format!("{tailored}\nUse Odometer's read-only analytics to inspect observed agent usage. Call odometer_status first when readiness or coverage is uncertain. Use session_report before referring to a session key; do not request raw transcripts through MCP. Preserve missing/fallback pricing, currencies, cost surfaces and quota provenance. Cohort optimization signals are observational, not causal proof.\nSample task: Call odometer_status, then usage_report for the last complete UTC week and workflow_metrics for the same window. Explain missing coverage and propose one measurable experiment without claiming a demonstrated speedup.\nGeneric client: configure a stdio MCP server named odometer with the displayed executable argument vector. No HTTP endpoint is required.")
}
pub fn apply(id: &str) -> Result<ApplyResult> {
    let plan = PLANS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("Integration previews unavailable"))?
        .remove(id)
        .context("Preview expired or was already applied. Preview again.")?;
    if plan.created.elapsed() >= PLAN_TTL {
        bail!("Preview expired. Preview again.");
    }
    let path = PathBuf::from(&plan.preview.configuration_path);
    reject_link_parent(&path)?;
    let backup = crate::harness_integration::apply_mcp_config(
        &path,
        plan.original.clone(),
        plan.updated.clone(),
    )?;
    let result = ApplyResult {
        configuration_path: plan.preview.configuration_path,
        backup_path: backup
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        restart_required: true,
    };
    let mut history = APPLIED
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("Integration history unavailable"))?;
    if history.len() >= MAX_PLANS && !history.contains_key(&path) {
        history.clear();
    }
    history.insert(
        path,
        Applied {
            original: plan.original,
            updated: plan.updated,
            backup,
        },
    );
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn managed_setup_preserves_secrets_and_refuses_concurrent_edits() {
        let directory = tempfile::tempdir().unwrap();
        for (client, name, input) in [
            (Client::ClaudeCode, "claude.json", r#"{"secret":"synthetic unrelated secret","mcpServers":{"other":{"command":"keep"}}}"#),
            (Client::Codex, "config.toml", "# keep comment\nsecret = 'synthetic unrelated secret'\n[mcp_servers.other]\ncommand = 'keep'\n"),
        ] {
            let path = directory.path().join(name); std::fs::write(&path, input).unwrap();
            let preview = preview_at(client, Scope::User, Change::Install, &path, Path::new("synthetic-odometer")).unwrap();
            assert!(!preview.entry_preview.contains("secret")); assert!(!preview.entry_preview.contains("other"));
            let result = apply(&preview.id).unwrap(); assert!(result.backup_path.is_some());
            let installed = std::fs::read(&path).unwrap(); assert!(String::from_utf8_lossy(&installed).contains("synthetic unrelated secret"));
            assert_eq!(inspect(&path, client, Path::new("synthetic-odometer")).unwrap(), (true, true));
            let remove = preview_at(client, Scope::User, Change::Remove, &path, Path::new("synthetic-odometer")).unwrap();
            std::fs::write(&path, b"a concurrent user edit").unwrap();
            assert!(apply(&remove.id).is_err()); assert_eq!(std::fs::read(&path).unwrap(), b"a concurrent user edit");
            assert_eq!(std::fs::read(result.backup_path.unwrap()).unwrap(), input.as_bytes());
        }
    }
    #[test]
    fn unrelated_odometer_entry_is_never_taken_over_or_removed() {
        let existing = br#"{"mcpServers":{"odometer":{"command":"other-server","env":{"API_KEY":"synthetic"}}}}"#;
        for action in [Change::Install, Change::Remove] {
            assert!(edit(
                Some(existing),
                Client::ClaudeCode,
                action,
                &entry(Path::new("synthetic"))
            )
            .is_err());
        }
    }

    #[test]
    fn restore_preserves_original_and_rejects_a_changed_backup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let original = b"{\"unrelated\":\"synthetic private value\"}\n";
        std::fs::write(&path, original).unwrap();
        let install = preview_at(
            Client::ClaudeCode,
            Scope::User,
            Change::Install,
            &path,
            Path::new("synthetic"),
        )
        .unwrap();
        apply(&install.id).unwrap();
        let restore = preview_at(
            Client::ClaudeCode,
            Scope::User,
            Change::Restore,
            &path,
            Path::new("synthetic"),
        )
        .unwrap();
        assert!(!restore.entry_preview.contains("private value"));
        apply(&restore.id).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let install = preview_at(
            Client::ClaudeCode,
            Scope::User,
            Change::Install,
            &path,
            Path::new("synthetic"),
        )
        .unwrap();
        let applied = apply(&install.id).unwrap();
        std::fs::write(applied.backup_path.unwrap(), b"synthetic changed backup").unwrap();
        assert!(preview_at(
            Client::ClaudeCode,
            Scope::User,
            Change::Restore,
            &path,
            Path::new("synthetic")
        )
        .is_err());
    }

    #[test]
    fn repair_preserves_inline_toml_options_and_declines_custom_environment() {
        let original = br#"mcp_servers = { odometer = { command = "old", args = ["mcp"], custom = "retain", env = { ODOMETER_MANAGED_INTEGRATION = "odometer-mcp-v1", API_KEY = "synthetic-secret" } } }"#;
        let updated = edit(
            Some(original),
            Client::Codex,
            Change::Install,
            &entry(Path::new("synthetic")),
        )
        .unwrap();
        let text = String::from_utf8_lossy(&updated);
        assert!(text.contains("retain"));
        assert!(text.contains("synthetic-secret"));
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(&path, &updated).unwrap();
        assert_eq!(
            inspect(&path, Client::Codex, Path::new("synthetic")).unwrap(),
            (true, false)
        );
        assert!(preview_at(
            Client::Codex,
            Scope::User,
            Change::Install,
            &path,
            Path::new("synthetic")
        )
        .unwrap_err()
        .to_string()
        .contains("Repair those manually"));
        assert_eq!(std::fs::read(&path).unwrap(), updated);
        assert!(preview_at(
            Client::Codex,
            Scope::User,
            Change::Remove,
            &path,
            Path::new("synthetic")
        )
        .is_ok());
    }

    #[test]
    fn project_scope_requires_an_existing_absolute_directory_and_selects_only_client_config() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            config_path(Client::Codex, Scope::Project, Some(directory.path())).unwrap(),
            directory.path().join(".codex/config.toml")
        );
        assert_eq!(
            config_path(Client::ClaudeCode, Scope::Project, Some(directory.path())).unwrap(),
            directory.path().join(".mcp.json")
        );
        assert!(config_path(Client::Codex, Scope::Project, None).is_err());
        assert!(config_path(Client::Codex, Scope::Project, Some(Path::new("."))).is_err());
        assert!(config_path(
            Client::ClaudeCode,
            Scope::Project,
            Some(&directory.path().join("absent"))
        )
        .is_err());
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn version_timeout_does_not_join_a_descendant_holding_stdout() {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic-client");
        std::fs::write(
            &path,
            b"#!/bin/sh\nsleep 4 &\nprintf 'synthetic-client 1.2.3\\n'\nexit 0\n",
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = Instant::now();
        assert!(client_version(&path)
            .unwrap_err()
            .to_string()
            .contains("timed out"));
        assert!(started.elapsed() < Duration::from_millis(3500));
    }
}
