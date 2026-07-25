use crate::config::{Config, InstructionRoot};
use crate::correlation::project_scope_identity;
use crate::model::Session;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

pub const MAX_PREVIEW_BYTES: u64 = 1024 * 1024;
const OVERSIZED_BYTES: u64 = 64 * 1024;
const OVERSIZED_LINES: usize = 800;
const MAX_DISCOVERED_FILES: usize = 10_000;
const MAX_SCAN_DEPTH: usize = 48;
const STALE_AFTER_DAYS: i64 = 180;
const ACTIVE_WITHIN_DAYS: i64 = 30;

struct InstructionDefinition {
    file_name: &'static str,
    harness: &'static str,
}

// Keep discovery data-driven so another harness is one definition plus its
// semantics, rather than another scanner branch. Only currently supported
// harnesses are admitted in this phase.
const DEFINITIONS: &[InstructionDefinition] = &[
    InstructionDefinition {
        file_name: "AGENTS.md",
        harness: "codex",
    },
    InstructionDefinition {
        file_name: "CLAUDE.md",
        harness: "claude_code",
    },
];

#[derive(Clone, Debug, Serialize)]
pub struct InstructionWarning {
    pub kind: String,
    pub severity: String,
    pub message: String,
    pub related_paths: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstructionFile {
    pub id: String,
    pub path_id: String,
    pub path: String,
    pub directory: String,
    pub file_name: String,
    pub harnesses: Vec<String>,
    pub root_path: String,
    pub root_source: String,
    pub root_recursive: bool,
    pub project_path: Option<String>,
    pub project_scope: Option<String>,
    pub relative_path: String,
    pub depth: usize,
    pub size: u64,
    pub line_count: Option<usize>,
    pub modified_at: Option<DateTime<Utc>>,
    pub content_hash: Option<String>,
    pub parent_id: Option<String>,
    pub effective_ids: Vec<String>,
    pub warnings: Vec<InstructionWarning>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstructionRootSummary {
    pub path: String,
    pub source: String,
    pub recursive: bool,
    pub exists: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstructionInventory {
    pub files: Vec<InstructionFile>,
    pub roots: Vec<InstructionRootSummary>,
    pub truncated: bool,
    pub scanned_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstructionContent {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug)]
struct ScanRoot {
    path: PathBuf,
    source: &'static str,
    recursive: bool,
}

#[derive(Clone)]
struct DiscoveredFile {
    item: InstructionFile,
    content: Option<String>,
}

pub fn discover(config: &Config, sessions: &[Session]) -> anyhow::Result<InstructionInventory> {
    if !config.instructions_enabled {
        return Ok(InstructionInventory {
            files: Vec::new(),
            roots: Vec::new(),
            truncated: false,
            scanned_at: Utc::now(),
        });
    }
    let roots = configured_scan_roots(config, sessions);
    discover_from_roots(&roots, sessions)
}

pub fn read_content(path: &Path) -> anyhow::Result<InstructionContent> {
    let metadata = validate_instruction_path(path)?;
    if metadata.len() > MAX_PREVIEW_BYTES {
        anyhow::bail!(
            "instruction file exceeds the {} MiB preview limit",
            MAX_PREVIEW_BYTES / 1024 / 1024
        );
    }
    let bytes = std::fs::read(path)?;
    let content = String::from_utf8(bytes)
        .map_err(|_| anyhow::anyhow!("instruction file is not valid UTF-8"))?;
    Ok(InstructionContent {
        path: display_path(path),
        content,
    })
}

pub fn validate_instruction_path(path: &Path) -> anyhow::Result<std::fs::Metadata> {
    if definition_for(path).is_none() {
        anyhow::bail!("unsupported instruction file name");
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        anyhow::bail!("instruction path must be a regular file, not a symbolic link");
    }
    Ok(metadata)
}

pub fn normalized_path_key(path: &Path) -> String {
    let value = path.to_string_lossy();
    let value = value.strip_prefix(r"\\?\").unwrap_or(&value);
    let normalized = value.replace('\\', "/").trim_end_matches('/').to_owned();
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

pub fn path_identity(path: &Path) -> String {
    stable_hash(normalized_path_key(path).as_bytes())
}

fn configured_scan_roots(config: &Config, sessions: &[Session]) -> Vec<ScanRoot> {
    let mut roots = Vec::new();
    let home = dirs::home_dir();
    if let Some(path) = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home.as_ref().map(|path| path.join(".codex")))
    {
        roots.push(ScanRoot {
            path,
            source: "global",
            recursive: false,
        });
    }
    if let Some(path) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home.map(|path| path.join(".claude")))
    {
        roots.push(ScanRoot {
            path,
            source: "global",
            recursive: false,
        });
    }
    roots.extend(config.instruction_roots.iter().map(|root| ScanRoot {
        path: root.path.clone(),
        source: "configured",
        recursive: root.recursive,
    }));
    roots.extend(
        sessions
            .iter()
            .filter_map(|session| session.working_directory.as_deref())
            .map(PathBuf::from)
            .map(|path| project_root(&path))
            .map(|path| ScanRoot {
                path,
                source: "observed",
                recursive: true,
            }),
    );
    deduplicate_roots(roots)
}

fn deduplicate_roots(roots: Vec<ScanRoot>) -> Vec<ScanRoot> {
    let mut deduplicated = BTreeMap::<String, ScanRoot>::new();
    for root in roots {
        let key = normalized_path_key(&root.path);
        match deduplicated.get_mut(&key) {
            Some(existing) => {
                existing.recursive |= root.recursive;
                if source_priority(root.source) > source_priority(existing.source) {
                    existing.source = root.source;
                }
            }
            None => {
                deduplicated.insert(key, root);
            }
        }
    }
    deduplicated.into_values().collect()
}

fn source_priority(source: &str) -> u8 {
    match source {
        "global" => 3,
        "configured" => 2,
        _ => 1,
    }
}

fn discover_from_roots(
    roots: &[ScanRoot],
    sessions: &[Session],
) -> anyhow::Result<InstructionInventory> {
    let mut by_path = BTreeMap::<String, DiscoveredFile>::new();
    let mut truncated = false;
    for root in roots {
        for path in instruction_paths(root) {
            if by_path.len() >= MAX_DISCOVERED_FILES {
                truncated = true;
                break;
            }
            let key = normalized_path_key(&path);
            let Ok(discovered) = inspect_file(root, &path) else {
                continue;
            };
            match by_path.get(&key) {
                Some(existing)
                    if path_depth(Path::new(&existing.item.root_path))
                        >= path_depth(&root.path) => {}
                _ => {
                    by_path.insert(key, discovered);
                }
            }
        }
        if truncated {
            break;
        }
    }

    let latest_activity = latest_project_activity(sessions);
    let mut discovered = by_path.into_values().collect::<Vec<_>>();
    add_hierarchy(&mut discovered);
    add_warnings(&mut discovered, &latest_activity);
    discovered.sort_by(|left, right| {
        left.item
            .project_path
            .cmp(&right.item.project_path)
            .then_with(|| left.item.directory.cmp(&right.item.directory))
            .then_with(|| left.item.file_name.cmp(&right.item.file_name))
    });
    Ok(InstructionInventory {
        files: discovered.into_iter().map(|entry| entry.item).collect(),
        roots: roots
            .iter()
            .map(|root| InstructionRootSummary {
                path: display_path(&root.path),
                source: root.source.into(),
                recursive: root.recursive,
                exists: root.path.is_dir(),
            })
            .collect(),
        truncated,
        scanned_at: Utc::now(),
    })
}

fn instruction_paths(root: &ScanRoot) -> Vec<PathBuf> {
    if !root.path.is_dir() {
        return Vec::new();
    }
    if !root.recursive {
        return DEFINITIONS
            .iter()
            .map(|definition| root.path.join(definition.file_name))
            .filter(|path| {
                std::fs::symlink_metadata(path)
                    .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            })
            .collect();
    }
    WalkDir::new(&root.path)
        .follow_links(false)
        .max_depth(MAX_SCAN_DEPTH)
        .into_iter()
        .filter_entry(should_descend)
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .filter(|entry| definition_for(entry.path()).is_some())
        .map(DirEntry::into_path)
        .take(MAX_DISCOVERED_FILES)
        .collect()
}

fn should_descend(entry: &DirEntry) -> bool {
    if entry.depth() == 0 || !entry.file_type().is_dir() {
        return true;
    }
    let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
    !matches!(
        name.as_str(),
        ".git"
            | ".hg"
            | ".svn"
            | ".next"
            | ".svelte-kit"
            | "node_modules"
            | "target"
            | "dist"
            | "build"
            | "out"
            | "bin"
            | "obj"
            | "vendor"
            | "coverage"
    )
}

pub(crate) fn should_watch_instruction_entry(entry: &DirEntry) -> bool {
    should_descend(entry)
}

fn definition_for(path: &Path) -> Option<&'static InstructionDefinition> {
    let name = path.file_name()?.to_str()?;
    DEFINITIONS
        .iter()
        .find(|definition| name.eq_ignore_ascii_case(definition.file_name))
}

fn inspect_file(root: &ScanRoot, path: &Path) -> anyhow::Result<DiscoveredFile> {
    let definition =
        definition_for(path).ok_or_else(|| anyhow::anyhow!("unsupported instruction file name"))?;
    let metadata = std::fs::symlink_metadata(path)?;
    anyhow::ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink(),
        "instruction path must be a regular file"
    );
    let modified_at = metadata.modified().ok().map(DateTime::<Utc>::from);
    let content = if metadata.len() <= MAX_PREVIEW_BYTES {
        std::fs::read_to_string(path).ok()
    } else {
        None
    };
    let normalized_content = content.as_deref().map(normalize_markdown);
    let directory = path.parent().unwrap_or_else(|| Path::new(""));
    let is_global = root.source == "global";
    let project = (!is_global).then(|| project_root_or(directory, &root.path));
    let project_scope = project
        .as_ref()
        .map(|path| project_scope_identity(&display_path(path)));
    let relative_path = path
        .strip_prefix(&root.path)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let id = stable_hash(normalized_path_key(path).as_bytes());
    Ok(DiscoveredFile {
        item: InstructionFile {
            id: id.clone(),
            path_id: id,
            path: display_path(path),
            directory: display_path(directory),
            file_name: definition.file_name.into(),
            harnesses: vec![definition.harness.into()],
            root_path: display_path(&root.path),
            root_source: root.source.into(),
            root_recursive: root.recursive,
            project_path: project.as_ref().map(|path| display_path(path)),
            project_scope,
            relative_path,
            depth: path_depth(path.strip_prefix(&root.path).unwrap_or(path)),
            size: metadata.len(),
            line_count: content.as_deref().map(|value| value.lines().count()),
            modified_at,
            content_hash: normalized_content
                .as_deref()
                .map(|value| stable_hash(value.as_bytes())),
            parent_id: None,
            effective_ids: Vec::new(),
            warnings: Vec::new(),
        },
        content,
    })
}

fn add_hierarchy(files: &mut [DiscoveredFile]) {
    let snapshots = files
        .iter()
        .map(|entry| entry.item.clone())
        .collect::<Vec<_>>();
    for file in files {
        let mut ancestors = snapshots
            .iter()
            .filter(|candidate| candidate.file_name == file.item.file_name)
            .filter(|candidate| {
                candidate.root_source == "global"
                    || (candidate.project_scope == file.item.project_scope
                        && is_ancestor_directory(&candidate.directory, &file.item.directory))
            })
            .collect::<Vec<_>>();
        ancestors.sort_by_key(|candidate| {
            if candidate.root_source == "global" {
                0
            } else {
                path_depth(Path::new(&candidate.directory)) + 1
            }
        });
        file.item.effective_ids = ancestors
            .iter()
            .map(|candidate| candidate.id.clone())
            .collect();
        file.item.parent_id = ancestors
            .iter()
            .rev()
            .find(|candidate| candidate.id != file.item.id && candidate.root_source != "global")
            .map(|candidate| candidate.id.clone());
    }
}

fn add_warnings(files: &mut [DiscoveredFile], latest_activity: &HashMap<String, DateTime<Utc>>) {
    for file in files.iter_mut() {
        if file.item.size > OVERSIZED_BYTES
            || file
                .item
                .line_count
                .is_some_and(|lines| lines > OVERSIZED_LINES)
        {
            let message = format!(
                "Large instruction file: {} bytes{}.",
                file.item.size,
                file.item
                    .line_count
                    .map(|lines| format!(", {lines} lines"))
                    .unwrap_or_default()
            );
            push_warning(&mut file.item, "oversized", "warning", message, Vec::new());
        }
        if let (Some(scope), Some(modified)) = (&file.item.project_scope, file.item.modified_at) {
            if let Some(activity) = latest_activity.get(scope) {
                let now = Utc::now();
                if now - modified > Duration::days(STALE_AFTER_DAYS)
                    && now - *activity <= Duration::days(ACTIVE_WITHIN_DAYS)
                    && *activity > modified
                {
                    push_warning(
                        &mut file.item,
                        "possibly_stale",
                        "info",
                        "Unchanged for over 180 days while this project has recent agent activity."
                            .into(),
                        Vec::new(),
                    );
                }
            }
        }
    }

    let mut duplicates = HashMap::<String, Vec<usize>>::new();
    for (index, file) in files.iter().enumerate() {
        if let Some(hash) = &file.item.content_hash {
            duplicates.entry(hash.clone()).or_default().push(index);
        }
    }
    for indexes in duplicates.values().filter(|indexes| indexes.len() > 1) {
        for index in indexes {
            let related_paths = indexes
                .iter()
                .filter(|candidate| *candidate != index)
                .map(|candidate| files[*candidate].item.path.clone())
                .collect::<Vec<_>>();
            push_warning(
                &mut files[*index].item,
                "duplicate",
                "info",
                format!(
                    "Identical normalized content appears in {} other file(s).",
                    related_paths.len()
                ),
                related_paths,
            );
        }
    }

    for left in 0..files.len() {
        for right in (left + 1)..files.len() {
            if !same_effective_chain(&files[left].item, &files[right].item) {
                continue;
            }
            let Some(left_content) = files[left].content.as_deref() else {
                continue;
            };
            let Some(right_content) = files[right].content.as_deref() else {
                continue;
            };
            let left_directives = directives(left_content);
            let right_directives = directives(right_content);
            let conflicts = left_directives
                .iter()
                .filter(|(action, polarity)| {
                    right_directives
                        .get(action.as_str())
                        .is_some_and(|other| other != *polarity)
                })
                .map(|(action, _)| action.clone())
                .take(3)
                .collect::<Vec<_>>();
            if conflicts.is_empty() {
                continue;
            }
            let message = format!(
                "Possible opposite directives in the effective chain: {}.",
                conflicts.join(", ")
            );
            let left_path = files[left].item.path.clone();
            let right_path = files[right].item.path.clone();
            push_warning(
                &mut files[left].item,
                "possible_conflict",
                "warning",
                message.clone(),
                vec![right_path],
            );
            push_warning(
                &mut files[right].item,
                "possible_conflict",
                "warning",
                message,
                vec![left_path],
            );
        }
    }
}

fn same_effective_chain(left: &InstructionFile, right: &InstructionFile) -> bool {
    if left.file_name != right.file_name {
        return false;
    }
    if left.root_source == "global" || right.root_source == "global" {
        return true;
    }
    left.project_scope == right.project_scope
        && (is_ancestor_directory(&left.directory, &right.directory)
            || is_ancestor_directory(&right.directory, &left.directory))
}

fn directives(content: &str) -> HashMap<String, bool> {
    let mut output = HashMap::new();
    for line in content.lines() {
        let trimmed = line
            .trim()
            .trim_start_matches(|character: char| {
                matches!(
                    character,
                    '-' | '*' | '+' | '>' | '#' | '0'..='9' | '.' | ')' | ' '
                )
            })
            .trim()
            .to_ascii_lowercase();
        let patterns = [
            ("must not ", false),
            ("do not ", false),
            ("don't ", false),
            ("never ", false),
            ("always ", true),
            ("must ", true),
        ];
        let Some((prefix, polarity)) = patterns
            .iter()
            .find(|(prefix, _)| trimmed.starts_with(prefix))
        else {
            continue;
        };
        let action = trimmed[prefix.len()..]
            .trim_matches(|character: char| character.is_ascii_punctuation() || character == ' ')
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if action.len() >= 4 {
            output.insert(action, *polarity);
        }
    }
    output
}

fn push_warning(
    file: &mut InstructionFile,
    kind: &str,
    severity: &str,
    message: String,
    related_paths: Vec<String>,
) {
    if file
        .warnings
        .iter()
        .any(|warning| warning.kind == kind && warning.related_paths == related_paths)
    {
        return;
    }
    file.warnings.push(InstructionWarning {
        kind: kind.into(),
        severity: severity.into(),
        message,
        related_paths,
    });
}

fn latest_project_activity(sessions: &[Session]) -> HashMap<String, DateTime<Utc>> {
    let mut latest = HashMap::new();
    for session in sessions {
        let Some(directory) = session.working_directory.as_deref() else {
            continue;
        };
        let project = project_root(Path::new(directory));
        let scope = project_scope_identity(&display_path(&project));
        latest
            .entry(scope)
            .and_modify(|value: &mut DateTime<Utc>| {
                if session.last_event_at > *value {
                    *value = session.last_event_at;
                }
            })
            .or_insert(session.last_event_at);
    }
    latest
}

fn project_root(path: &Path) -> PathBuf {
    project_root_or(path, path)
}

fn project_root_or(path: &Path, fallback: &Path) -> PathBuf {
    gix::discover(path)
        .ok()
        .map(|repository| {
            repository
                .work_dir()
                .unwrap_or_else(|| repository.git_dir())
                .to_path_buf()
        })
        .unwrap_or_else(|| fallback.to_path_buf())
}

fn normalize_markdown(content: &str) -> String {
    content
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

fn is_ancestor_directory(ancestor: &str, descendant: &str) -> bool {
    let ancestor = normalized_path_key(Path::new(ancestor));
    let descendant = normalized_path_key(Path::new(descendant));
    descendant == ancestor || descendant.starts_with(&(ancestor + "/"))
}

fn path_depth(path: &Path) -> usize {
    path.components().count()
}

fn display_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned()
}

fn stable_hash(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

pub fn validate_instruction_roots(roots: &[InstructionRoot]) -> anyhow::Result<()> {
    if roots.len() > 128 {
        anyhow::bail!("instruction discovery is limited to 128 configured roots");
    }
    let mut seen = HashSet::new();
    for root in roots {
        if root.path.as_os_str().is_empty() || !root.path.is_absolute() {
            anyhow::bail!("instruction roots must be absolute paths");
        }
        if !seen.insert(normalized_path_key(&root.path)) {
            anyhow::bail!("instruction roots contain a duplicate path");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn root(path: &Path, recursive: bool) -> ScanRoot {
        ScanRoot {
            path: path.to_path_buf(),
            source: "configured",
            recursive,
        }
    }

    #[test]
    fn non_recursive_and_recursive_discovery_respect_ignored_trees() {
        let directory = tempdir().unwrap();
        std::fs::write(directory.path().join("AGENTS.md"), "# Root").unwrap();
        std::fs::create_dir_all(directory.path().join("app")).unwrap();
        std::fs::write(directory.path().join("app/CLAUDE.md"), "# App").unwrap();
        std::fs::create_dir_all(directory.path().join("node_modules/pkg")).unwrap();
        std::fs::write(
            directory.path().join("node_modules/pkg/AGENTS.md"),
            "# Ignore",
        )
        .unwrap();

        let flat = instruction_paths(&root(directory.path(), false));
        assert_eq!(flat.len(), 1);
        let recursive = instruction_paths(&root(directory.path(), true));
        assert_eq!(recursive.len(), 2);
        assert!(recursive
            .iter()
            .all(|path| !path.to_string_lossy().contains("node_modules")));
    }

    #[test]
    fn nested_files_receive_parent_and_effective_chain() {
        let directory = tempdir().unwrap();
        std::fs::write(directory.path().join("AGENTS.md"), "Always run tests").unwrap();
        std::fs::create_dir_all(directory.path().join("packages/app")).unwrap();
        std::fs::write(
            directory.path().join("packages/app/AGENTS.md"),
            "Always run tests\nAlways lint",
        )
        .unwrap();
        let inventory = discover_from_roots(&[root(directory.path(), true)], &[]).unwrap();
        let nested = inventory
            .files
            .iter()
            .find(|file| file.relative_path.contains("packages"))
            .unwrap();
        assert!(nested.parent_id.is_some());
        assert_eq!(nested.effective_ids.len(), 2);
    }

    #[test]
    fn duplicate_and_opposite_directives_are_flagged() {
        let directory = tempdir().unwrap();
        std::fs::write(directory.path().join("AGENTS.md"), "Always run tests").unwrap();
        std::fs::create_dir_all(directory.path().join("first/deep")).unwrap();
        std::fs::write(directory.path().join("first/AGENTS.md"), "Never run tests").unwrap();
        std::fs::write(
            directory.path().join("first/deep/AGENTS.md"),
            "Never run tests",
        )
        .unwrap();
        let inventory = discover_from_roots(&[root(directory.path(), true)], &[]).unwrap();
        assert!(inventory.files.iter().any(|file| file
            .warnings
            .iter()
            .any(|warning| warning.kind == "possible_conflict")));
        assert_eq!(
            inventory
                .files
                .iter()
                .filter(|file| file
                    .warnings
                    .iter()
                    .any(|warning| warning.kind == "duplicate"))
                .count(),
            2
        );
    }

    #[test]
    fn roots_must_be_absolute_and_unique() {
        assert!(validate_instruction_roots(&[InstructionRoot {
            path: PathBuf::from("relative"),
            recursive: false,
        }])
        .is_err());
        let directory = tempdir().unwrap();
        let duplicate = InstructionRoot {
            path: directory.path().to_path_buf(),
            recursive: true,
        };
        assert!(validate_instruction_roots(&[duplicate.clone(), duplicate]).is_err());
    }
}
