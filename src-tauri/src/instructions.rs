use crate::config::{Config, InstructionRoot};
use crate::correlation::project_scope_identity;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration as StdDuration, Instant};
use walkdir::{DirEntry, WalkDir};

pub const MAX_PREVIEW_BYTES: u64 = 1024 * 1024;
const OVERSIZED_BYTES: u64 = 64 * 1024;
const OVERSIZED_LINES: usize = 800;
const MAX_DISCOVERED_FILES: usize = 10_000;
const MAX_VISITED_ENTRIES: usize = 250_000;
const MAX_SCAN_DEPTH: usize = 48;
const PROGRESS_ENTRY_INTERVAL: usize = 2_048;
const PROGRESS_TIME_INTERVAL: StdDuration = StdDuration::from_millis(250);
const STALE_AFTER_DAYS: i64 = 180;
const ACTIVE_WITHIN_DAYS: i64 = 30;

pub const SCAN_CANCELLED_ERROR: &str = "instruction scan cancelled";

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
    pub truncation_reason: Option<String>,
    pub entries_visited: usize,
    pub elapsed_ms: u64,
    pub scanned_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InstructionScanProgress {
    pub scan_id: u64,
    pub phase: String,
    pub roots_done: usize,
    pub roots_total: usize,
    pub entries_visited: usize,
    pub files_found: usize,
    pub elapsed_ms: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug)]
pub struct InstructionSessionContext {
    pub working_directory: PathBuf,
    pub last_event_at: DateTime<Utc>,
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

#[derive(Clone, Debug)]
struct ProjectContext {
    path: PathBuf,
    scope: String,
    last_event_at: DateTime<Utc>,
}

#[derive(Clone, Copy)]
struct ScanLimits {
    max_discovered_files: usize,
    max_visited_entries: usize,
    max_scan_depth: usize,
}

impl Default for ScanLimits {
    fn default() -> Self {
        Self {
            max_discovered_files: MAX_DISCOVERED_FILES,
            max_visited_entries: MAX_VISITED_ENTRIES,
            max_scan_depth: MAX_SCAN_DEPTH,
        }
    }
}

#[derive(Default)]
struct ScanBudget {
    entries_visited: usize,
    files_found: usize,
    truncation_reason: Option<&'static str>,
}

pub fn discover_with_progress<P, C>(
    config: &Config,
    sessions: &[InstructionSessionContext],
    scan_id: u64,
    mut on_progress: P,
    is_cancelled: C,
) -> anyhow::Result<InstructionInventory>
where
    P: FnMut(InstructionScanProgress),
    C: Fn() -> bool,
{
    let started = Instant::now();
    if !config.instructions_enabled {
        return Ok(InstructionInventory {
            files: Vec::new(),
            roots: Vec::new(),
            truncated: false,
            truncation_reason: None,
            entries_visited: 0,
            elapsed_ms: 0,
            scanned_at: Utc::now(),
        });
    }
    on_progress(scan_progress(
        scan_id,
        "preparing",
        0,
        0,
        &ScanBudget::default(),
        started,
    ));
    let projects = resolve_project_contexts(sessions, &is_cancelled)?;
    let roots = configured_scan_roots(config, &projects);
    discover_from_roots(
        &roots,
        &projects,
        scan_id,
        started,
        ScanLimits::default(),
        &mut on_progress,
        &is_cancelled,
    )
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

fn resolve_project_contexts<C>(
    sessions: &[InstructionSessionContext],
    is_cancelled: &C,
) -> anyhow::Result<Vec<ProjectContext>>
where
    C: Fn() -> bool,
{
    let working_directories = deduplicate_session_contexts(sessions);
    let mut projects = BTreeMap::<String, ProjectContext>::new();
    for session in working_directories {
        ensure_scan_current(is_cancelled)?;
        let Some(path) = discover_project_root(&session.working_directory) else {
            continue;
        };
        let key = normalized_path_key(&path);
        let scope = project_scope_identity(&display_path(&path));
        projects
            .entry(key)
            .and_modify(|existing| {
                if session.last_event_at > existing.last_event_at {
                    existing.last_event_at = session.last_event_at;
                }
            })
            .or_insert(ProjectContext {
                path,
                scope,
                last_event_at: session.last_event_at,
            });
    }
    Ok(projects.into_values().collect())
}

fn deduplicate_session_contexts(
    sessions: &[InstructionSessionContext],
) -> Vec<InstructionSessionContext> {
    let mut working_directories = BTreeMap::<String, InstructionSessionContext>::new();
    for session in sessions {
        let key = normalized_path_key(&session.working_directory);
        working_directories
            .entry(key)
            .and_modify(|existing| {
                if session.last_event_at > existing.last_event_at {
                    existing.last_event_at = session.last_event_at;
                }
            })
            .or_insert_with(|| session.clone());
    }
    working_directories.into_values().collect()
}

fn configured_scan_roots(config: &Config, projects: &[ProjectContext]) -> Vec<ScanRoot> {
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
    roots.extend(projects.iter().map(|project| ScanRoot {
        path: project.path.clone(),
        source: "observed",
        recursive: true,
    }));
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

fn traversal_roots(roots: &[ScanRoot]) -> Vec<ScanRoot> {
    let mut candidates = roots
        .iter()
        .filter(|root| root.path.is_dir())
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        path_depth(&left.path)
            .cmp(&path_depth(&right.path))
            .then_with(|| source_priority(right.source).cmp(&source_priority(left.source)))
    });
    let mut planned = Vec::<ScanRoot>::new();
    for candidate in candidates {
        if planned
            .iter()
            .any(|ancestor| ancestor.recursive && is_path_ancestor(&ancestor.path, &candidate.path))
        {
            continue;
        }
        planned.push(candidate);
    }
    planned
}

fn attribution_root<'a>(roots: &'a [ScanRoot], path: &Path) -> Option<&'a ScanRoot> {
    roots
        .iter()
        .filter(|root| {
            if root.recursive {
                is_path_ancestor(&root.path, path)
            } else {
                path.parent().is_some_and(|parent| {
                    normalized_path_key(parent) == normalized_path_key(&root.path)
                })
            }
        })
        .max_by(|left, right| {
            path_depth(&left.path)
                .cmp(&path_depth(&right.path))
                .then_with(|| source_priority(left.source).cmp(&source_priority(right.source)))
        })
}

fn discover_from_roots(
    roots: &[ScanRoot],
    projects: &[ProjectContext],
    scan_id: u64,
    started: Instant,
    limits: ScanLimits,
    on_progress: &mut impl FnMut(InstructionScanProgress),
    is_cancelled: &impl Fn() -> bool,
) -> anyhow::Result<InstructionInventory> {
    let mut by_path = BTreeMap::<String, DiscoveredFile>::new();
    let scan_roots = traversal_roots(roots);
    let roots_total = scan_roots.len();
    let mut budget = ScanBudget::default();
    on_progress(scan_progress(
        scan_id,
        "scanning",
        0,
        roots_total,
        &budget,
        started,
    ));
    for (root_index, root) in scan_roots.iter().enumerate() {
        let paths = instruction_paths(
            root,
            &mut budget,
            limits,
            scan_id,
            root_index,
            roots_total,
            started,
            on_progress,
            is_cancelled,
        )?;
        for path in paths {
            ensure_scan_current(is_cancelled)?;
            let key = normalized_path_key(&path);
            let owner = attribution_root(roots, &path).unwrap_or(root);
            let Ok(discovered) = inspect_file(owner, &path) else {
                continue;
            };
            by_path.insert(key, discovered);
        }
        on_progress(scan_progress(
            scan_id,
            "scanning",
            root_index + 1,
            roots_total,
            &budget,
            started,
        ));
        if budget.truncation_reason.is_some() {
            break;
        }
    }

    ensure_scan_current(is_cancelled)?;
    on_progress(scan_progress(
        scan_id,
        "analyzing",
        roots_total.min(scan_roots.len()),
        roots_total,
        &budget,
        started,
    ));
    let latest_activity = latest_project_activity(projects);
    let mut discovered = by_path.into_values().collect::<Vec<_>>();
    add_hierarchy(&mut discovered, is_cancelled)?;
    add_warnings(&mut discovered, &latest_activity, is_cancelled)?;
    discovered.sort_by(|left, right| {
        left.item
            .project_path
            .cmp(&right.item.project_path)
            .then_with(|| left.item.directory.cmp(&right.item.directory))
            .then_with(|| left.item.file_name.cmp(&right.item.file_name))
    });
    ensure_scan_current(is_cancelled)?;
    let elapsed_ms = elapsed_millis(started);
    let inventory = InstructionInventory {
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
        truncated: budget.truncation_reason.is_some(),
        truncation_reason: budget.truncation_reason.map(str::to_owned),
        entries_visited: budget.entries_visited,
        elapsed_ms,
        scanned_at: Utc::now(),
    };
    on_progress(InstructionScanProgress {
        scan_id,
        phase: "complete".into(),
        roots_done: roots_total,
        roots_total,
        entries_visited: budget.entries_visited,
        files_found: inventory.files.len(),
        elapsed_ms,
        truncated: inventory.truncated,
    });
    Ok(inventory)
}

#[allow(clippy::too_many_arguments)]
fn instruction_paths(
    root: &ScanRoot,
    budget: &mut ScanBudget,
    limits: ScanLimits,
    scan_id: u64,
    roots_done: usize,
    roots_total: usize,
    started: Instant,
    on_progress: &mut impl FnMut(InstructionScanProgress),
    is_cancelled: &impl Fn() -> bool,
) -> anyhow::Result<Vec<PathBuf>> {
    if !root.path.is_dir() {
        return Ok(Vec::new());
    }
    if !root.recursive {
        let mut paths = Vec::new();
        for definition in DEFINITIONS {
            ensure_scan_current(is_cancelled)?;
            if budget.entries_visited >= limits.max_visited_entries {
                budget.truncation_reason = Some("entry_limit");
                break;
            }
            budget.entries_visited += 1;
            let path = root.path.join(definition.file_name);
            if std::fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            {
                if budget.files_found >= limits.max_discovered_files {
                    budget.truncation_reason = Some("file_limit");
                    break;
                }
                budget.files_found += 1;
                paths.push(path);
            }
        }
        return Ok(paths);
    }

    let mut paths = Vec::new();
    let mut last_reported_entries = budget.entries_visited;
    let mut last_reported_at = Instant::now();
    for entry in WalkDir::new(&root.path)
        .follow_links(false)
        .max_depth(limits.max_scan_depth)
        .into_iter()
        .filter_entry(should_descend)
    {
        ensure_scan_current(is_cancelled)?;
        if budget.entries_visited >= limits.max_visited_entries {
            budget.truncation_reason = Some("entry_limit");
            break;
        }
        budget.entries_visited += 1;
        let Ok(entry) = entry else {
            continue;
        };
        if entry.file_type().is_file() && definition_for(entry.path()).is_some() {
            if budget.files_found >= limits.max_discovered_files {
                budget.truncation_reason = Some("file_limit");
                break;
            }
            budget.files_found += 1;
            paths.push(entry.into_path());
        }
        if budget.entries_visited - last_reported_entries >= PROGRESS_ENTRY_INTERVAL
            || last_reported_at.elapsed() >= PROGRESS_TIME_INTERVAL
        {
            on_progress(scan_progress(
                scan_id,
                "scanning",
                roots_done,
                roots_total,
                budget,
                started,
            ));
            last_reported_entries = budget.entries_visited;
            last_reported_at = Instant::now();
        }
    }
    Ok(paths)
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

fn add_hierarchy(
    files: &mut [DiscoveredFile],
    is_cancelled: &impl Fn() -> bool,
) -> anyhow::Result<()> {
    let snapshots = files
        .iter()
        .map(|entry| entry.item.clone())
        .collect::<Vec<_>>();
    for file in files {
        ensure_scan_current(is_cancelled)?;
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
    Ok(())
}

fn add_warnings(
    files: &mut [DiscoveredFile],
    latest_activity: &HashMap<String, DateTime<Utc>>,
    is_cancelled: &impl Fn() -> bool,
) -> anyhow::Result<()> {
    for file in files.iter_mut() {
        ensure_scan_current(is_cancelled)?;
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
        ensure_scan_current(is_cancelled)?;
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

    let directive_sets = files
        .iter()
        .map(|file| file.content.as_deref().map(directives))
        .collect::<Vec<_>>();
    for left in 0..files.len() {
        ensure_scan_current(is_cancelled)?;
        for right in (left + 1)..files.len() {
            if !same_effective_chain(&files[left].item, &files[right].item) {
                continue;
            }
            let Some(left_directives) = directive_sets[left].as_ref() else {
                continue;
            };
            let Some(right_directives) = directive_sets[right].as_ref() else {
                continue;
            };
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
    Ok(())
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

fn latest_project_activity(projects: &[ProjectContext]) -> HashMap<String, DateTime<Utc>> {
    projects
        .iter()
        .map(|project| (project.scope.clone(), project.last_event_at))
        .collect()
}

fn discover_project_root(path: &Path) -> Option<PathBuf> {
    gix::discover(path).ok().map(|repository| {
        repository
            .workdir()
            .unwrap_or_else(|| repository.git_dir())
            .to_path_buf()
    })
}

fn project_root_or(path: &Path, fallback: &Path) -> PathBuf {
    discover_project_root(path).unwrap_or_else(|| fallback.to_path_buf())
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
    is_path_ancestor(Path::new(ancestor), Path::new(descendant))
}

fn is_path_ancestor(ancestor: &Path, descendant: &Path) -> bool {
    let ancestor = normalized_path_key(ancestor);
    let descendant = normalized_path_key(descendant);
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

fn ensure_scan_current(is_cancelled: &impl Fn() -> bool) -> anyhow::Result<()> {
    if is_cancelled() {
        anyhow::bail!(SCAN_CANCELLED_ERROR);
    }
    Ok(())
}

fn elapsed_millis(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

fn scan_progress(
    scan_id: u64,
    phase: &str,
    roots_done: usize,
    roots_total: usize,
    budget: &ScanBudget,
    started: Instant,
) -> InstructionScanProgress {
    InstructionScanProgress {
        scan_id,
        phase: phase.into(),
        roots_done,
        roots_total,
        entries_visited: budget.entries_visited,
        files_found: budget.files_found,
        elapsed_ms: elapsed_millis(started),
        truncated: budget.truncation_reason.is_some(),
    }
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

    fn paths(root: &ScanRoot, limits: ScanLimits) -> (Vec<PathBuf>, ScanBudget) {
        let mut budget = ScanBudget::default();
        let result = instruction_paths(
            root,
            &mut budget,
            limits,
            1,
            0,
            1,
            Instant::now(),
            &mut |_| {},
            &|| false,
        )
        .unwrap();
        (result, budget)
    }

    fn discover_roots(roots: &[ScanRoot]) -> InstructionInventory {
        discover_from_roots(
            roots,
            &[],
            1,
            Instant::now(),
            ScanLimits::default(),
            &mut |_| {},
            &|| false,
        )
        .unwrap()
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

        let (flat, _) = paths(&root(directory.path(), false), ScanLimits::default());
        assert_eq!(flat.len(), 1);
        let (recursive, _) = paths(&root(directory.path(), true), ScanLimits::default());
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
        let inventory = discover_roots(&[root(directory.path(), true)]);
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
        let inventory = discover_roots(&[root(directory.path(), true)]);
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

    #[test]
    fn recursive_ancestor_collapses_overlapping_traversal_roots() {
        let directory = tempdir().unwrap();
        let nested = directory.path().join("nested/project");
        std::fs::create_dir_all(&nested).unwrap();
        let roots = vec![root(directory.path(), true), root(&nested, true)];
        let planned = traversal_roots(&roots);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].path, directory.path());
    }

    #[test]
    fn collapsed_traversal_keeps_most_specific_root_attribution() {
        let directory = tempdir().unwrap();
        let nested = directory.path().join("nested/project");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("AGENTS.md"), "# Project").unwrap();
        let roots = vec![
            root(directory.path(), true),
            ScanRoot {
                path: nested.clone(),
                source: "observed",
                recursive: true,
            },
        ];
        let inventory = discover_roots(&roots);
        assert_eq!(inventory.files.len(), 1);
        assert_eq!(inventory.files[0].root_path, display_path(&nested));
        assert_eq!(inventory.files[0].root_source, "observed");
    }

    #[test]
    fn scan_entry_limit_bounds_trees_without_instruction_files() {
        let directory = tempdir().unwrap();
        for index in 0..10 {
            std::fs::create_dir(directory.path().join(format!("folder-{index}"))).unwrap();
        }
        let limits = ScanLimits {
            max_visited_entries: 4,
            ..ScanLimits::default()
        };
        let (found, budget) = paths(&root(directory.path(), true), limits);
        assert!(found.is_empty());
        assert_eq!(budget.entries_visited, 4);
        assert_eq!(budget.truncation_reason, Some("entry_limit"));
    }

    #[test]
    fn repeated_working_directories_are_deduplicated_with_latest_activity() {
        let directory = tempdir().unwrap();
        let earlier = Utc::now() - Duration::days(2);
        let later = Utc::now();
        let contexts = [
            InstructionSessionContext {
                working_directory: directory.path().to_path_buf(),
                last_event_at: earlier,
            },
            InstructionSessionContext {
                working_directory: directory.path().to_path_buf(),
                last_event_at: later,
            },
        ];
        let deduplicated = deduplicate_session_contexts(&contexts);
        assert_eq!(deduplicated.len(), 1);
        assert_eq!(deduplicated[0].last_event_at, later);
    }

    #[test]
    fn non_repository_working_directories_are_not_observed_roots() {
        let directory = tempdir().unwrap();
        let projects = resolve_project_contexts(
            &[InstructionSessionContext {
                working_directory: directory.path().to_path_buf(),
                last_event_at: Utc::now(),
            }],
            &|| false,
        )
        .unwrap();
        assert!(projects.is_empty());
    }
}
