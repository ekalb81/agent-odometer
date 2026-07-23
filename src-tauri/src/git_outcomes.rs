use crate::correlation::ExternalEvent;
use crate::model::Session;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Borrow;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitOutcomeKind {
    Kept,
    Reverted,
    Abandoned,
    Ambiguous,
    NotEvaluated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitOutcome {
    pub session_id: String,
    pub repository_scope: Option<String>,
    pub kind: GitOutcomeKind,
    pub commit_ids: Vec<String>,
    pub evidence: String,
}

#[derive(Clone)]
struct CommitInfo {
    id: String,
    timestamp: DateTime<Utc>,
    reverts: Option<String>,
}

fn repository_scope(repo: &gix::Repository) -> PathBuf {
    repo.work_dir()
        .unwrap_or_else(|| repo.git_dir())
        .to_path_buf()
}

fn load_commits(repo: &gix::Repository) -> anyhow::Result<Vec<CommitInfo>> {
    let head = repo.head_commit()?;
    let mut commits = Vec::new();
    let mut pending: Vec<gix::ObjectId> = vec![head.id];
    let mut seen: std::collections::HashSet<gix::ObjectId> = std::collections::HashSet::new();
    while let Some(id) = pending.pop() {
        if commits.len() >= 50_000 || !seen.insert(id) {
            continue;
        }
        let commit = repo.find_object(id)?.into_commit();
        let timestamp = DateTime::from_timestamp(commit.time()?.seconds, 0)
            .ok_or_else(|| anyhow::anyhow!("commit timestamp out of range"))?;
        let message = String::from_utf8_lossy(commit.message_raw_sloppy());
        let reverts = message.find("This reverts commit ").and_then(|index| {
            message[index + 20..]
                .split(|ch: char| !ch.is_ascii_hexdigit())
                .next()
                .filter(|value| value.len() >= 7)
                .map(str::to_owned)
        });
        pending.extend(commit.parent_ids().map(|parent| parent.detach()));
        commits.push(CommitInfo {
            id: id.to_hex().to_string(),
            timestamp,
            reverts,
        });
    }
    commits.sort_by_key(|commit| commit.timestamp);
    Ok(commits)
}

pub fn evaluate<S: Borrow<Session>>(
    sessions: &[S],
    post_window_hours: i64,
) -> (Vec<GitOutcome>, Vec<ExternalEvent>) {
    let mut repo_cache: HashMap<PathBuf, Result<Vec<CommitInfo>, String>> = HashMap::new();
    let mut outcomes = Vec::new();
    for session in sessions {
        let session = session.borrow();
        let Some(cwd) = session.working_directory.as_deref() else {
            outcomes.push(GitOutcome {
                session_id: session.id.clone(),
                repository_scope: None,
                kind: GitOutcomeKind::NotEvaluated,
                commit_ids: Vec::new(),
                evidence: "session has no working directory".into(),
            });
            continue;
        };
        let repo = match gix::discover(Path::new(cwd)) {
            Ok(repo) => repo,
            Err(_) => {
                outcomes.push(GitOutcome {
                    session_id: session.id.clone(),
                    repository_scope: None,
                    kind: GitOutcomeKind::NotEvaluated,
                    commit_ids: Vec::new(),
                    evidence: "no containing local repository".into(),
                });
                continue;
            }
        };
        let scope_path = repository_scope(&repo);
        let scope = scope_path.to_string_lossy().into_owned();
        let commits = repo_cache
            .entry(scope_path)
            .or_insert_with(|| load_commits(&repo).map_err(|error| error.to_string()));
        let Ok(commits) = commits else {
            outcomes.push(GitOutcome {
                session_id: session.id.clone(),
                repository_scope: Some(scope),
                kind: GitOutcomeKind::NotEvaluated,
                commit_ids: Vec::new(),
                evidence: "local commit graph could not be read".into(),
            });
            continue;
        };
        let window_end = session.last_event_at + Duration::hours(post_window_hours.max(0));
        let start = commits.partition_point(|commit| commit.timestamp < session.started_at);
        let end = commits.partition_point(|commit| commit.timestamp <= window_end);
        let matched = &commits[start.min(end)..end];
        let reverted = matched.iter().any(|candidate| {
            commits.iter().rev().any(|later| {
                later.timestamp >= candidate.timestamp
                    && later
                        .reverts
                        .as_deref()
                        .is_some_and(|id| candidate.id.starts_with(id))
            })
        });
        let kind = if matched.is_empty() {
            GitOutcomeKind::Abandoned
        } else if reverted {
            GitOutcomeKind::Reverted
        } else {
            GitOutcomeKind::Kept
        };
        outcomes.push(GitOutcome {
            session_id: session.id.clone(),
            repository_scope: Some(scope),
            kind,
            commit_ids: matched.iter().map(|commit| commit.id.clone()).collect(),
            evidence: if matched.is_empty() {
                format!(
                    "no reachable HEAD commit within {}h",
                    post_window_hours.max(0)
                )
            } else {
                format!(
                    "{} reachable HEAD commit(s) in the activity window",
                    matched.len()
                )
            },
        });
    }

    let mut commit_sessions: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, outcome) in outcomes.iter().enumerate() {
        for id in &outcome.commit_ids {
            commit_sessions.entry(id.clone()).or_default().push(index);
        }
    }
    for indices in commit_sessions.values().filter(|indices| indices.len() > 1) {
        for index in indices {
            outcomes[*index].kind = GitOutcomeKind::Ambiguous;
            outcomes[*index].evidence = "commit overlaps multiple session windows".into();
        }
    }

    let mut events = Vec::new();
    for outcome in &outcomes {
        if matches!(outcome.kind, GitOutcomeKind::NotEvaluated) {
            continue;
        }
        let timestamp = outcome
            .commit_ids
            .first()
            .and_then(|id| {
                repo_cache
                    .values()
                    .filter_map(|commits| commits.as_ref().ok())
                    .find_map(|commits| {
                        commits
                            .iter()
                            .find(|commit| &commit.id == id)
                            .map(|commit| commit.timestamp)
                    })
            })
            .or_else(|| {
                sessions
                    .iter()
                    .map(Borrow::borrow)
                    .find(|session| session.id == outcome.session_id)
                    .map(|session| session.last_event_at)
            })
            .unwrap_or_else(Utc::now);
        let mut metadata = BTreeMap::new();
        metadata.insert("session_id".into(), outcome.session_id.clone());
        metadata.insert(
            "outcome".into(),
            format!("{:?}", outcome.kind).to_ascii_lowercase(),
        );
        metadata.insert("commit_count".into(), outcome.commit_ids.len().to_string());
        events.push(ExternalEvent {
            id: format!(
                "git:{}:{}",
                outcome.session_id,
                timestamp.timestamp_millis()
            ),
            timestamp,
            scope: outcome.repository_scope.clone(),
            source: "git".into(),
            kind: "session_outcome".into(),
            metadata,
        });
    }
    (outcomes, events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_names_are_stable_snake_case() {
        assert_eq!(
            serde_json::to_string(&GitOutcomeKind::NotEvaluated).unwrap(),
            "\"not_evaluated\""
        );
        assert_eq!(
            serde_json::to_string(&GitOutcomeKind::Ambiguous).unwrap(),
            "\"ambiguous\""
        );
    }
}
