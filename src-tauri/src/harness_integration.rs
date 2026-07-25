use crate::config::{claude_config_dir, codex_home_dir, Config};
use crate::model::Harness;
use crate::turn_receipts::{load_run_record, HookRunRecord};
use anyhow::{anyhow, Context};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

const INTEGRATION_ID: &str = "odometer-turn-receipts-v1";
const STATUS_MESSAGE: &str = "Calculating Odometer turn receipt";

#[derive(Debug, Clone, Serialize)]
pub struct HarnessIntegrationStatus {
    pub requested: bool,
    pub installed: bool,
    pub config_path: String,
    pub detail: String,
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

struct PlannedWrite {
    path: PathBuf,
    original: Option<Vec<u8>>,
    updated: Vec<u8>,
}

#[derive(Default)]
struct HookInspection {
    owned: bool,
    current: bool,
}

/// Rollback guard spanning hook-file writes and the subsequent Odometer config
/// save. If either side fails, the harness configuration is restored.
pub struct IntegrationTransaction {
    originals: Vec<(PathBuf, Option<Vec<u8>>)>,
    committed: bool,
}

impl IntegrationTransaction {
    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for IntegrationTransaction {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        for (path, original) in self.originals.iter().rev() {
            if let Some(bytes) = original {
                let _ = write_recoverably(path, bytes);
            } else if path.is_file() {
                let _ = std::fs::remove_file(path);
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
/// groups and unrelated settings are retained.
pub fn sync(config: &Config) -> anyhow::Result<IntegrationTransaction> {
    let executable = std::env::current_exe().context("could not locate the Odometer executable")?;
    let codex_path = codex_home_dir().join("hooks.json");
    let claude_path = claude_config_dir().join("settings.json");
    let codex_enabled = config.turn_receipts_enabled && config.turn_receipts_codex;
    let claude_enabled = config.turn_receipts_enabled && config.turn_receipts_claude;

    if codex_enabled && !codex_path.exists() && codex_config_has_inline_hooks()? {
        return Err(anyhow!(
            "Codex already defines inline hooks in config.toml. Odometer did not create a second hook source; move those hooks to hooks.json or add the Odometer command manually."
        ));
    }

    let plans = [
        plan_hook_file(
            &codex_path,
            &hook_command(&executable, "codex"),
            codex_enabled,
        )?,
        plan_hook_file(
            &claude_path,
            &hook_command(&executable, "claude"),
            claude_enabled,
        )?,
    ];
    let mut originals: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::new();
    for plan in plans.into_iter().flatten() {
        if let Err(error) = write_recoverably(&plan.path, &plan.updated) {
            for (path, original) in originals.iter().rev() {
                if let Some(bytes) = original {
                    let _ = write_recoverably(path, bytes);
                } else if path.is_file() {
                    let _ = std::fs::remove_file(path);
                }
            }
            return Err(error);
        }
        originals.push((plan.path, plan.original));
    }
    Ok(IntegrationTransaction {
        originals,
        committed: false,
    })
}

pub fn status(config: &Config) -> TurnReceiptIntegrationStatus {
    let executable = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("agent-odometer"));
    let codex_path = codex_home_dir().join("hooks.json");
    let claude_path = claude_config_dir().join("settings.json");
    TurnReceiptIntegrationStatus {
        enabled: config.turn_receipts_enabled,
        executable_path: executable.to_string_lossy().into_owned(),
        codex: one_status(
            Harness::Codex,
            config.turn_receipts_enabled && config.turn_receipts_codex,
            &codex_path,
            &hook_command(&executable, "codex"),
        ),
        claude_code: one_status(
            Harness::ClaudeCode,
            config.turn_receipts_enabled && config.turn_receipts_claude,
            &claude_path,
            &hook_command(&executable, "claude"),
        ),
    }
}

fn one_status(
    harness: Harness,
    requested: bool,
    path: &Path,
    expected_command: &str,
) -> HarnessIntegrationStatus {
    let inspection = inspect_hook_file(path, expected_command);
    let (installed, stale, read_error) = match inspection {
        Ok(inspection) => (
            inspection.current,
            inspection.owned && !inspection.current,
            None,
        ),
        Err(error) => (false, false, Some(error.to_string())),
    };
    let detail = if let Some(error) = read_error {
        format!("Cannot inspect configuration: {error}")
    } else {
        match (requested, installed, stale) {
            (true, true, _) => match harness {
                Harness::Codex => "Installed. Use /hooks in Codex to review and trust the command.".into(),
                Harness::ClaudeCode => "Installed. Use /hooks in Claude Code to inspect the command.".into(),
            },
            (true, false, true) => "The Odometer hook points to a different executable. Use Repair setup.".into(),
            (true, false, false) => "Enabled in Odometer but the hook is not installed. Use Repair setup.".into(),
            (false, _, true) | (false, true, false) => "An Odometer hook remains installed. Save or repair the disabled setup to remove it.".into(),
            (false, false, false) => "Off. Harness configuration is unchanged.".into(),
        }
    };
    let HookRunRecord {
        last_run_at,
        success,
        last_receipt,
        detail: last_run_detail,
    } = load_run_record(harness);
    HarnessIntegrationStatus {
        requested,
        installed,
        config_path: path.to_string_lossy().into_owned(),
        detail,
        last_run_at,
        last_run_success: last_run_at.map(|_| success),
        last_receipt,
        last_run_detail,
    }
}

fn hook_command(executable: &Path, harness: &str) -> String {
    let path = executable.to_string_lossy();
    #[cfg(windows)]
    let quoted = format!("\"{}\"", path.replace('"', ""));
    #[cfg(not(windows))]
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    format!("{quoted} hook {harness} --integration-id {INTEGRATION_ID}")
}

fn is_odometer_handler(value: &Value) -> bool {
    value
        .get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(INTEGRATION_ID))
}

fn plan_hook_file(
    path: &Path,
    command: &str,
    enabled: bool,
) -> anyhow::Result<Option<PlannedWrite>> {
    let original = std::fs::read(path).ok();
    if original.is_none() && !enabled {
        return Ok(None);
    }
    let mut root = match &original {
        Some(bytes) => serde_json::from_slice::<Value>(bytes)
            .with_context(|| format!("{} is not valid JSON", path.display()))?,
        None => Value::Object(Map::new()),
    };
    let before = root.clone();
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
        let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            continue;
        };
        handlers.retain(|handler| !is_odometer_handler(handler));
    }
    stop.retain(|group| {
        group
            .get("hooks")
            .and_then(Value::as_array)
            .is_none_or(|handlers| !handlers.is_empty())
    });
    if enabled {
        stop.push(json!({
            "hooks": [{
                "type": "command",
                "command": command,
                "timeout": 5,
                "statusMessage": STATUS_MESSAGE
            }]
        }));
    }

    if stop.is_empty() {
        hooks.remove("Stop");
    }
    if hooks.is_empty() {
        object.remove("hooks");
    }
    if root == before {
        return Ok(None);
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

fn inspect_hook_file(path: &Path, expected_command: &str) -> anyhow::Result<HookInspection> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HookInspection::default())
        }
        Err(error) => return Err(error.into()),
    };
    let root: Value = serde_json::from_slice(&bytes)?;
    let handlers = root
        .get("hooks")
        .and_then(|hooks| hooks.get("Stop"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|group| group.get("hooks").and_then(Value::as_array))
        .flatten();
    let mut inspection = HookInspection::default();
    for handler in handlers {
        if !is_odometer_handler(handler) {
            continue;
        }
        inspection.owned = true;
        if handler.get("command").and_then(Value::as_str) == Some(expected_command) {
            inspection.current = true;
        }
    }
    Ok(inspection)
}

fn codex_config_has_inline_hooks() -> anyhow::Result<bool> {
    let path = codex_home_dir().join("config.toml");
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    Ok(raw.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("[hooks]") || line.starts_with("[[hooks.")
    }))
}

fn write_recoverably(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("configuration path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let suffix = format!("odometer-{}", std::process::id());
    let temporary = path.with_extension(format!("{suffix}.tmp"));
    let backup = path.with_extension(format!("{suffix}.bak"));
    std::fs::write(&temporary, bytes)?;
    if !path.exists() {
        std::fs::rename(&temporary, path)?;
        return Ok(());
    }
    std::fs::rename(path, &backup)?;
    match std::fs::rename(&temporary, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(backup);
            Ok(())
        }
        Err(error) => {
            let _ = std::fs::rename(&backup, path);
            let _ = std::fs::remove_file(temporary);
            Err(error.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn install_and_remove_preserve_unrelated_hooks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hooks.json");
        std::fs::write(
            &path,
            br#"{"description":"mine","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-me"}]}]}}"#,
        )
        .unwrap();
        let install = plan_hook_file(
            &path,
            "odometer hook codex --integration-id odometer-turn-receipts-v1",
            true,
        )
        .unwrap()
        .unwrap();
        write_recoverably(&install.path, &install.updated).unwrap();
        assert!(
            inspect_hook_file(
                &path,
                "odometer hook codex --integration-id odometer-turn-receipts-v1"
            )
            .unwrap()
            .current
        );
        let installed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(installed.to_string().contains("keep-me"));

        let remove = plan_hook_file(&path, "ignored", false).unwrap().unwrap();
        write_recoverably(&remove.path, &remove.updated).unwrap();
        assert!(!inspect_hook_file(&path, "ignored").unwrap().owned);
        let removed: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(removed.to_string().contains("keep-me"));
        assert_eq!(removed["description"], "mine");
    }

    #[test]
    fn disabled_missing_file_is_untouched() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(plan_hook_file(&path, "ignored", false).unwrap().is_none());
        assert!(!path.exists());
    }

    #[test]
    fn disabled_unrelated_config_is_not_reformatted() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let original = br#"{"theme":"dark","hooks":{"Stop":[{"hooks":[{"type":"command","command":"keep-me"}]}]}}"#;
        std::fs::write(&path, original).unwrap();
        assert!(plan_hook_file(&path, "ignored", false).unwrap().is_none());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }

    #[test]
    fn invalid_existing_json_is_never_overwritten() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, b"not-json").unwrap();
        assert!(plan_hook_file(&path, "command", true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not-json");
    }
}
