//! Read-only Claude Code guard target preflight. This does not install a hook,
//! read a command body into IPC, or make the disabled action engine reachable.

use crate::action_contract::{ActionPreview, PreviewPrecondition};
use crate::config::claude_config_dir;
use crate::harness_integration::read_optional_config;
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Debug, PartialEq, Eq)]
struct ClaudeGuardTarget {
    settings_present: bool,
    user_hooks_not_disabled: bool,
    pre_tool_use_unoccupied: bool,
}

/// Only fixed status codes leave this module. Existing hook commands, the
/// settings path, and parse errors remain local to Rust.
fn inspect_at(root: &Path) -> Result<ClaudeGuardTarget, &'static str> {
    if !root.is_absolute() {
        return Err("claude_settings_unreviewable");
    }
    match fs::symlink_metadata(root) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err("claude_settings_unreviewable")
        }
        Ok(metadata) => {
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes()
                    & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
                    != 0
                {
                    return Err("claude_settings_unreviewable");
                }
            }
            #[cfg(not(windows))]
            let _ = metadata;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ClaudeGuardTarget {
                settings_present: false,
                user_hooks_not_disabled: false,
                pre_tool_use_unoccupied: false,
            });
        }
        Err(_) => return Err("claude_settings_unreviewable"),
    }
    let Some(bytes) = read_optional_config(&root.join("settings.json"))
        .map_err(|_| "claude_settings_unreviewable")?
    else {
        return Ok(ClaudeGuardTarget {
            settings_present: false,
            user_hooks_not_disabled: false,
            pre_tool_use_unoccupied: false,
        });
    };
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|_| "claude_settings_unreviewable")?;
    let settings = value.as_object().ok_or("claude_settings_unreviewable")?;
    let hooks_disabled = settings
        .get("disableAllHooks")
        .map(Value::as_bool)
        .unwrap_or(Some(false))
        .ok_or("claude_settings_unreviewable")?;
    let pre_tool_use_unoccupied = match settings.get("hooks") {
        None => true,
        Some(hooks) => {
            let hooks = hooks.as_object().ok_or("claude_settings_unreviewable")?;
            match hooks.get("PreToolUse") {
                None => true,
                Some(entries) => entries
                    .as_array()
                    .ok_or("claude_settings_unreviewable")?
                    .is_empty(),
            }
        }
    };
    Ok(ClaudeGuardTarget {
        settings_present: true,
        user_hooks_not_disabled: !hooks_disabled,
        pre_tool_use_unoccupied,
    })
}

/// Adds only fixed booleans to the already-disabled budget preview. The
/// source must be inspected again if a future reviewed adapter ever writes.
pub(crate) fn add_claude_guard_preflight(preview: &mut ActionPreview) {
    add_for_root(preview, &claude_config_dir());
}

fn add_for_root(preview: &mut ActionPreview, root: &Path) {
    preview.target_type = "claude_user_settings_json";
    let result = inspect_at(root);
    let target = result.as_ref().ok();
    preview.proposed_change = match target {
        None => "Claude Code user settings could not be reviewed safely. No guard action is available.",
        Some(value) if !value.settings_present => {
            "Claude Code user settings are absent. A future adapter would need a reviewed create-and-restore plan."
        }
        Some(value) if !value.user_hooks_not_disabled => {
            "Claude Code user settings disable hooks, so a guard hook there would not be active."
        }
        Some(value) if !value.pre_tool_use_unoccupied => {
            "Existing Claude Code user PreToolUse hooks require review; Odometer would not replace them."
        }
        Some(_) => "Claude Code user settings have no PreToolUse hook. Other hook scopes are not inspected; command-hook startup failures and timeouts cannot enforce a hard budget cap.",
    };
    preview.preconditions.extend([
        PreviewPrecondition {
            code: "claude_settings_reviewable",
            satisfied: target.is_some(),
        },
        PreviewPrecondition {
            code: "claude_settings_present",
            satisfied: target.is_some_and(|value| value.settings_present),
        },
        PreviewPrecondition {
            code: "claude_user_hooks_not_disabled",
            satisfied: target.is_some_and(|value| value.user_hooks_not_disabled),
        },
        PreviewPrecondition {
            code: "claude_user_pretooluse_unoccupied",
            satisfied: target.is_some_and(|value| value.pre_tool_use_unoccupied),
        },
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn preview() -> ActionPreview {
        ActionPreview {
            contract_version: 1,
            kind: "budget_guard",
            target_type: "provider_guard_configuration",
            redacted_target: "target:synthetic".into(),
            source_revision: "synthetic".into(),
            preconditions: Vec::new(),
            proposed_change: "Future reviewed change",
            backup_requirement: "Exact backup required",
            postcondition_requirement: "Read back required",
            apply_available: false,
            undo_available: false,
            unavailable_reason: "writes_disabled_pending_security_review",
        }
    }

    #[test]
    fn inspects_only_fixed_guard_state_without_exposing_existing_commands() {
        let root = TempDir::new().unwrap();
        let secret = "private-command-and-path";
        fs::write(
            root.path().join("settings.json"),
            format!(r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{secret}"}}]}}]}}}}"#),
        )
        .unwrap();
        let target = inspect_at(root.path()).unwrap();
        assert_eq!(
            target,
            ClaudeGuardTarget {
                settings_present: true,
                user_hooks_not_disabled: true,
                pre_tool_use_unoccupied: true,
            }
        );
        assert!(!format!("{target:?}").contains(secret));
        let mut proposal = preview();
        add_for_root(&mut proposal, root.path());
        assert_eq!(proposal.target_type, "claude_user_settings_json");
        assert_eq!(proposal.preconditions.len(), 4);
        assert!(proposal.preconditions.iter().all(|item| item.satisfied));
        assert!(!proposal.apply_available && !proposal.undo_available);
        let serialized = serde_json::to_string(&proposal).unwrap();
        assert!(!serialized.contains(secret));
        assert!(!serialized.contains(root.path().to_str().unwrap()));
        fs::write(
            root.path().join("settings.json"),
            r#"{"disableAllHooks":true,"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"private"}]}]}}"#,
        )
        .unwrap();
        let occupied = inspect_at(root.path()).unwrap();
        assert!(!occupied.user_hooks_not_disabled && !occupied.pre_tool_use_unoccupied);
        add_for_root(&mut proposal, root.path());
        assert!(proposal
            .proposed_change
            .contains("user settings disable hooks"));
    }

    #[test]
    fn missing_malformed_nonregular_and_oversized_settings_fail_closed() {
        let root = TempDir::new().unwrap();
        let path = root.path().join("settings.json");
        assert_eq!(
            inspect_at(Path::new("relative-claude-config")),
            Err("claude_settings_unreviewable")
        );
        assert!(!inspect_at(root.path()).unwrap().settings_present);
        fs::write(&path, "not JSON").unwrap();
        assert_eq!(inspect_at(root.path()), Err("claude_settings_unreviewable"));
        fs::write(&path, r#"{"disableAllHooks":"false"}"#).unwrap();
        assert_eq!(inspect_at(root.path()), Err("claude_settings_unreviewable"));
        fs::write(&path, r#"{"hooks":{"PreToolUse":{}}}"#).unwrap();
        assert_eq!(inspect_at(root.path()), Err("claude_settings_unreviewable"));
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert_eq!(inspect_at(root.path()), Err("claude_settings_unreviewable"));
        fs::remove_dir(&path).unwrap();
        let mut oversized = fs::File::create(&path).unwrap();
        oversized.set_len(4 * 1024 * 1024 + 1).unwrap();
        oversized.flush().unwrap();
        assert_eq!(inspect_at(root.path()), Err("claude_settings_unreviewable"));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_root_and_settings_fail_closed() {
        use std::os::unix::fs::symlink;
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let linked_root = root.path().join("linked-root");
        symlink(outside.path(), &linked_root).unwrap();
        assert_eq!(
            inspect_at(&linked_root),
            Err("claude_settings_unreviewable")
        );
        let outside_file = outside.path().join("outside.json");
        fs::write(&outside_file, "{}").unwrap();
        symlink(&outside_file, root.path().join("settings.json")).unwrap();
        assert_eq!(inspect_at(root.path()), Err("claude_settings_unreviewable"));
    }
}
