//! Canonical, privacy-aware project identity resolved from a session's
//! working directory (#41).
//!
//! Project identity is a *session* attribute, not an event attribute: it is
//! resolved once per observed working directory and stored on
//! `durable_sessions`, never on a per-event fact/rollup table. This
//! deliberately sidesteps the two-table rollup-dimension invariant in
//! `AGENTS.md` (`range_totals_multi` is keyed by `session_key`, and project
//! aggregation groups already-computed per-session totals rather than adding
//! a new grouping column to an event fact table).
//!
//! Resolution never fabricates confidence: when a git repository is found its
//! root (or, for a linked worktree, the shared main-repository root) is by
//! far the strongest signal and always wins. Absent that, a recognized
//! workspace-marker ancestor is used. Absent that, a Claude Code project
//! folder slug is used when the transcript path exposes one. The final
//! fallback is the working directory's own normalized path — deliberately
//! narrow, so an ambiguous case never silently merges two unrelated projects.

use crate::git_outcomes::discover_repository_identity;
use crate::paths::normalized_path_key;
use crate::provider::{claude_code_provider_id, ProviderId};
use std::path::{Path, PathBuf};

/// How a [`ProjectIdentity`] was resolved. Stored alongside the identity
/// rather than re-derived at read time, so a later change to the resolution
/// heuristics never silently reclassifies historical rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectProvenance {
    /// A local Git repository root (or, for a linked worktree, the shared
    /// main-repository root reached through `commondir`).
    RepositoryRoot,
    /// No Git repository, but a recognized workspace/package manifest was
    /// found in an ancestor directory (nearest one wins).
    WorkspaceRoot,
    /// A provider-native project grouping distinct from the raw path (today:
    /// Claude Code's `projects/<slug>` transcript directory).
    ProviderProjectId,
    /// No stronger signal was available; the normalized working-directory
    /// path itself is the identity. Two sessions land here as the same
    /// project only when their working directories are exactly the same
    /// path (case/separator-insensitive on Windows).
    FallbackPathIdentity,
}

impl ProjectProvenance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepositoryRoot => "repository_root",
            Self::WorkspaceRoot => "workspace_root",
            Self::ProviderProjectId => "provider_project_id",
            Self::FallbackPathIdentity => "fallback_path_identity",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "repository_root" => Self::RepositoryRoot,
            "workspace_root" => Self::WorkspaceRoot,
            "provider_project_id" => Self::ProviderProjectId,
            "fallback_path_identity" => Self::FallbackPathIdentity,
            _ => return None,
        })
    }
}

/// A resolved project identity: a stable key for grouping, a local display
/// label, and the provenance that produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIdentity {
    /// Stable, namespaced identity key. Namespacing by provenance
    /// (`repo:`/`workspace:`/`provider:`/`path:`) guarantees keys from
    /// different resolution strategies never collide even if their inputs
    /// happen to hash alike.
    pub project_key: String,
    /// Local display label. Never redacted here — redaction for aggregate
    /// storage/export is a presentation-layer concern applied where data
    /// leaves the desktop, matching this app's existing local-only handling
    /// of working directories.
    pub label: String,
    pub provenance: ProjectProvenance,
}

const WORKSPACE_MARKERS: &[&str] = &[
    "package.json",
    "pnpm-workspace.yaml",
    "pyproject.toml",
    "Cargo.toml",
    "go.work",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
];

/// Bounds the ancestor walk used for workspace-marker discovery so a working
/// directory near a filesystem root (or an unusually deep tree) cannot make
/// resolution unbounded.
const MAX_ANCESTOR_WALK: usize = 24;

/// FNV-1a, matching the deterministic-key convention every other module in
/// this codebase uses for local reconciliation/identity keys (see
/// `history_store::stable_hash`, `config_events::stable_hash`,
/// `correlation::stable_hash`). Not a security boundary; no hashing
/// dependency is added for it.
fn stable_hash(value: &str) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

fn strip_git_suffix(name: &str) -> &str {
    name.strip_suffix(".git").unwrap_or(name)
}

fn file_name_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

/// Resolves symlinks when the path is currently reachable. A missing or
/// inaccessible working directory (a common case for historical sessions
/// whose source has since moved or disappeared) falls back to the path as
/// recorded rather than failing resolution outright.
fn resolvable_path(raw: &Path) -> PathBuf {
    std::fs::canonicalize(raw).unwrap_or_else(|_| raw.to_path_buf())
}

/// Walks upward from `start`, returning the nearest ancestor (including
/// `start` itself) containing a recognized workspace/package manifest.
/// Deliberately the *nearest* match rather than the outermost: an outermost
/// match risks walking past unrelated sibling projects that merely share a
/// distant ancestor, which would over-merge — the one failure mode this
/// dimension must avoid more than under-merging.
fn workspace_marker_root(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    let mut steps = 0usize;
    while let Some(directory) = current {
        if WORKSPACE_MARKERS
            .iter()
            .any(|marker| directory.join(marker).is_file() || directory.join(marker).is_dir())
        {
            return Some(directory.to_path_buf());
        }
        steps += 1;
        if steps >= MAX_ANCESTOR_WALK {
            break;
        }
        current = directory.parent();
    }
    None
}

/// Claude Code lays transcripts out as `.../projects/<slug>/<uuid>.jsonl`
/// (subagent transcripts nest one directory deeper and do not match this
/// shape, which is fine: they fall through to the workspace/fallback tiers
/// below like any other session). The slug is a real provider-computed
/// grouping distinct from the raw working-directory string, so it is worth
/// preferring over the raw-path fallback even though it carries the same
/// case/separator sensitivity.
fn provider_project_slug(file_path: &str) -> Option<String> {
    let path = Path::new(file_path);
    let parent = path.parent()?;
    let slug = parent.file_name()?.to_str()?.to_owned();
    let grandparent_name = parent.parent()?.file_name()?.to_str()?;
    (grandparent_name == "projects" && !slug.is_empty()).then_some(slug)
}

/// Resolves the project identity for one session's working directory.
///
/// Returns `None` when there is no working directory at all — a session
/// without one is never forced into a synthetic bucket, since that would be
/// a guess rather than an observation.
pub fn resolve_project_identity(
    working_directory: Option<&str>,
    harness: &ProviderId,
    file_path: &str,
    home: Option<&Path>,
) -> Option<ProjectIdentity> {
    let raw = working_directory?.trim();
    if raw.is_empty() {
        return None;
    }
    let raw_path = Path::new(raw);
    let resolved = resolvable_path(raw_path);

    if let Some(identity) = discover_repository_identity(&resolved) {
        let repo_root = identity.common_root;
        let repo_name = strip_git_suffix(&file_name_label(&repo_root)).to_string();
        // Preserve the exact pre-#41 rendered label (`repo/relative/path`)
        // by computing the relative segment against the recorded raw path
        // first; a symlinked or otherwise-divergent raw path falls back to
        // the resolved path against the same root.
        let relative = raw_path
            .strip_prefix(&repo_root)
            .ok()
            .or_else(|| resolved.strip_prefix(&repo_root).ok())
            .map(|rest| rest.to_string_lossy().replace('\\', "/"))
            .filter(|segment| !segment.is_empty());
        let label = match relative {
            Some(rest) => format!("{repo_name}/{rest}"),
            None => repo_name,
        };
        let project_key = format!(
            "repo:{}",
            stable_hash(&normalized_path_key(&repo_root, true))
        );
        return Some(ProjectIdentity {
            project_key,
            label,
            provenance: ProjectProvenance::RepositoryRoot,
        });
    }

    if let Some(root) = workspace_marker_root(&resolved) {
        let project_key = format!(
            "workspace:{}",
            stable_hash(&normalized_path_key(&root, true))
        );
        return Some(ProjectIdentity {
            project_key,
            label: crate::commands::shorten_display_path(raw_path, home, 3),
            provenance: ProjectProvenance::WorkspaceRoot,
        });
    }

    if *harness == claude_code_provider_id() {
        if let Some(slug) = provider_project_slug(file_path) {
            let project_key = format!("provider:{harness}:{}", stable_hash(&slug));
            return Some(ProjectIdentity {
                project_key,
                label: crate::commands::shorten_display_path(raw_path, home, 3),
                provenance: ProjectProvenance::ProviderProjectId,
            });
        }
    }

    let project_key = format!(
        "path:{}",
        stable_hash(&normalized_path_key(&resolved, true))
    );
    Some(ProjectIdentity {
        project_key,
        label: crate::commands::shorten_display_path(raw_path, home, 3),
        provenance: ProjectProvenance::FallbackPathIdentity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{claude_code_provider_id, codex_provider_id};
    use std::path::Path;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        git(directory.path(), &["init", "--quiet"]);
        git(directory.path(), &["config", "user.name", "Synthetic Test"]);
        git(
            directory.path(),
            &["config", "user.email", "synthetic@example.invalid"],
        );
        // A local/global gitconfig with commit signing enabled must not make
        // this fixture flaky; the fixture never needs a real signature.
        git(directory.path(), &["config", "commit.gpgsign", "false"]);
        std::fs::write(directory.path().join("fixture.txt"), "content").unwrap();
        git(directory.path(), &["add", "fixture.txt"]);
        git(directory.path(), &["commit", "--quiet", "-m", "init"]);
        directory
    }

    #[test]
    fn no_working_directory_resolves_to_none() {
        assert!(resolve_project_identity(None, &codex_provider_id(), "x.jsonl", None).is_none());
        assert!(
            resolve_project_identity(Some("   "), &codex_provider_id(), "x.jsonl", None).is_none()
        );
    }

    #[test]
    fn repository_root_is_the_project_identity_with_a_relative_suffix() {
        let repo = repository();
        let nested = repo.path().join("packages/app");
        std::fs::create_dir_all(&nested).unwrap();
        let identity = resolve_project_identity(
            Some(nested.to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(identity.provenance, ProjectProvenance::RepositoryRoot);
        let repo_name = repo.path().file_name().unwrap().to_string_lossy();
        assert_eq!(identity.label, format!("{repo_name}/packages/app"));

        let at_root = resolve_project_identity(
            Some(repo.path().to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(at_root.label, repo_name);
        assert_eq!(at_root.project_key, identity.project_key);
    }

    #[test]
    fn nested_repository_does_not_collapse_into_its_container() {
        let outer = repository();
        let inner_path = outer.path().join("vendor/inner");
        std::fs::create_dir_all(&inner_path).unwrap();
        git(&inner_path, &["init", "--quiet"]);
        git(&inner_path, &["config", "user.name", "Synthetic Test"]);
        git(
            &inner_path,
            &["config", "user.email", "synthetic@example.invalid"],
        );

        let outer_identity = resolve_project_identity(
            Some(outer.path().to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        let inner_identity = resolve_project_identity(
            Some(inner_path.to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_ne!(outer_identity.project_key, inner_identity.project_key);
    }

    #[test]
    fn linked_worktrees_share_one_project_identity() {
        let main = repository();
        let branch = String::from_utf8(
            Command::new("git")
                .arg("-C")
                .arg(main.path())
                .args(["branch", "--show-current"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let branch = branch.trim();
        let linked_parent = tempfile::tempdir().unwrap();
        let linked = linked_parent.path().join("linked-worktree");
        git(
            main.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "synthetic-worktree",
                linked.to_str().unwrap(),
                branch,
            ],
        );

        let main_identity = resolve_project_identity(
            Some(main.path().to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        let linked_identity = resolve_project_identity(
            Some(linked.to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(main_identity.project_key, linked_identity.project_key);
        assert_eq!(main_identity.provenance, ProjectProvenance::RepositoryRoot);
    }

    #[test]
    fn bare_repository_resolves_without_a_workdir() {
        let bare = tempfile::tempdir().unwrap();
        let bare_path = bare.path().join("service.git");
        git(
            bare.path(),
            &["init", "--quiet", "--bare", bare_path.to_str().unwrap()],
        );
        let identity = resolve_project_identity(
            Some(bare_path.to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(identity.provenance, ProjectProvenance::RepositoryRoot);
        assert_eq!(identity.label, "service");
    }

    #[test]
    fn monorepo_subdirectories_share_the_repository_project() {
        let repo = repository();
        let a = repo.path().join("apps/a");
        let b = repo.path().join("apps/b");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let ia = resolve_project_identity(
            Some(a.to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        let ib = resolve_project_identity(
            Some(b.to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(ia.project_key, ib.project_key);
    }

    #[test]
    fn workspace_marker_is_used_without_a_git_repository() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("package.json"), "{}").unwrap();
        let nested = root.path().join("src/lib");
        std::fs::create_dir_all(&nested).unwrap();
        let identity = resolve_project_identity(
            Some(nested.to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(identity.provenance, ProjectProvenance::WorkspaceRoot);

        let identity_at_root = resolve_project_identity(
            Some(root.path().to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(identity.project_key, identity_at_root.project_key);
    }

    #[test]
    fn provider_project_id_is_used_for_claude_code_project_directories() {
        let cwd = tempfile::tempdir().unwrap();
        let file_path =
            "/home/dev/.claude/projects/-home-dev-myproject/00000000-0000-0000-0000-000000000000.jsonl"
                .to_string();
        let identity = resolve_project_identity(
            Some(cwd.path().to_str().unwrap()),
            &claude_code_provider_id(),
            &file_path,
            None,
        )
        .unwrap();
        assert_eq!(identity.provenance, ProjectProvenance::ProviderProjectId);

        // Same slug, different (but equally non-repo/non-workspace) cwd
        // string, still resolves to the same provider project id.
        let other_cwd = tempfile::tempdir().unwrap();
        let identity_other = resolve_project_identity(
            Some(other_cwd.path().to_str().unwrap()),
            &claude_code_provider_id(),
            &file_path,
            None,
        )
        .unwrap();
        assert_eq!(identity.project_key, identity_other.project_key);
    }

    #[test]
    fn fallback_path_identity_is_used_as_a_last_resort() {
        let directory = tempfile::tempdir().unwrap();
        let identity = resolve_project_identity(
            Some(directory.path().to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(identity.provenance, ProjectProvenance::FallbackPathIdentity);
    }

    #[test]
    fn windows_case_and_separator_variants_share_one_fallback_identity() {
        // `normalized_path_key` folds case and separators on Windows; this
        // guards that project identity actually goes through it rather than
        // hashing the raw string.
        let a = resolve_project_identity(
            Some(r"C:\Users\dev\Scratch"),
            &codex_provider_id(),
            "x.jsonl",
            None,
        );
        let b = resolve_project_identity(
            Some("c:/users/dev/scratch"),
            &codex_provider_id(),
            "x.jsonl",
            None,
        );
        if cfg!(windows) {
            assert_eq!(a.unwrap().project_key, b.unwrap().project_key);
        }
    }

    #[test]
    fn renamed_or_moved_folders_are_distinct_projects_not_silently_merged() {
        // Renaming a working directory is indistinguishable, from path
        // evidence alone, from an unrelated project at a new path — the
        // resolver must not guess a merge. Reconciling identity across a
        // rename is a user-driven `merge` action (history_store), not
        // something automatic resolution should attempt.
        let before = tempfile::tempdir().unwrap();
        let before_path = before.path().to_str().unwrap().to_string();
        let identity_before =
            resolve_project_identity(Some(&before_path), &codex_provider_id(), "x.jsonl", None)
                .unwrap();

        let renamed = before.path().parent().unwrap().join("renamed-project");
        std::fs::rename(before.path(), &renamed).unwrap();
        let identity_after = resolve_project_identity(
            Some(renamed.to_str().unwrap()),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();

        assert_ne!(identity_before.project_key, identity_after.project_key);
    }

    #[test]
    fn missing_working_directory_falls_back_gracefully() {
        // No git binary walk should panic or fail when the directory no
        // longer exists on disk (a common case when backfilling history for
        // sessions whose source has since moved or been deleted).
        let identity = resolve_project_identity(
            Some("/definitely/does/not/exist/anywhere-odometer-test"),
            &codex_provider_id(),
            "x.jsonl",
            None,
        )
        .unwrap();
        assert_eq!(identity.provenance, ProjectProvenance::FallbackPathIdentity);
    }
}
