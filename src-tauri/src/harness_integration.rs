use crate::config::{claude_config_dir, codex_home_dir, Config};
use crate::model::Harness;
use crate::provider::{claude_code_provider_id, codex_provider_id};
use crate::turn_receipts::{load_run_record, HookRunRecord};
use anyhow::{anyhow, Context};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use toml_edit::{value, ArrayOfTables, DocumentMut, Item, Table};

const INTEGRATION_ID: &str = "odometer-turn-receipts-v1";
const STATUS_MESSAGE: &str = "Calculating Odometer turn receipt";
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct HarnessIntegrationStatus {
    pub requested: bool,
    pub configured: bool,
    pub receipt_observed: bool,
    pub config_source: String,
    pub config_path: String,
    pub diagnostic_code: String,
    pub detail: String,
    pub restart_recommended: bool,
    pub trust_review_recommended: bool,
    pub last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_run_success: Option<bool>,
    pub last_receipt: Option<String>,
    pub last_run_detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TurnReceiptIntegrationStatus {
    pub enabled: bool,
    pub executable_path: String,
    pub codex: HarnessIntegrationStatus,
    pub claude_code: HarnessIntegrationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigSource {
    CodexHooksJson,
    CodexInlineToml,
    ClaudeSettingsJson,
}

impl ConfigSource {
    fn code(self) -> &'static str {
        match self {
            Self::CodexHooksJson => "codex_hooks_json",
            Self::CodexInlineToml => "codex_inline_toml",
            Self::ClaudeSettingsJson => "claude_settings_json",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::CodexHooksJson => "Codex hooks.json",
            Self::CodexInlineToml => "the existing inline Codex config",
            Self::ClaudeSettingsJson => "Claude Code user settings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum JsonHookSpec {
    Codex { command: String },
    Claude { executable: String },
}

impl JsonHookSpec {
    fn codex(command: &str) -> Self {
        Self::Codex {
            command: command.to_owned(),
        }
    }

    fn claude(executable: &Path) -> Self {
        Self::Claude {
            executable: executable.to_string_lossy().into_owned(),
        }
    }

    fn handler(&self) -> Value {
        match self {
            Self::Codex { command } => json!({
                "type": "command",
                "command": command,
                "timeout": 5,
                "statusMessage": STATUS_MESSAGE
            }),
            Self::Claude { executable } => json!({
                "type": "command",
                "command": executable,
                "args": ["hook", "claude", "--integration-id", INTEGRATION_ID],
                "timeout": 5,
                "statusMessage": STATUS_MESSAGE
            }),
        }
    }

    fn is_current(&self, handler: &Value) -> bool {
        if handler.get("type").and_then(Value::as_str) != Some("command")
            || handler.get("timeout").and_then(Value::as_u64) != Some(5)
            || handler.get("statusMessage").and_then(Value::as_str) != Some(STATUS_MESSAGE)
            || !handler
                .get("async")
                .is_none_or(|value| value.as_bool() == Some(false))
        {
            return false;
        }
        match self {
            Self::Codex { command } => {
                if handler.get("command").and_then(Value::as_str) != Some(command) {
                    return false;
                }
                ["commandWindows", "command_windows"]
                    .into_iter()
                    .filter_map(|key| handler.get(key))
                    .all(|value| value.as_str() == Some(command))
            }
            Self::Claude { executable } => {
                handler.get("command").and_then(Value::as_str) == Some(executable)
                    && handler.get("if").is_none()
                    && handler.get("asyncRewake").is_none()
                    && handler.get("args")
                        == Some(&json!([
                            "hook",
                            "claude",
                            "--integration-id",
                            INTEGRATION_ID
                        ]))
            }
        }
    }
}

#[derive(Debug)]
struct SourceInspectionError {
    source: ConfigSource,
    path: PathBuf,
    error: anyhow::Error,
}

#[derive(Debug)]
struct PlannedWrite {
    path: PathBuf,
    original: Option<Vec<u8>>,
    updated: Vec<u8>,
}

#[derive(Debug)]
struct AppliedWrite {
    path: PathBuf,
    original: Option<Vec<u8>>,
    applied: Vec<u8>,
    backup: Option<PathBuf>,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
struct HookInspection {
    owned_count: usize,
    current_count: usize,
}

impl HookInspection {
    fn owned(self) -> bool {
        self.owned_count > 0
    }

    fn current(self) -> bool {
        self.current_count > 0
    }
}

#[derive(Debug, Clone, Copy)]
struct CodexInspection {
    json: HookInspection,
    inline: HookInspection,
    inline_hooks_present: bool,
    hooks_disabled: bool,
}

/// Rollback guard spanning hook-file writes and the subsequent Odometer config
/// save. If either side fails, harness configuration is restored unless it was
/// edited again after Odometer applied its change.
pub struct IntegrationTransaction {
    applied: Vec<AppliedWrite>,
    committed: bool,
}

impl IntegrationTransaction {
    pub fn commit(mut self) -> anyhow::Result<()> {
        let mut failures = Vec::new();
        for write in &self.applied {
            if let Err(error) = finalize_backup(write) {
                failures.push(error.to_string());
            }
        }
        // A commit-time cleanup or preservation warning must not make Drop
        // roll back the already-saved app configuration and installed hooks.
        self.committed = true;
        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(failures.join("; ")))
        }
    }

    pub fn abort(mut self) -> anyhow::Result<()> {
        let result = restore_applied(&self.applied);
        self.committed = true;
        result
    }
}

impl Drop for IntegrationTransaction {
    fn drop(&mut self) {
        if !self.committed {
            if let Err(error) = restore_applied(&self.applied) {
                tracing::warn!("{error}");
            }
        }
    }
}

pub fn receipt_settings_changed(previous: &Config, next: &Config) -> bool {
    previous.turn_receipts_enabled != next.turn_receipts_enabled
        || previous.turn_receipts_codex != next.turn_receipts_codex
        || previous.turn_receipts_claude != next.turn_receipts_claude
}

/// Installs or removes only Odometer-owned Stop-hook handlers. Existing hook
/// groups and unrelated settings are retained. Codex keeps using an existing
/// Odometer source; otherwise an existing inline hook table wins over creating
/// a second hooks.json source.
pub fn sync(config: &Config) -> anyhow::Result<IntegrationTransaction> {
    let executable = integration_executable()?;
    sync_at(config, &executable, &codex_home_dir(), &claude_config_dir())
}

fn sync_at(
    config: &Config,
    executable: &Path,
    codex_home: &Path,
    claude_config: &Path,
) -> anyhow::Result<IntegrationTransaction> {
    let codex_enabled = config.turn_receipts_enabled && config.turn_receipts_codex;
    let claude_enabled = config.turn_receipts_enabled && config.turn_receipts_claude;
    let codex_command = codex_hook_command(executable);
    let mut plans = plan_codex_hook_files(codex_home, &codex_command, codex_enabled)?;
    let claude_spec = JsonHookSpec::claude(executable);
    if let Some(plan) = plan_json_hook_file(
        &claude_config.join("settings.json"),
        &claude_spec,
        claude_enabled,
    )? {
        plans.push(plan);
    }
    apply_plans(plans)
}

fn apply_plans(plans: Vec<PlannedWrite>) -> anyhow::Result<IntegrationTransaction> {
    // Validate every source before the first mutation. This keeps an edit made
    // after inspection from being silently overwritten by a multi-file setup.
    for plan in &plans {
        ensure_unchanged(plan)?;
    }

    let mut applied = Vec::new();
    for plan in plans {
        // Keep the check adjacent to replacement as well; another process may
        // have edited a later file while an earlier plan was being applied.
        let backup = match ensure_unchanged(&plan).and_then(|()| {
            replace_if_unchanged(&plan.path, plan.original.as_deref(), &plan.updated)
        }) {
            Ok(backup) => backup,
            Err(error) => {
                return match restore_applied(&applied) {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(anyhow!("{error}; {rollback_error}")),
                };
            }
        };
        applied.push(AppliedWrite {
            path: plan.path,
            original: plan.original,
            applied: plan.updated,
            backup,
        });
    }
    Ok(IntegrationTransaction {
        applied,
        committed: false,
    })
}

fn ensure_unchanged(plan: &PlannedWrite) -> anyhow::Result<()> {
    let current = read_optional_config(&plan.path)?;
    if current == plan.original {
        return Ok(());
    }
    Err(anyhow!(
        "configuration_changed: {} changed while Odometer was preparing receipt hooks; refresh status and try again",
        plan.path.display()
    ))
}

fn restore_applied(applied: &[AppliedWrite]) -> anyhow::Result<()> {
    let mut failures = Vec::new();
    for write in applied.iter().rev() {
        // Never roll back over a newer user edit. The next explicit Repair can
        // reconcile any remaining Odometer entry without discarding that edit.
        let result = if let Some(backup) = &write.backup {
            restore_from_backup_if_unchanged(&write.path, backup, &write.applied)
        } else {
            remove_if_unchanged(&write.path, &write.applied)
        };
        if let Err(error) = result {
            failures.push(format!("{}: {error}", write.path.display()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(anyhow!(
            "receipt-hook rollback was incomplete: {}",
            failures.join("; ")
        ))
    }
}

fn finalize_backup(write: &AppliedWrite) -> anyhow::Result<()> {
    let (Some(backup), Some(expected)) = (&write.backup, &write.original) else {
        return Ok(());
    };
    let parent = backup
        .parent()
        .ok_or_else(|| anyhow!("receipt-hook recovery path has no parent"))?;
    let detached = vacant_recovery_path(parent)?;
    atomic_install_new(&detached, backup).with_context(|| {
        format!(
            "could not detach receipt-hook recovery file {} for final verification",
            backup.display()
        )
    })?;
    let current = match read_regular_file(&detached) {
        Ok(current) => current,
        Err(error) => {
            return Err(combine_operation_and_rollback(
                error.context("could not verify detached receipt-hook recovery file"),
                restore_detached_file(&detached, backup),
            ));
        }
    };
    if current != *expected {
        sync_regular_file(&detached)?;
        let preserved = match restore_detached_file(&detached, backup) {
            Ok(()) => backup,
            Err(error) => {
                return Err(anyhow!(
                    "a concurrent receipt-hook edit was preserved at {}, but its original recovery name could not be restored: {error}",
                    detached.display()
                ));
            }
        };
        return Err(anyhow!(
            "a concurrent receipt-hook edit was preserved at {}; the installed configuration was not overwritten",
            preserved.display()
        ));
    }
    std::fs::remove_file(&detached).with_context(|| {
        format!(
            "could not remove receipt-hook recovery file {}",
            detached.display()
        )
    })?;
    sync_parent_directory(&detached)?;
    Ok(())
}

pub fn status(config: &Config) -> TurnReceiptIntegrationStatus {
    let executable = integration_executable().unwrap_or_else(|_| PathBuf::from("agent-odometer"));
    let codex_command = codex_hook_command(&executable);
    let claude_spec = JsonHookSpec::claude(&executable);
    TurnReceiptIntegrationStatus {
        enabled: config.turn_receipts_enabled,
        executable_path: executable.to_string_lossy().into_owned(),
        codex: codex_status(
            config.turn_receipts_enabled && config.turn_receipts_codex,
            &codex_home_dir(),
            &codex_command,
        ),
        claude_code: json_status(
            claude_code_provider_id(),
            config.turn_receipts_enabled && config.turn_receipts_claude,
            ConfigSource::ClaudeSettingsJson,
            &claude_config_dir().join("settings.json"),
            &claude_spec,
        ),
    }
}

fn integration_executable() -> anyhow::Result<PathBuf> {
    let current = std::env::current_exe().context("could not locate the Odometer executable")?;
    Ok(resolve_stable_launcher(
        &current,
        std::env::var_os("APPIMAGE").as_deref(),
        std::env::var_os("APPDIR").as_deref(),
    ))
}

fn resolve_stable_launcher(
    current: &Path,
    appimage: Option<&std::ffi::OsStr>,
    appdir: Option<&std::ffi::OsStr>,
) -> PathBuf {
    let (Some(appimage), Some(appdir)) = (appimage, appdir) else {
        return current.to_path_buf();
    };
    let appimage = Path::new(appimage);
    let appdir = Path::new(appdir);
    if !appimage.is_absolute() || !appimage.is_file() || !appdir.is_absolute() || !appdir.is_dir() {
        return current.to_path_buf();
    }
    let inside_appdir = current
        .canonicalize()
        .ok()
        .zip(appdir.canonicalize().ok())
        .is_some_and(|(current, appdir)| current.starts_with(appdir));
    if inside_appdir {
        appimage.to_path_buf()
    } else {
        current.to_path_buf()
    }
}

fn codex_status(
    requested: bool,
    codex_home: &Path,
    expected_command: &str,
) -> HarnessIntegrationStatus {
    let json_path = codex_home.join("hooks.json");
    let inline_path = codex_home.join("config.toml");
    let inspected = inspect_codex_sources(&json_path, &inline_path, expected_command);
    let inspection = match inspected {
        Ok(inspection) => inspection,
        Err(failure) => {
            return error_status(
                codex_provider_id(),
                requested,
                failure.source,
                &failure.path,
                failure.error,
            )
        }
    };
    let source = preferred_codex_source(inspection);
    let path = match source {
        ConfigSource::CodexHooksJson => &json_path,
        ConfigSource::CodexInlineToml => &inline_path,
        ConfigSource::ClaudeSettingsJson => unreachable!(),
    };
    build_status(
        codex_provider_id(),
        requested,
        source,
        path,
        HookInspection {
            owned_count: inspection.json.owned_count + inspection.inline.owned_count,
            current_count: inspection.json.current_count + inspection.inline.current_count,
        },
        inspection.hooks_disabled,
        &[path, &inline_path],
    )
}

fn json_status(
    harness: Harness,
    requested: bool,
    source: ConfigSource,
    path: &Path,
    spec: &JsonHookSpec,
) -> HarnessIntegrationStatus {
    let inspection = read_optional_config(path).and_then(|bytes| {
        let Some(bytes) = bytes else {
            return Ok((HookInspection::default(), false));
        };
        let root = parse_json(path, &bytes)?;
        let inspection = inspect_json_value(path, &root, spec)?;
        let hooks_disabled = match root.get("disableAllHooks") {
            None => false,
            Some(value) => value
                .as_bool()
                .ok_or_else(|| anyhow!("{}.disableAllHooks must be a boolean", path.display()))?,
        };
        Ok((inspection, hooks_disabled))
    });
    match inspection {
        Ok((inspection, hooks_disabled)) => build_status(
            harness,
            requested,
            source,
            path,
            inspection,
            hooks_disabled,
            &[path],
        ),
        Err(error) => error_status(harness, requested, source, path, error),
    }
}

fn build_status(
    harness: Harness,
    requested: bool,
    source: ConfigSource,
    path: &Path,
    inspection: HookInspection,
    hooks_disabled: bool,
    freshness_paths: &[&Path],
) -> HarnessIntegrationStatus {
    let HookRunRecord {
        last_run_at,
        success,
        last_receipt,
        detail: last_run_detail,
    } = load_run_record(harness.clone());
    let configured = inspection.current();
    let observation_is_current = observation_is_current(last_run_at, freshness_paths);
    let duplicate_or_stale = inspection.owned_count > 1
        || (inspection.owned_count > 0 && inspection.current_count < inspection.owned_count);
    let receipt_observed = requested
        && configured
        && !hooks_disabled
        && !duplicate_or_stale
        && observation_is_current
        && success;
    let recent_failure = requested
        && configured
        && !hooks_disabled
        && observation_is_current
        && last_run_at.is_some()
        && !success;

    let (diagnostic_code, detail) = if !requested && inspection.owned() {
        (
            "hook_cleanup_needed",
            "An Odometer hook remains configured. Save or repair the disabled setup to remove it."
                .to_owned(),
        )
    } else if !requested {
        (
            "hook_not_requested",
            "Off. Harness configuration is unchanged.".to_owned(),
        )
    } else if hooks_disabled {
        let detail = if harness == codex_provider_id() {
            "Configured, but Codex hooks are disabled in config.toml. Enable [features].hooks (or remove the deprecated [features].codex_hooks = false), review /hooks, then start a fresh task."
        } else if harness == claude_code_provider_id() {
            "Configured, but disableAllHooks is true in Claude Code user settings. Re-enable hooks, inspect /hooks, then start a fresh task."
        } else {
            // Neutral fallback for a provider without dedicated guidance text.
            "Configured, but hooks are disabled for this provider. Re-enable them, review /hooks, then start a fresh task."
        };
        ("hooks_disabled", detail.to_owned())
    } else if !configured && inspection.owned() {
        (
            "hook_stale",
            "The Odometer hook points to a different executable. Use Repair setup.".to_owned(),
        )
    } else if !configured {
        (
            "hook_missing",
            "Enabled in Odometer, but the hook is not configured. Use Repair setup.".to_owned(),
        )
    } else if duplicate_or_stale {
        (
            "hook_duplicate",
            "More than one Odometer hook entry was found. Use Repair setup to keep one current handler."
                .to_owned(),
        )
    } else if recent_failure {
        (
            "receipt_failed",
            "Configured, but the latest receipt attempt failed. Review the bounded error below and try a fresh task."
                .to_owned(),
        )
    } else if receipt_observed {
        (
            "receipt_observed",
            "Configured, and a receipt from this configuration was observed.".to_owned(),
        )
    } else {
        let guidance = if harness == codex_provider_id() {
            "Review and trust the command in /hooks, then start a fresh task to observe a receipt."
        } else if harness == claude_code_provider_id() {
            "Requires Claude Code 2.1.139 or later. Inspect it in /hooks, then start a fresh CLI or local Desktop task to observe a receipt."
        } else {
            // Neutral fallback for a provider without dedicated guidance text.
            "Inspect it in /hooks, then start a fresh task to observe a receipt."
        };
        (
            "awaiting_receipt",
            format!("Configured in {}. {guidance}", source.description()),
        )
    };

    HarnessIntegrationStatus {
        requested,
        configured,
        receipt_observed,
        config_source: source.code().to_owned(),
        config_path: path.to_string_lossy().into_owned(),
        diagnostic_code: diagnostic_code.to_owned(),
        detail,
        restart_recommended: requested && configured && !receipt_observed,
        // Codex-specific trust-review guidance; other providers never trigger it.
        trust_review_recommended: harness == codex_provider_id()
            && requested
            && configured
            && !receipt_observed,
        last_run_at,
        last_run_success: last_run_at.map(|_| success),
        last_receipt,
        last_run_detail,
    }
}

fn observation_is_current(
    last_run_at: Option<chrono::DateTime<chrono::Utc>>,
    freshness_paths: &[&Path],
) -> bool {
    let modified_at = freshness_paths
        .iter()
        .filter_map(|path| {
            std::fs::metadata(path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from)
        })
        .max();
    matches!((last_run_at, modified_at), (Some(run), Some(changed)) if run >= changed)
}

fn error_status(
    harness: Harness,
    requested: bool,
    source: ConfigSource,
    path: &Path,
    error: anyhow::Error,
) -> HarnessIntegrationStatus {
    let HookRunRecord {
        last_run_at,
        success,
        last_receipt,
        detail: last_run_detail,
    } = load_run_record(harness);
    HarnessIntegrationStatus {
        requested,
        configured: false,
        receipt_observed: false,
        config_source: source.code().to_owned(),
        config_path: path.to_string_lossy().into_owned(),
        diagnostic_code: "configuration_invalid".to_owned(),
        detail: format!("Cannot inspect configuration: {error}"),
        restart_recommended: false,
        trust_review_recommended: false,
        last_run_at,
        last_run_success: last_run_at.map(|_| success),
        last_receipt,
        last_run_detail,
    }
}

fn codex_hook_command(executable: &Path) -> String {
    let path = executable.to_string_lossy();
    #[cfg(windows)]
    let quoted = format!("\"{}\"", path.replace('"', ""));
    #[cfg(not(windows))]
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    format!("{quoted} hook codex --integration-id {INTEGRATION_ID}")
}

fn plan_codex_hook_files(
    codex_home: &Path,
    command: &str,
    enabled: bool,
) -> anyhow::Result<Vec<PlannedWrite>> {
    let json_path = codex_home.join("hooks.json");
    let inline_path = codex_home.join("config.toml");
    let json_spec = JsonHookSpec::codex(command);
    let json_original = read_optional_config(&json_path)?;
    let inline_original = read_optional_config(&inline_path)?;
    let mut plans = Vec::new();

    if !enabled {
        if let Some(plan) = plan_json_hook_bytes(&json_path, json_original, &json_spec, false)? {
            plans.push(plan);
        }
        if let Some(plan) = plan_toml_hook_bytes(&inline_path, inline_original, command, false)? {
            plans.push(plan);
        }
        return Ok(plans);
    }

    let json_inspection = inspect_optional_json(&json_path, json_original.as_deref(), &json_spec)?;
    let (inline_inspection, inline_hooks_present, hooks_disabled) =
        inspect_optional_toml(&inline_path, inline_original.as_deref(), command)?;
    let inspected = CodexInspection {
        json: json_inspection,
        inline: inline_inspection,
        inline_hooks_present,
        hooks_disabled,
    };
    let preferred = preferred_codex_source(inspected);

    match preferred {
        ConfigSource::CodexHooksJson => {
            if let Some(plan) = plan_json_hook_bytes(&json_path, json_original, &json_spec, true)? {
                plans.push(plan);
            }
            if let Some(plan) = plan_toml_hook_bytes(&inline_path, inline_original, command, false)?
            {
                plans.push(plan);
            }
        }
        ConfigSource::CodexInlineToml => {
            if let Some(plan) = plan_toml_hook_bytes(&inline_path, inline_original, command, true)?
            {
                plans.push(plan);
            }
            if let Some(plan) = plan_json_hook_bytes(&json_path, json_original, &json_spec, false)?
            {
                plans.push(plan);
            }
        }
        ConfigSource::ClaudeSettingsJson => unreachable!(),
    }
    Ok(plans)
}

fn preferred_codex_source(inspection: CodexInspection) -> ConfigSource {
    if inspection.inline.current() && !inspection.json.current() {
        ConfigSource::CodexInlineToml
    } else if inspection.json.current() {
        ConfigSource::CodexHooksJson
    } else if inspection.inline.owned() && !inspection.json.owned() {
        ConfigSource::CodexInlineToml
    } else if inspection.json.owned() {
        ConfigSource::CodexHooksJson
    } else if inspection.inline_hooks_present {
        ConfigSource::CodexInlineToml
    } else {
        ConfigSource::CodexHooksJson
    }
}

fn inspect_codex_sources(
    json_path: &Path,
    inline_path: &Path,
    expected_command: &str,
) -> Result<CodexInspection, SourceInspectionError> {
    let json = read_optional_config(json_path).map_err(|error| SourceInspectionError {
        source: ConfigSource::CodexHooksJson,
        path: json_path.to_path_buf(),
        error,
    })?;
    let json = inspect_optional_json(
        json_path,
        json.as_deref(),
        &JsonHookSpec::codex(expected_command),
    )
    .map_err(|error| SourceInspectionError {
        source: ConfigSource::CodexHooksJson,
        path: json_path.to_path_buf(),
        error,
    })?;
    let inline = read_optional_config(inline_path).map_err(|error| SourceInspectionError {
        source: ConfigSource::CodexInlineToml,
        path: inline_path.to_path_buf(),
        error,
    })?;
    let (inline, inline_hooks_present, hooks_disabled) =
        inspect_optional_toml(inline_path, inline.as_deref(), expected_command).map_err(
            |error| SourceInspectionError {
                source: ConfigSource::CodexInlineToml,
                path: inline_path.to_path_buf(),
                error,
            },
        )?;
    Ok(CodexInspection {
        json,
        inline,
        inline_hooks_present,
        hooks_disabled,
    })
}

fn plan_json_hook_file(
    path: &Path,
    spec: &JsonHookSpec,
    enabled: bool,
) -> anyhow::Result<Option<PlannedWrite>> {
    plan_json_hook_bytes(path, read_optional_config(path)?, spec, enabled)
}

fn plan_json_hook_bytes(
    path: &Path,
    original: Option<Vec<u8>>,
    spec: &JsonHookSpec,
    enabled: bool,
) -> anyhow::Result<Option<PlannedWrite>> {
    if original.is_none() && !enabled {
        return Ok(None);
    }
    let mut root = match &original {
        Some(bytes) => parse_json(path, bytes)?,
        None => Value::Object(Map::new()),
    };
    let inspection = inspect_json_value(path, &root, spec)?;
    if (enabled && inspection.owned_count == 1 && inspection.current_count == 1)
        || (!enabled && !inspection.owned())
    {
        return Ok(None);
    }

    let object = root
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", path.display()))?;
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}.hooks must be a JSON object", path.display()))?;
    let stop = hooks
        .entry("Stop")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| anyhow!("{}.hooks.Stop must be an array", path.display()))?;

    for group in stop.iter_mut() {
        let handlers = group
            .get_mut("hooks")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| anyhow!("{}.hooks.Stop[].hooks must be an array", path.display()))?;
        handlers.retain(|handler| !is_odometer_json_handler(handler));
    }
    stop.retain(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|handlers| !handlers.is_empty())
    });
    if enabled {
        stop.push(json!({
            "hooks": [spec.handler()]
        }));
    }
    if stop.is_empty() {
        hooks.remove("Stop");
    }
    if hooks.is_empty() {
        object.remove("hooks");
    }

    let mut updated = serde_json::to_vec_pretty(&root)?;
    updated.push(b'\n');
    if original.as_deref() == Some(updated.as_slice()) {
        return Ok(None);
    }
    Ok(Some(PlannedWrite {
        path: path.to_path_buf(),
        original,
        updated,
    }))
}

fn plan_toml_hook_bytes(
    path: &Path,
    original: Option<Vec<u8>>,
    command: &str,
    enabled: bool,
) -> anyhow::Result<Option<PlannedWrite>> {
    if original.is_none() && !enabled {
        return Ok(None);
    }
    let mut document = match &original {
        Some(bytes) => parse_toml(path, bytes)?,
        None => DocumentMut::new(),
    };
    let inspection = inspect_toml_document(path, &document, command)?;
    if (enabled && inspection.owned_count == 1 && inspection.current_count == 1)
        || (!enabled && !inspection.owned())
    {
        return Ok(None);
    }

    if document.get("hooks").is_none() {
        document["hooks"] = Item::Table(Table::new());
    }
    let hooks = document
        .get_mut("hooks")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| anyhow!("{}.hooks must be a TOML table", path.display()))?;
    if hooks.get("Stop").is_none() {
        hooks.insert("Stop", Item::ArrayOfTables(ArrayOfTables::new()));
    }
    let stop_empty = {
        let stop = hooks
            .get_mut("Stop")
            .and_then(Item::as_array_of_tables_mut)
            .ok_or_else(|| anyhow!("{}.hooks.Stop must be an array of tables", path.display()))?;
        for group in stop.iter_mut() {
            let handlers = group
                .get_mut("hooks")
                .and_then(Item::as_array_of_tables_mut)
                .ok_or_else(|| {
                    anyhow!(
                        "{}.hooks.Stop[].hooks must be an array of tables",
                        path.display()
                    )
                })?;
            handlers.retain(|handler| !is_odometer_toml_handler(handler));
        }
        stop.retain(|group| {
            group
                .get("hooks")
                .and_then(Item::as_array_of_tables)
                .is_some_and(|handlers| !handlers.is_empty())
        });
        if enabled {
            let mut handler = Table::new();
            handler["type"] = value("command");
            handler["command"] = value(command);
            handler["timeout"] = value(5);
            handler["statusMessage"] = value(STATUS_MESSAGE);
            let mut handlers = ArrayOfTables::new();
            handlers.push(handler);
            let mut group = Table::new();
            group.insert("hooks", Item::ArrayOfTables(handlers));
            stop.push(group);
        }
        stop.is_empty()
    };
    if stop_empty {
        hooks.remove("Stop");
    }
    if hooks.is_empty() {
        document.as_table_mut().remove("hooks");
    }

    let updated = document.to_string().into_bytes();
    if original.as_deref() == Some(updated.as_slice()) {
        return Ok(None);
    }
    Ok(Some(PlannedWrite {
        path: path.to_path_buf(),
        original,
        updated,
    }))
}

fn inspect_optional_json(
    path: &Path,
    bytes: Option<&[u8]>,
    spec: &JsonHookSpec,
) -> anyhow::Result<HookInspection> {
    let Some(bytes) = bytes else {
        return Ok(HookInspection::default());
    };
    inspect_json_value(path, &parse_json(path, bytes)?, spec)
}

fn inspect_json_value(
    path: &Path,
    root: &Value,
    spec: &JsonHookSpec,
) -> anyhow::Result<HookInspection> {
    let object = root
        .as_object()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", path.display()))?;
    if matches!(spec, JsonHookSpec::Claude { .. })
        && object
            .get("disableAllHooks")
            .is_some_and(|value| !value.is_boolean())
    {
        return Err(anyhow!(
            "{}.disableAllHooks must be a boolean",
            path.display()
        ));
    }
    let Some(hooks) = object.get("hooks") else {
        return Ok(HookInspection::default());
    };
    let hooks = hooks
        .as_object()
        .ok_or_else(|| anyhow!("{}.hooks must be a JSON object", path.display()))?;
    let Some(stop) = hooks.get("Stop") else {
        return Ok(HookInspection::default());
    };
    let stop = stop
        .as_array()
        .ok_or_else(|| anyhow!("{}.hooks.Stop must be an array", path.display()))?;
    let mut inspection = HookInspection::default();
    for group in stop {
        let handlers = group
            .get("hooks")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("{}.hooks.Stop[].hooks must be an array", path.display()))?;
        for handler in handlers {
            if !is_odometer_json_handler(handler) {
                continue;
            }
            inspection.owned_count += 1;
            if spec.is_current(handler) {
                inspection.current_count += 1;
            }
        }
    }
    Ok(inspection)
}

fn inspect_optional_toml(
    path: &Path,
    bytes: Option<&[u8]>,
    expected_command: &str,
) -> anyhow::Result<(HookInspection, bool, bool)> {
    let Some(bytes) = bytes else {
        return Ok((HookInspection::default(), false, false));
    };
    let document = parse_toml(path, bytes)?;
    let inspection = inspect_toml_document(path, &document, expected_command)?;
    let inline_hooks_present = document.get("hooks").is_some();
    let hooks_disabled = match document.get("features") {
        None => false,
        Some(item) => {
            let features = item
                .as_table_like()
                .ok_or_else(|| anyhow!("{}.features must be a TOML table", path.display()))?;
            let canonical = feature_bool(path, features.get("hooks"), "hooks")?;
            let legacy = feature_bool(path, features.get("codex_hooks"), "codex_hooks")?;
            canonical.or(legacy) == Some(false)
        }
    };
    Ok((inspection, inline_hooks_present, hooks_disabled))
}

fn feature_bool(path: &Path, item: Option<&Item>, key: &str) -> anyhow::Result<Option<bool>> {
    let Some(item) = item else {
        return Ok(None);
    };
    item.as_bool()
        .map(Some)
        .ok_or_else(|| anyhow!("{}.features.{key} must be a boolean", path.display()))
}

fn inspect_toml_document(
    path: &Path,
    document: &DocumentMut,
    expected_command: &str,
) -> anyhow::Result<HookInspection> {
    let Some(hooks) = document.get("hooks") else {
        return Ok(HookInspection::default());
    };
    let hooks = hooks.as_table().ok_or_else(|| {
        if hooks.as_inline_table().is_some() {
            unsupported_inline_hooks(path, "hooks is an inline table")
        } else {
            anyhow!("{}.hooks must be a TOML table", path.display())
        }
    })?;
    let Some(stop) = hooks.get("Stop") else {
        return Ok(HookInspection::default());
    };
    let stop = stop.as_array_of_tables().ok_or_else(|| {
        if stop.as_array().is_some() {
            unsupported_inline_hooks(path, "hooks.Stop is an inline array")
        } else {
            anyhow!("{}.hooks.Stop must be an array of tables", path.display())
        }
    })?;
    let mut inspection = HookInspection::default();
    for group in stop.iter() {
        let handlers_item = group.get("hooks").ok_or_else(|| {
            anyhow!(
                "{}.hooks.Stop[].hooks must be an array of tables",
                path.display()
            )
        })?;
        let handlers = handlers_item.as_array_of_tables().ok_or_else(|| {
            if handlers_item.as_array().is_some() {
                unsupported_inline_hooks(path, "hooks.Stop[].hooks is an inline array")
            } else {
                anyhow!(
                    "{}.hooks.Stop[].hooks must be an array of tables",
                    path.display()
                )
            }
        })?;
        for handler in handlers.iter() {
            if !is_odometer_toml_handler(handler) {
                continue;
            }
            inspection.owned_count += 1;
            if is_current_toml_handler(handler, expected_command) {
                inspection.current_count += 1;
            }
        }
    }
    Ok(inspection)
}

fn unsupported_inline_hooks(path: &Path, shape: &str) -> anyhow::Error {
    anyhow!(
        "{} uses a valid but unsupported inline hook shape ({shape}); Odometer will not rewrite it or create a second hook source. Convert it to [[hooks.Stop]] / [[hooks.Stop.hooks]] or configure the Odometer handler manually",
        path.display()
    )
}

fn parse_json(path: &Path, bytes: &[u8]) -> anyhow::Result<Value> {
    serde_json::from_slice(bytes).with_context(|| format!("{} is not valid JSON", path.display()))
}

fn parse_toml(path: &Path, bytes: &[u8]) -> anyhow::Result<DocumentMut> {
    let raw = std::str::from_utf8(bytes)
        .with_context(|| format!("{} is not valid UTF-8", path.display()))?;
    raw.parse::<DocumentMut>()
        .with_context(|| format!("{} is not valid TOML", path.display()))
}

fn is_odometer_json_handler(value: &Value) -> bool {
    ["command", "commandWindows", "command_windows"]
        .into_iter()
        .filter_map(|key| value.get(key).and_then(Value::as_str))
        .any(command_has_integration_id)
        || value
            .get("args")
            .and_then(Value::as_array)
            .is_some_and(|args| {
                args_have_integration_id(
                    args.iter().map(|argument| argument.as_str().unwrap_or("")),
                )
            })
}

fn is_odometer_toml_handler(table: &Table) -> bool {
    ["command", "commandWindows", "command_windows"]
        .into_iter()
        .filter_map(|key| table.get(key).and_then(Item::as_str))
        .any(command_has_integration_id)
}

fn command_has_integration_id(command: &str) -> bool {
    args_have_integration_id(command.split_ascii_whitespace())
}

fn args_have_integration_id<'a>(arguments: impl Iterator<Item = &'a str>) -> bool {
    let mut arguments = arguments.peekable();
    while let Some(argument) = arguments.next() {
        let argument = unquote_shell_token(argument);
        if argument == "--integration-id" {
            if arguments
                .next()
                .map(unquote_shell_token)
                .is_some_and(|value| value == INTEGRATION_ID)
            {
                return true;
            }
        } else if argument
            .strip_prefix("--integration-id=")
            .map(unquote_shell_token)
            .is_some_and(|value| value == INTEGRATION_ID)
        {
            return true;
        }
    }
    false
}

fn unquote_shell_token(token: &str) -> &str {
    let bytes = token.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes[0] == bytes[bytes.len() - 1] {
        &token[1..token.len() - 1]
    } else {
        token
    }
}

fn is_current_toml_handler(table: &Table, expected_command: &str) -> bool {
    table.get("type").and_then(Item::as_str) == Some("command")
        && table.get("command").and_then(Item::as_str) == Some(expected_command)
        && ["commandWindows", "command_windows"]
            .into_iter()
            .filter_map(|key| table.get(key))
            .all(|value| value.as_str() == Some(expected_command))
        && table.get("timeout").and_then(Item::as_integer) == Some(5)
        && table.get("statusMessage").and_then(Item::as_str) == Some(STATUS_MESSAGE)
        && table
            .get("async")
            .is_none_or(|item| item.as_bool() == Some(false))
}

fn read_optional_config(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "{} is a symbolic link; Odometer will not replace or de-link harness configuration. Configure this receipt hook manually or replace the link with a regular file",
            path.display()
        ));
    }
    if !metadata.file_type().is_file() {
        return Err(anyhow!(
            "{} is not a regular file; Odometer will not replace it",
            path.display()
        ));
    }
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(anyhow!(
            "{} exceeds the {} MiB integration safety limit",
            path.display(),
            MAX_CONFIG_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(path)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(anyhow!(
            "{} exceeds the {} MiB integration safety limit",
            path.display(),
            MAX_CONFIG_BYTES / 1024 / 1024
        ));
    }
    Ok(Some(bytes))
}

fn replace_if_unchanged(
    path: &Path,
    expected: Option<&[u8]>,
    bytes: &[u8],
) -> anyhow::Result<Option<PathBuf>> {
    replace_if_unchanged_with_hooks(path, expected, bytes, || {}, || {})
}

fn replace_if_unchanged_with_hooks<Before, After>(
    path: &Path,
    expected: Option<&[u8]>,
    bytes: &[u8],
    before_replace: Before,
    after_replace: After,
) -> anyhow::Result<Option<PathBuf>>
where
    Before: FnOnce(),
    After: FnOnce(),
{
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("configuration path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let temporary = tempfile::Builder::new()
        .prefix(".odometer-hook-")
        .suffix(".tmp")
        .tempfile_in(parent)
        .with_context(|| {
            format!(
                "could not create a private receipt-hook temporary file beside {}",
                path.display()
            )
        })?
        .into_temp_path();
    #[cfg(windows)]
    if expected.is_some() {
        if let Err(error) = seed_windows_security_template(path, &temporary) {
            return Err(close_temp_after_error(temporary, error));
        }
    }
    let write_result = (|| -> std::io::Result<()> {
        let mut temporary_file = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&temporary)?;
        temporary_file.write_all(bytes)?;
        temporary_file.sync_all()
    })();
    if let Err(error) = write_result {
        return Err(close_temp_after_error(temporary, error.into()));
    }

    let Some(expected) = expected else {
        before_replace();
        if let Err(error) = read_optional_config(path).and_then(|current| {
            if current.is_none() {
                Ok(())
            } else {
                Err(configuration_changed(path))
            }
        }) {
            return Err(close_temp_after_error(temporary, error));
        }
        let temporary = keep_temp_path(temporary)?;
        if let Err(error) = atomic_install_new(path, &temporary) {
            let error = if path_entry_exists(path) {
                configuration_changed(path)
            } else {
                error
            };
            return Err(remove_retained_temp_after_error(&temporary, error));
        }
        if let Err(error) = sync_replacement(path) {
            let rollback = remove_if_unchanged(path, bytes);
            return Err(combine_operation_and_rollback(error, rollback));
        }
        after_replace();
        return Ok(None);
    };

    let expected_metadata = match capture_replacement_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) => return Err(close_temp_after_error(temporary, error)),
    };
    let preparation = (|| -> anyhow::Result<()> {
        prepare_replacement_metadata(path, &temporary)?;
        #[cfg(unix)]
        sync_metadata_file(&temporary)?;
        before_replace();
        if read_optional_config(path)?.as_deref() != Some(expected)
            || capture_replacement_metadata(path)? != expected_metadata
        {
            return Err(configuration_changed(path));
        }
        Ok(())
    })();
    if let Err(error) = preparation {
        return Err(close_temp_after_error(temporary, error));
    }

    let backup_hint = match vacant_recovery_path(parent) {
        Ok(path) => path,
        Err(error) => return Err(close_temp_after_error(temporary, error)),
    };
    let temporary = keep_temp_path(temporary)?;
    let backup = match atomic_replace_existing(path, &temporary, &backup_hint).with_context(|| {
        format!(
            "could not atomically replace receipt-hook configuration {}",
            path.display()
        )
    }) {
        Ok(backup) => backup,
        Err(error) => {
            #[cfg(unix)]
            {
                reconcile_failed_exchange(
                    path,
                    &temporary,
                    expected,
                    &expected_metadata,
                    bytes,
                    error,
                )?
            }
            #[cfg(not(unix))]
            {
                return Err(remove_retained_temp_after_error(&temporary, error));
            }
        }
    };
    let replaced = match read_regular_file(&backup) {
        Ok(replaced) => replaced,
        Err(error) => {
            let rollback = restore_from_backup_if_unchanged(path, &backup, bytes);
            return Err(combine_operation_and_rollback(error, rollback));
        }
    };
    let replaced_metadata = capture_replacement_metadata(&backup);
    if replaced != expected
        || !replaced_metadata.is_ok_and(|metadata| metadata == expected_metadata)
    {
        let rollback = restore_from_backup_if_unchanged(path, &backup, bytes);
        return Err(combine_operation_and_rollback(
            configuration_changed(path),
            rollback,
        ));
    }
    if let Err(error) = sync_replacement(path) {
        let rollback = restore_from_backup_if_unchanged(path, &backup, bytes);
        return Err(combine_operation_and_rollback(error, rollback));
    }
    after_replace();
    Ok(Some(backup))
}

fn close_temp_after_error(temporary: tempfile::TempPath, error: anyhow::Error) -> anyhow::Error {
    let path = temporary.to_path_buf();
    match temporary.close() {
        Ok(()) => error,
        Err(cleanup) => anyhow!(
            "{error}; could not remove private receipt-hook temporary file {}: {cleanup}",
            path.display()
        ),
    }
}

fn keep_temp_path(temporary: tempfile::TempPath) -> anyhow::Result<PathBuf> {
    match temporary.keep() {
        Ok(path) => Ok(path),
        Err(error) => {
            let message = anyhow!(
                "could not prepare private receipt-hook temporary file {} for atomic replacement: {}",
                error.path.display(),
                error.error
            );
            Err(close_temp_after_error(error.path, message))
        }
    }
}

fn remove_retained_temp_after_error(path: &Path, error: anyhow::Error) -> anyhow::Error {
    match std::fs::remove_file(path) {
        Ok(()) => error,
        Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => error,
        Err(cleanup) => anyhow!(
            "{error}; could not remove private receipt-hook temporary file {}: {cleanup}",
            path.display()
        ),
    }
}

#[cfg(any(unix, test))]
fn reconcile_failed_exchange(
    path: &Path,
    retained: &Path,
    expected: &[u8],
    expected_metadata: &ReplacementMetadata,
    applied: &[u8],
    error: anyhow::Error,
) -> anyhow::Result<PathBuf> {
    let active = read_optional_config(path);
    let recovery = read_optional_config(retained);

    if active.as_ref().ok().and_then(Option::as_deref) == Some(applied)
        && recovery.as_ref().ok().and_then(Option::as_deref) == Some(expected)
        && capture_replacement_metadata(retained)
            .is_ok_and(|metadata| &metadata == expected_metadata)
    {
        // Some delegated filesystems can report a rename error after applying
        // the exchange. Treat the observed, fully reconciled state as success
        // so the original remains registered as transaction recovery data.
        return Ok(retained.to_path_buf());
    }

    if active.as_ref().ok().and_then(Option::as_deref) == Some(expected)
        && recovery.as_ref().ok().and_then(Option::as_deref) == Some(applied)
        && capture_replacement_metadata(path).is_ok_and(|metadata| &metadata == expected_metadata)
    {
        return Err(remove_retained_temp_after_error(retained, error));
    }

    let active_detail = active.err().map_or_else(
        || "readable".to_owned(),
        |read| format!("unreadable: {read}"),
    );
    let recovery_detail = recovery.err().map_or_else(
        || "readable".to_owned(),
        |read| format!("unreadable: {read}"),
    );
    Err(anyhow!(
        "{error}; atomic exchange outcome is ambiguous (active path {active_detail}, recovery path {recovery_detail}); Odometer preserved both {} and {} for manual recovery",
        path.display(),
        retained.display()
    ))
}

fn vacant_recovery_path(parent: &Path) -> anyhow::Result<PathBuf> {
    let reservation = tempfile::Builder::new()
        .prefix(".odometer-hook-")
        .suffix(".recovery")
        .tempfile_in(parent)
        .context("could not reserve a private receipt-hook recovery name")?;
    let path = reservation.path().to_path_buf();
    reservation.close().with_context(|| {
        format!(
            "could not release receipt-hook recovery-name reservation {}",
            path.display()
        )
    })?;
    Ok(path)
}

fn combine_operation_and_rollback(
    operation: anyhow::Error,
    rollback: anyhow::Result<()>,
) -> anyhow::Error {
    match rollback {
        Ok(()) => operation,
        Err(rollback) => anyhow!("{operation}; rollback was incomplete: {rollback}"),
    }
}

fn remove_if_unchanged(path: &Path, expected: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("configuration path has no parent"))?;
    let detached = vacant_recovery_path(parent)?;
    atomic_install_new(&detached, path)?;
    let current = match read_regular_file(&detached) {
        Ok(current) => current,
        Err(error) => {
            return Err(combine_operation_and_rollback(
                error,
                restore_detached_file(&detached, path),
            ));
        }
    };
    if current != expected {
        return Err(combine_operation_and_rollback(
            configuration_changed(path),
            restore_detached_file(&detached, path),
        ));
    }
    std::fs::remove_file(&detached)?;
    sync_parent_directory(&detached)?;
    Ok(())
}

fn restore_from_backup_if_unchanged(
    path: &Path,
    backup: &Path,
    expected_active: &[u8],
) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("configuration path has no parent"))?;
    let displaced_hint = vacant_recovery_path(parent)?;
    let displaced = atomic_replace_existing(path, backup, &displaced_hint)?;
    let displaced_bytes = match read_regular_file(&displaced) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Err(combine_operation_and_rollback(
                error,
                undo_restore_swap(path, &displaced, backup),
            ));
        }
    };
    if displaced_bytes != expected_active {
        return Err(combine_operation_and_rollback(
            anyhow!(
                "configuration_changed: {} changed after Odometer updated it; the prior file remains recoverable",
                path.display()
            ),
            undo_restore_swap(path, &displaced, backup),
        ));
    }
    std::fs::remove_file(&displaced).with_context(|| {
        format!(
            "could not remove displaced receipt-hook configuration {}",
            displaced.display()
        )
    })?;
    sync_replacement(path)?;
    Ok(())
}

fn restore_detached_file(detached: &Path, destination: &Path) -> anyhow::Result<()> {
    atomic_install_new(destination, detached).with_context(|| {
        format!(
            "could not restore detached receipt-hook file {} to {}",
            detached.display(),
            destination.display()
        )
    })?;
    sync_replacement(destination)
}

fn undo_restore_swap(path: &Path, displaced: &Path, backup: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("configuration path has no parent"))?;
    let recovery_hint = vacant_recovery_path(parent)?;
    let recovered_original = atomic_replace_existing(path, displaced, &recovery_hint)?;
    if recovered_original != backup {
        restore_detached_file(&recovered_original, backup)?;
    }
    sync_replacement(path)
}

fn read_regular_file(path: &Path) -> anyhow::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() {
        return Err(anyhow!(
            "receipt-hook recovery path {} is not a regular file",
            path.display()
        ));
    }
    Ok(std::fs::read(path)?)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Debug, PartialEq, Eq)]
struct ReplacementMetadata {
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    modified: (i64, i64),
    xattrs: Vec<(std::ffi::OsString, Vec<u8>)>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn capture_replacement_metadata(path: &Path) -> anyhow::Result<ReplacementMetadata> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(path)?;
    Ok(ReplacementMetadata {
        mode: metadata.mode(),
        links: metadata.nlink(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        modified: (metadata.mtime(), metadata.mtime_nsec()),
        xattrs: read_xattrs(path)?,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn read_xattrs(path: &Path) -> anyhow::Result<Vec<(std::ffi::OsString, Vec<u8>)>> {
    let mut attributes = xattr::list(path)?
        .map(|name| {
            let value = xattr::get(path, &name)?.ok_or_else(|| {
                anyhow!(
                    "extended attribute {:?} disappeared while inspecting {}",
                    name,
                    path.display()
                )
            })?;
            Ok((name, value))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    attributes.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(attributes)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Debug, PartialEq, Eq)]
struct ReplacementMetadata;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn capture_replacement_metadata(_path: &Path) -> anyhow::Result<ReplacementMetadata> {
    Ok(ReplacementMetadata)
}

fn sync_regular_file(path: &Path) -> anyhow::Result<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("could not open {} for durability sync", path.display()))?
        .sync_all()
        .with_context(|| format!("could not sync {}", path.display()))
}

#[cfg(unix)]
fn sync_metadata_file(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)
        .with_context(|| format!("could not open {} for metadata sync", path.display()))?
        .sync_all()
        .with_context(|| format!("could not sync metadata for {}", path.display()))
}

fn sync_replacement(path: &Path) -> anyhow::Result<()> {
    sync_regular_file(path)?;
    sync_parent_directory(path)
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("configuration path has no parent"))?;
    std::fs::File::open(parent)
        .with_context(|| format!("could not open {} for durability sync", parent.display()))?
        .sync_all()
        .with_context(|| format!("could not sync {}", parent.display()))
}

#[cfg(windows)]
fn sync_parent_directory(_path: &Path) -> anyhow::Result<()> {
    // Windows does not expose a supported FlushFileBuffers operation for a
    // directory handle. The replacement file itself is flushed explicitly.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_parent_directory(path: &Path) -> anyhow::Result<()> {
    Err(anyhow!(
        "durable receipt-hook replacement is not supported on this platform for {}",
        path.display()
    ))
}

#[cfg(windows)]
fn seed_windows_security_template(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::CopyFileW;

    if std::fs::metadata(source)?.permissions().readonly() {
        return Err(anyhow!(
            "{} is read-only; Odometer will not create a writable replacement",
            source.display()
        ));
    }
    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe { CopyFileW(source_wide.as_ptr(), destination_wide.as_ptr(), 0) };
    if result == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "could not copy the security template from {}",
                source.display()
            )
        });
    }
    Ok(())
}

#[cfg(windows)]
fn atomic_install_new(path: &Path, replacement: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replacement_wide = replacement
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            replacement_wide.as_ptr(),
            path_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn atomic_install_new(path: &Path, replacement: &Path) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let replacement = std::ffi::CString::new(replacement.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            replacement.as_ptr(),
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn atomic_install_new(path: &Path, replacement: &Path) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let replacement = std::ffi::CString::new(replacement.as_os_str().as_bytes())?;
    let result =
        unsafe { libc::renamex_np(replacement.as_ptr(), path.as_ptr(), libc::RENAME_EXCL) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn atomic_install_new(path: &Path, _replacement: &Path) -> anyhow::Result<()> {
    Err(anyhow!(
        "atomic receipt-hook creation is not supported on this Unix platform for {}",
        path.display()
    ))
}

#[cfg(not(any(unix, windows)))]
fn atomic_install_new(path: &Path, _replacement: &Path) -> anyhow::Result<()> {
    Err(anyhow!(
        "atomic receipt-hook creation is not supported on this platform for {}",
        path.display()
    ))
}

#[cfg(windows)]
fn prepare_replacement_metadata(_path: &Path, _temporary: &Path) -> anyhow::Result<()> {
    // ReplaceFileW merges the replaced file's creation time, DACL, security
    // attributes, encryption, compression, and named streams onto the
    // replacement. Passing no IGNORE_* flags makes merge failures fatal.
    Ok(())
}

#[cfg(target_os = "linux")]
fn prepare_replacement_metadata(path: &Path, temporary: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;

    let original = std::fs::symlink_metadata(path)?;
    if original.nlink() != 1 {
        return Err(anyhow!(
            "{} has multiple hard links; Odometer will not break that relationship",
            path.display()
        ));
    }
    if xattr::list(path)?.next().is_some() {
        return Err(anyhow!(
            "{} has extended attributes or ACL metadata that Odometer cannot preserve safely",
            path.display()
        ));
    }
    std::fs::set_permissions(temporary, original.permissions())?;
    let replacement = std::fs::symlink_metadata(temporary)?;
    if original.uid() != replacement.uid()
        || original.gid() != replacement.gid()
        || original.mode() != replacement.mode()
    {
        return Err(anyhow!(
            "{} has owner, group, or mode metadata that Odometer cannot reproduce safely",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn prepare_replacement_metadata(path: &Path, temporary: &Path) -> anyhow::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let original = std::fs::symlink_metadata(path)?;
    if original.nlink() != 1 {
        return Err(anyhow!(
            "{} has multiple hard links; Odometer will not break that relationship",
            path.display()
        ));
    }
    let source = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let destination = std::ffi::CString::new(temporary.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::copyfile(
            source.as_ptr(),
            destination.as_ptr(),
            std::ptr::null_mut(),
            libc::COPYFILE_METADATA,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    std::fs::File::options()
        .write(true)
        .open(temporary)?
        .set_times(std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()))?;
    Ok(())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn prepare_replacement_metadata(path: &Path, _temporary: &Path) -> anyhow::Result<()> {
    Err(anyhow!(
        "automatic receipt-hook replacement is not supported safely on this Unix platform for {}",
        path.display()
    ))
}

#[cfg(windows)]
fn atomic_replace_existing(
    path: &Path,
    replacement: &Path,
    backup: &Path,
) -> anyhow::Result<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    if path_entry_exists(backup) {
        return Err(anyhow!(
            "refusing to overwrite receipt-hook recovery file {}",
            backup.display()
        ));
    }
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let replacement_wide = replacement
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let backup_wide = backup
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        ReplaceFileW(
            path_wide.as_ptr(),
            replacement_wide.as_ptr(),
            backup_wide.as_ptr(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        let error = std::io::Error::last_os_error();
        repair_windows_partial_replace(path, replacement, backup, &error)?;
        return Err(error.into());
    }
    Ok(backup.to_path_buf())
}

#[cfg(windows)]
fn repair_windows_partial_replace(
    path: &Path,
    _replacement: &Path,
    backup: &Path,
    error: &std::io::Error,
) -> anyhow::Result<()> {
    const ERROR_UNABLE_TO_MOVE_REPLACEMENT_2: i32 = 1177;

    if error.raw_os_error() != Some(ERROR_UNABLE_TO_MOVE_REPLACEMENT_2) || path_entry_exists(path) {
        return Ok(());
    }
    if !path_entry_exists(backup) {
        return Err(anyhow!(
            "ReplaceFileW left {} unavailable and did not leave the original at {}; manual recovery is required ({error})",
            path.display(),
            backup.display()
        ));
    }
    atomic_install_new(path, backup).with_context(|| {
        format!(
            "ReplaceFileW left {} unavailable; the original remains at {}, but automatic restoration failed",
            path.display(),
            backup.display()
        )
    })?;
    sync_replacement(path)?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn atomic_replace_existing(
    path: &Path,
    replacement: &Path,
    _backup: &Path,
) -> anyhow::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let replacement_c = std::ffi::CString::new(replacement.as_os_str().as_bytes())?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            replacement_c.as_ptr(),
            libc::AT_FDCWD,
            path.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(replacement.to_path_buf())
}

#[cfg(target_os = "macos")]
fn atomic_replace_existing(
    path: &Path,
    replacement: &Path,
    _backup: &Path,
) -> anyhow::Result<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let path = std::ffi::CString::new(path.as_os_str().as_bytes())?;
    let replacement_c = std::ffi::CString::new(replacement.as_os_str().as_bytes())?;
    let result =
        unsafe { libc::renamex_np(replacement_c.as_ptr(), path.as_ptr(), libc::RENAME_SWAP) };
    if result != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(replacement.to_path_buf())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn atomic_replace_existing(
    path: &Path,
    _replacement: &Path,
    _backup: &Path,
) -> anyhow::Result<PathBuf> {
    Err(anyhow!(
        "atomic receipt-hook replacement is not supported on this Unix platform for {}",
        path.display()
    ))
}

fn path_entry_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn configuration_changed(path: &Path) -> anyhow::Error {
    anyhow!(
        "configuration_changed: {} changed while Odometer was preparing receipt hooks; refresh status and try again",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const COMMAND: &str = "odometer hook codex --integration-id odometer-turn-receipts-v1";
    const OLD_COMMAND: &str = "old-odometer hook codex --integration-id odometer-turn-receipts-v1";

    fn codex_spec() -> JsonHookSpec {
        JsonHookSpec::codex(COMMAND)
    }

    fn enabled_config() -> Config {
        Config {
            turn_receipts_enabled: true,
            turn_receipts_codex: true,
            turn_receipts_claude: false,
            ..Config::default()
        }
    }

    fn apply_and_commit(plans: Vec<PlannedWrite>) {
        apply_plans(plans).unwrap().commit().unwrap();
    }

    #[test]
    fn json_install_and_remove_preserve_unrelated_hooks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(
            &path,
            br#"{"description":"mine","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-me"}]}]}}"#,
        )
        .unwrap();
        let install = plan_json_hook_file(&path, &codex_spec(), true)
            .unwrap()
            .unwrap();
        apply_and_commit(vec![install]);
        assert!(inspect_optional_json(
            &path,
            read_optional_config(&path).unwrap().as_deref(),
            &codex_spec()
        )
        .unwrap()
        .current());
        let installed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(installed.to_string().contains("keep-me"));

        let remove = plan_json_hook_file(&path, &codex_spec(), false)
            .unwrap()
            .unwrap();
        apply_and_commit(vec![remove]);
        let removed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(removed.to_string().contains("keep-me"));
        assert_eq!(removed["description"], "mine");
    }

    #[test]
    fn existing_inline_codex_hooks_remain_inline_and_preserve_comments() {
        let dir = tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let original = r#"# keep this comment
model = "gpt-test"

[[hooks.Stop]]
matcher = ""

[[hooks.Stop.hooks]]
type = "command"
command = "keep-me"
timeout = 12
"#;
        std::fs::write(&config_path, original).unwrap();

        let plans = plan_codex_hook_files(dir.path(), COMMAND, true).unwrap();
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].path, config_path);
        apply_and_commit(plans);

        let updated = std::fs::read_to_string(&config_path).unwrap();
        assert!(updated.contains("# keep this comment"));
        assert!(updated.contains("command = \"keep-me\""));
        assert!(updated.contains(INTEGRATION_ID));
        assert!(!dir.path().join("hooks.json").exists());
        let inspected =
            inspect_codex_sources(&dir.path().join("hooks.json"), &config_path, COMMAND).unwrap();
        assert_eq!(inspected.inline.current_count, 1);
        assert_eq!(
            preferred_codex_source(inspected),
            ConfigSource::CodexInlineToml
        );
    }

    #[test]
    fn existing_odometer_json_source_is_kept_when_inline_hooks_are_added_later() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("hooks.json");
        std::fs::write(
            &json_path,
            format!(
                r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{COMMAND}","timeout":5,"statusMessage":"{STATUS_MESSAGE}"}}]}}]}}}}"#
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"keep-me\"\n",
        )
        .unwrap();

        assert!(plan_codex_hook_files(dir.path(), COMMAND, true)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn duplicate_owned_sources_are_deduplicated_without_touching_user_hooks() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("hooks.json");
        std::fs::write(
            &json_path,
            format!(
                r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{COMMAND}"}},{{"type":"command","command":"keep-json"}}]}}]}}}}"#
            ),
        )
        .unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"{COMMAND}\"\n\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"keep-toml\"\n"
            ),
        )
        .unwrap();

        apply_and_commit(plan_codex_hook_files(dir.path(), COMMAND, true).unwrap());
        let inspected = inspect_codex_sources(&json_path, &config_path, COMMAND).unwrap();
        assert_eq!(inspected.json.current_count, 1);
        assert_eq!(inspected.inline.owned_count, 0);
        assert!(std::fs::read_to_string(json_path)
            .unwrap()
            .contains("keep-json"));
        assert!(std::fs::read_to_string(config_path)
            .unwrap()
            .contains("keep-toml"));
    }

    #[test]
    fn stale_inline_handler_is_repaired_in_place() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            format!(
                "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"{OLD_COMMAND}\"\n"
            ),
        )
        .unwrap();
        apply_and_commit(plan_codex_hook_files(dir.path(), COMMAND, true).unwrap());
        let updated = std::fs::read_to_string(path).unwrap();
        assert!(updated.contains(COMMAND));
        assert!(!updated.contains(OLD_COMMAND));
    }

    #[test]
    fn disabling_removes_owned_handlers_from_both_codex_sources() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("hooks.json");
        std::fs::write(
            &json_path,
            format!(
                r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{COMMAND}"}},{{"type":"command","command":"keep-json"}}]}}]}}}}"#
            ),
        )
        .unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(
            &config_path,
            format!(
                "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"{COMMAND}\"\n\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"keep-toml\"\n"
            ),
        )
        .unwrap();

        apply_and_commit(plan_codex_hook_files(dir.path(), COMMAND, false).unwrap());
        let inspected = inspect_codex_sources(&json_path, &config_path, COMMAND).unwrap();
        assert_eq!(inspected.json.owned_count + inspected.inline.owned_count, 0);
        assert!(std::fs::read_to_string(json_path)
            .unwrap()
            .contains("keep-json"));
        assert!(std::fs::read_to_string(config_path)
            .unwrap()
            .contains("keep-toml"));
    }

    #[test]
    fn invalid_toml_is_never_overwritten() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[[hooks.Stop]\n").unwrap();
        assert!(plan_codex_hook_files(dir.path(), COMMAND, true).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "[[hooks.Stop]\n");
    }

    #[test]
    fn invalid_unselected_claude_config_fails_closed_before_codex_setup() {
        let root = tempdir().unwrap();
        let codex_home = root.path().join("codex");
        let claude_home = root.path().join("claude");
        std::fs::create_dir_all(&claude_home).unwrap();
        std::fs::write(claude_home.join("settings.json"), "not-json").unwrap();
        let error = sync_at(
            &enabled_config(),
            Path::new("odometer"),
            &codex_home,
            &claude_home,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("settings.json is not valid JSON"));
        assert!(!codex_home.join("hooks.json").exists());
        assert_eq!(
            std::fs::read_to_string(claude_home.join("settings.json")).unwrap(),
            "not-json"
        );
    }

    #[test]
    fn claude_existing_hooks_are_preserved_explicitly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            br#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-claude"}]}]}}"#,
        )
        .unwrap();
        let executable = dir.path().join("Odometer App").join("odometer");
        apply_and_commit(vec![plan_json_hook_file(
            &path,
            &JsonHookSpec::claude(&executable),
            true,
        )
        .unwrap()
        .unwrap()]);
        let value: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert!(value.to_string().contains("keep-claude"));
        assert!(value.to_string().contains(INTEGRATION_ID));
        assert_eq!(value["theme"], "dark");
    }

    #[test]
    fn concurrent_change_is_not_overwritten() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(&path, "{}\n").unwrap();
        let plan = plan_json_hook_file(&path, &codex_spec(), true)
            .unwrap()
            .unwrap();
        std::fs::write(&path, "{\"changed\":true}\n").unwrap();
        let error = apply_plans(vec![plan]).err().unwrap().to_string();
        assert!(error.starts_with("configuration_changed:"));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "{\"changed\":true}\n"
        );
    }

    #[test]
    fn dropped_transaction_restores_original_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(&path, "{}\n").unwrap();
        let transaction = apply_plans(vec![plan_json_hook_file(&path, &codex_spec(), true)
            .unwrap()
            .unwrap()])
        .unwrap();
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains(INTEGRATION_ID));
        drop(transaction);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{}\n");
    }

    #[test]
    fn hooks_disabled_is_inspected_without_being_changed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[features]\nhooks = false\n\n[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"keep-me\"\n",
        )
        .unwrap();
        let plans = plan_codex_hook_files(dir.path(), COMMAND, true).unwrap();
        apply_and_commit(plans);
        let (_, _, disabled) = inspect_optional_toml(
            &path,
            read_optional_config(&path).unwrap().as_deref(),
            COMMAND,
        )
        .unwrap();
        assert!(disabled);
        assert!(std::fs::read_to_string(path)
            .unwrap()
            .contains("hooks = false"));
    }

    #[test]
    fn disabled_missing_file_is_untouched() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(plan_json_hook_file(&path, &codex_spec(), false)
            .unwrap()
            .is_none());
        assert!(!path.exists());
    }

    #[test]
    fn disabled_unrelated_config_is_not_reformatted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = br#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-me"}]}]}}"#;
        std::fs::write(&path, original).unwrap();
        assert!(plan_json_hook_file(&path, &codex_spec(), false)
            .unwrap()
            .is_none());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn invalid_existing_json_is_never_overwritten() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"not-json").unwrap();
        assert!(plan_json_hook_file(&path, &codex_spec(), true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not-json");
    }

    #[test]
    fn destination_recreated_during_replace_is_never_overwritten() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(&path, b"original").unwrap();

        let error = replace_if_unchanged_with_hooks(
            &path,
            Some(b"original"),
            b"odometer update",
            || std::fs::write(&path, b"concurrent edit").unwrap(),
            || {},
        )
        .unwrap_err()
        .to_string();

        assert!(error.starts_with("configuration_changed:"));
        assert_eq!(std::fs::read(&path).unwrap(), b"concurrent edit");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_fails_closed_without_delinking() {
        use std::os::unix::fs::symlink;

        let dir = tempdir().unwrap();
        let target = dir.path().join("real-hooks.json");
        let link = dir.path().join("hooks.json");
        std::fs::write(&target, b"{}\n").unwrap();
        symlink(&target, &link).unwrap();

        let error = plan_json_hook_file(&link, &codex_spec(), true)
            .unwrap_err()
            .to_string();
        assert!(error.contains("symbolic link"));
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&target).unwrap(), b"{}\n");
    }

    #[test]
    fn inline_config_error_reports_the_inline_source_and_path() {
        let dir = tempdir().unwrap();
        let inline_path = dir.path().join("config.toml");
        std::fs::write(&inline_path, "[[hooks.Stop]\n").unwrap();

        let status = codex_status(true, dir.path(), COMMAND);
        assert_eq!(status.config_source, "codex_inline_toml");
        assert_eq!(status.config_path, inline_path.to_string_lossy());
        assert_eq!(status.diagnostic_code, "configuration_invalid");
        assert!(status.detail.contains("config.toml is not valid TOML"));
    }

    #[test]
    fn valid_inline_hook_shapes_fail_closed_without_creating_hooks_json() {
        for content in [
            r#"hooks = { Stop = [{ hooks = [{ type = "command", command = "keep-me" }] }] }
"#,
            r#"[hooks]
Stop = [{ hooks = [{ type = "command", command = "keep-me" }] }]
"#,
        ] {
            let dir = tempdir().unwrap();
            let inline_path = dir.path().join("config.toml");
            std::fs::write(&inline_path, content).unwrap();

            let error = plan_codex_hook_files(dir.path(), COMMAND, true)
                .unwrap_err()
                .to_string();
            assert!(error.contains("valid but unsupported inline hook shape"));
            assert!(error.contains("will not rewrite it or create a second hook source"));
            assert_eq!(std::fs::read_to_string(&inline_path).unwrap(), content);
            assert!(!dir.path().join("hooks.json").exists());
        }
    }

    #[test]
    fn claude_handler_uses_direct_exec_and_preserves_path_spaces() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let executable = dir.path().join("Odometer App").join("odometer executable");
        let spec = JsonHookSpec::claude(&executable);
        apply_and_commit(vec![plan_json_hook_file(&path, &spec, true)
            .unwrap()
            .unwrap()]);

        let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let handler = &root["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(
            handler["command"],
            executable.to_string_lossy().into_owned()
        );
        assert_eq!(
            handler["args"],
            json!(["hook", "claude", "--integration-id", INTEGRATION_ID])
        );
        assert!(spec.is_current(handler));
        assert!(!handler["command"].as_str().unwrap().contains('"'));
    }

    #[test]
    fn claude_legacy_shell_handler_is_migrated_to_direct_exec() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let legacy = format!("'old odometer' hook claude --integration-id {INTEGRATION_ID}");
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({
                "hooks": {"Stop": [{"hooks": [{
                    "type": "command",
                    "command": legacy,
                    "timeout": 5,
                    "statusMessage": STATUS_MESSAGE
                }]}]}
            }))
            .unwrap(),
        )
        .unwrap();
        let executable = dir.path().join("new odometer");
        let spec = JsonHookSpec::claude(&executable);

        apply_and_commit(vec![plan_json_hook_file(&path, &spec, true)
            .unwrap()
            .unwrap()]);
        let root: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let inspection = inspect_json_value(&path, &root, &spec).unwrap();
        assert_eq!(inspection.owned_count, 1);
        assert_eq!(inspection.current_count, 1);
        assert!(!root.to_string().contains("old odometer"));
    }

    #[test]
    fn execution_shape_and_windows_override_are_validated() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        let spec = codex_spec();
        let root = json!({
            "hooks": {"Stop": [{"hooks": [{
                "type": "command",
                "command": COMMAND,
                "commandWindows": "different command",
                "timeout": 5,
                "statusMessage": STATUS_MESSAGE
            }, {
                "type": "prompt",
                "command": COMMAND,
                "timeout": 5,
                "statusMessage": STATUS_MESSAGE
            }]}]}
        });
        std::fs::write(&path, serde_json::to_vec(&root).unwrap()).unwrap();

        let inspection = inspect_json_value(&path, &root, &spec).unwrap();
        assert_eq!(inspection.owned_count, 2);
        assert_eq!(inspection.current_count, 0);
        apply_and_commit(vec![plan_json_hook_file(&path, &spec, true)
            .unwrap()
            .unwrap()]);
        let updated: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let inspection = inspect_json_value(&path, &updated, &spec).unwrap();
        assert_eq!(
            inspection,
            HookInspection {
                owned_count: 1,
                current_count: 1
            }
        );
    }

    #[test]
    fn ownership_requires_an_exact_integration_id_argument() {
        let json_path = Path::new("hooks.json");
        let json_root = json!({
            "hooks": {"Stop": [{"hooks": [{
                "type": "command",
                "command": "odometer hook codex --integration-id odometer-turn-receipts-v10"
            }, {
                "type": "command",
                "command": "echo odometer-turn-receipts-v1"
            }, {
                "type": "command",
                "command": "odometer",
                "args": ["hook", "codex", "odometer-turn-receipts-v1"]
            }, {
                "type": "command",
                "command": "odometer",
                "args": ["hook", "codex", "--integration-id", "odometer-turn-receipts-v10"]
            }]}]}
        });
        assert_eq!(
            inspect_json_value(json_path, &json_root, &codex_spec()).unwrap(),
            HookInspection::default()
        );

        let toml_path = Path::new("config.toml");
        let document = parse_toml(
            toml_path,
            br#"[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "odometer hook codex --integration-id odometer-turn-receipts-v10"

[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "echo odometer-turn-receipts-v1"
"#,
        )
        .unwrap();
        assert_eq!(
            inspect_toml_document(toml_path, &document, COMMAND).unwrap(),
            HookInspection::default()
        );

        assert!(command_has_integration_id(&format!(
            "odometer hook codex --integration-id={INTEGRATION_ID}"
        )));
        assert!(command_has_integration_id(&format!(
            "odometer hook codex --integration-id \"{INTEGRATION_ID}\""
        )));
    }

    #[test]
    fn wrong_typed_toml_async_is_repaired() {
        let path = Path::new("config.toml");
        let document = parse_toml(
            path,
            format!(
                "[[hooks.Stop]]\n[[hooks.Stop.hooks]]\ntype = \"command\"\ncommand = \"{COMMAND}\"\ntimeout = 5\nstatusMessage = \"{STATUS_MESSAGE}\"\nasync = \"false\"\n"
            )
            .as_bytes(),
        )
        .unwrap();
        let inspection = inspect_toml_document(path, &document, COMMAND).unwrap();
        assert_eq!(inspection.owned_count, 1);
        assert_eq!(inspection.current_count, 0);
    }

    #[test]
    fn claude_conditions_and_async_rewake_are_not_current() {
        let spec = JsonHookSpec::claude(Path::new("odometer"));
        for extra in [
            json!({"if": "test -f marker"}),
            json!({"asyncRewake": true}),
        ] {
            let mut handler = spec.handler();
            handler
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            assert!(!spec.is_current(&handler));
        }
    }

    #[test]
    fn escaped_integration_ids_are_removed_semantically() {
        let dir = tempdir().unwrap();
        let json_path = dir.path().join("hooks.json");
        let escaped_json = br#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"odometer --integration-id odometer-turn-receipts-v\u0031"}]}]}}"#;
        assert!(!String::from_utf8_lossy(escaped_json).contains(INTEGRATION_ID));
        std::fs::write(&json_path, escaped_json).unwrap();
        apply_and_commit(vec![plan_json_hook_file(&json_path, &codex_spec(), false)
            .unwrap()
            .unwrap()]);
        let root: Value = serde_json::from_slice(&std::fs::read(&json_path).unwrap()).unwrap();
        assert!(!root.to_string().contains(INTEGRATION_ID));

        let toml_path = dir.path().join("config.toml");
        let escaped_toml = r#"[[hooks.Stop]]
[[hooks.Stop.hooks]]
type = "command"
command = "odometer --integration-id odometer-turn-receipts-v\u0031"
"#;
        assert!(!escaped_toml.contains(INTEGRATION_ID));
        std::fs::write(&toml_path, escaped_toml).unwrap();
        apply_and_commit(vec![plan_toml_hook_bytes(
            &toml_path,
            read_optional_config(&toml_path).unwrap(),
            COMMAND,
            false,
        )
        .unwrap()
        .unwrap()]);
        let document = parse_toml(&toml_path, &std::fs::read(&toml_path).unwrap()).unwrap();
        assert!(!inspect_toml_document(&toml_path, &document, COMMAND)
            .unwrap()
            .owned());
    }

    #[test]
    fn codex_feature_alias_and_claude_global_disable_are_honored() {
        let dir = tempdir().unwrap();
        let alias_path = dir.path().join("config.toml");
        std::fs::write(&alias_path, "[features]\ncodex_hooks = false\n").unwrap();
        let (_, _, disabled) = inspect_optional_toml(
            &alias_path,
            read_optional_config(&alias_path).unwrap().as_deref(),
            COMMAND,
        )
        .unwrap();
        assert!(disabled);

        std::fs::write(
            &alias_path,
            "[features]\ncodex_hooks = false\nhooks = true\n",
        )
        .unwrap();
        let (_, _, disabled) = inspect_optional_toml(
            &alias_path,
            read_optional_config(&alias_path).unwrap().as_deref(),
            COMMAND,
        )
        .unwrap();
        assert!(!disabled, "the canonical hooks key must win over its alias");

        let claude_path = dir.path().join("settings.json");
        let spec = JsonHookSpec::claude(Path::new("odometer"));
        std::fs::write(
            &claude_path,
            serde_json::to_vec(&json!({
                "disableAllHooks": true,
                "hooks": {"Stop": [{"hooks": [spec.handler()]}]}
            }))
            .unwrap(),
        )
        .unwrap();
        let status = json_status(
            claude_code_provider_id(),
            true,
            ConfigSource::ClaudeSettingsJson,
            &claude_path,
            &spec,
        );
        assert!(status.configured);
        assert!(!status.receipt_observed);
        assert_eq!(status.diagnostic_code, "hooks_disabled");
        assert!(status.detail.contains("disableAllHooks"));
    }

    #[test]
    fn appimage_launcher_is_used_only_from_its_appdir() {
        let dir = tempdir().unwrap();
        let appimage = dir.path().join("Odometer.AppImage");
        let appdir = dir.path().join("mounted-appdir");
        let inside = appdir.join("usr").join("bin").join("agent-odometer");
        let outside = dir.path().join("agent-odometer");
        std::fs::create_dir_all(inside.parent().unwrap()).unwrap();
        std::fs::write(&appimage, b"appimage").unwrap();
        std::fs::write(&inside, b"inside").unwrap();
        std::fs::write(&outside, b"outside").unwrap();

        assert_eq!(
            resolve_stable_launcher(
                &inside,
                Some(appimage.as_os_str()),
                Some(appdir.as_os_str())
            ),
            appimage
        );
        assert_eq!(
            resolve_stable_launcher(
                &outside,
                Some(appimage.as_os_str()),
                Some(appdir.as_os_str())
            ),
            outside
        );
        assert_eq!(
            resolve_stable_launcher(&inside, Some(appimage.as_os_str()), None),
            inside
        );
    }

    #[test]
    fn edit_through_preopened_handle_is_preserved_at_commit() {
        use std::io::{Seek, SeekFrom};

        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        let original = b"original configuration";
        let applied = b"odometer configuration";
        std::fs::write(&path, original).unwrap();
        let mut old_handle = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();

        let backup = replace_if_unchanged_with_hooks(
            &path,
            Some(original),
            applied,
            || {},
            || {
                old_handle.set_len(0).unwrap();
                old_handle.seek(SeekFrom::Start(0)).unwrap();
                old_handle.write_all(b"concurrent old-handle edit").unwrap();
                old_handle.sync_all().unwrap();
            },
        )
        .unwrap()
        .unwrap();
        drop(old_handle);

        let transaction = IntegrationTransaction {
            applied: vec![AppliedWrite {
                path: path.clone(),
                original: Some(original.to_vec()),
                applied: applied.to_vec(),
                backup: Some(backup.clone()),
            }],
            committed: false,
        };
        let error = transaction.commit().unwrap_err().to_string();
        assert!(error.contains("concurrent receipt-hook edit was preserved"));
        assert_eq!(std::fs::read(&path).unwrap(), applied);
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            b"concurrent old-handle edit"
        );
        std::fs::remove_file(backup).unwrap();
    }

    #[test]
    fn explicit_abort_surfaces_and_preserves_a_later_edit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(&path, b"original").unwrap();
        let transaction = apply_plans(vec![PlannedWrite {
            path: path.clone(),
            original: Some(b"original".to_vec()),
            updated: b"odometer update".to_vec(),
        }])
        .unwrap();
        let backup = transaction.applied[0].backup.clone().unwrap();
        std::fs::write(&path, b"later user edit").unwrap();

        let error = transaction.abort().unwrap_err().to_string();
        assert!(error.contains("configuration_changed"));
        assert_eq!(std::fs::read(&path).unwrap(), b"later user edit");
        assert_eq!(std::fs::read(&backup).unwrap(), b"original");
        std::fs::remove_file(backup).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn metadata_change_during_replace_is_not_discarded() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(&path, b"original").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        let error = replace_if_unchanged_with_hooks(
            &path,
            Some(b"original"),
            b"odometer update",
            || std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap(),
            || {},
        )
        .unwrap_err()
        .to_string();

        assert!(error.starts_with("configuration_changed:"));
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn windows_security_template_is_copied_before_rewrite() {
        let dir = tempdir().unwrap();
        let source = dir.path().join("settings.json");
        let destination = dir.path().join("odometer.tmp");
        std::fs::write(&source, b"sensitive existing settings").unwrap();
        std::fs::write(&destination, b"").unwrap();

        seed_windows_security_template(&source, &destination).unwrap();

        assert_eq!(
            std::fs::read(destination).unwrap(),
            b"sensitive existing settings"
        );
    }

    #[cfg(windows)]
    #[test]
    fn documented_replacefile_partial_state_restores_canonical_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        let replacement = dir.path().join("replacement.tmp");
        let backup = dir.path().join("original.recovery");
        std::fs::write(&replacement, b"replacement").unwrap();
        std::fs::write(&backup, b"original").unwrap();

        repair_windows_partial_replace(
            &path,
            &replacement,
            &backup,
            &std::io::Error::from_raw_os_error(1177),
        )
        .unwrap();

        assert_eq!(std::fs::read(path).unwrap(), b"original");
        assert_eq!(std::fs::read(replacement).unwrap(), b"replacement");
        assert!(!backup.exists());
    }

    #[test]
    fn reported_exchange_failure_after_swap_preserves_original_for_recovery() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        let retained = dir.path().join("odometer.recovery");
        let original = b"original configuration";
        let applied = b"odometer configuration";

        // Model a filesystem that completed the exchange but reported an
        // error: the active name has the update and the retained name has the
        // original inode/content.
        std::fs::write(&path, applied).unwrap();
        std::fs::write(&retained, original).unwrap();
        let metadata = capture_replacement_metadata(&retained).unwrap();

        let recovery = reconcile_failed_exchange(
            &path,
            &retained,
            original,
            &metadata,
            applied,
            anyhow!("injected post-exchange error"),
        )
        .unwrap();

        assert_eq!(recovery, retained);
        assert_eq!(std::fs::read(&path).unwrap(), applied);
        assert_eq!(std::fs::read(&recovery).unwrap(), original);
    }

    #[test]
    fn reported_exchange_failure_without_swap_removes_only_private_update() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        let retained = dir.path().join("odometer.tmp");
        let original = b"original configuration";
        let applied = b"odometer configuration";

        std::fs::write(&path, original).unwrap();
        std::fs::write(&retained, applied).unwrap();
        let metadata = capture_replacement_metadata(&path).unwrap();

        let error = reconcile_failed_exchange(
            &path,
            &retained,
            original,
            &metadata,
            applied,
            anyhow!("injected pre-exchange error"),
        )
        .unwrap_err()
        .to_string();

        assert!(error.contains("injected pre-exchange error"));
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert!(!retained.exists());
    }

    #[test]
    fn codex_freshness_includes_config_toml_control_changes() {
        use std::time::{Duration, SystemTime};

        let dir = tempdir().unwrap();
        let hooks_path = dir.path().join("hooks.json");
        let config_path = dir.path().join("config.toml");
        std::fs::write(&hooks_path, "{}\n").unwrap();
        std::fs::write(&config_path, "[features]\nhooks = true\n").unwrap();

        let now = SystemTime::now();
        let hook_changed = now - Duration::from_secs(60);
        let receipt_observed = now - Duration::from_secs(40);
        let control_changed = now - Duration::from_secs(20);
        std::fs::File::options()
            .write(true)
            .open(&hooks_path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(hook_changed))
            .unwrap();
        std::fs::File::options()
            .write(true)
            .open(&config_path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(control_changed))
            .unwrap();
        let receipt_observed = Some(chrono::DateTime::<chrono::Utc>::from(receipt_observed));

        assert!(observation_is_current(receipt_observed, &[&hooks_path]));
        assert!(!observation_is_current(
            receipt_observed,
            &[&hooks_path, &config_path]
        ));
    }
}
