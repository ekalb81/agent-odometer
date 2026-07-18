use crate::claude_parser::ClaudeSessionParser;
use crate::model::{Session, SessionSummary};
use crate::parser::SessionParser;
use crate::store::AppState;
use dashmap::DashMap;
use notify::EventKind;
use notify_debouncer_full::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

/// Opaque handle that keeps the debouncer alive. Dropping it stops the watcher.
pub struct WatcherHandle {
    _inner: Box<dyn std::any::Any + Send + Sync>,
}

/// Per-file incremental parser, dispatching on the harness that owns the root
/// the file lives under.
enum AnyParser {
    Codex(SessionParser),
    Claude(ClaudeSessionParser),
}

impl AnyParser {
    fn parse_to_end(&mut self) -> anyhow::Result<bool> {
        match self {
            AnyParser::Codex(p) => p.parse_to_end(),
            AnyParser::Claude(p) => p.parse_to_end(),
        }
    }

    fn session(&self) -> Option<&Session> {
        match self {
            AnyParser::Codex(p) => p.session.as_ref(),
            AnyParser::Claude(p) => p.session.as_ref(),
        }
    }
}

/// Starts a debounced recursive watcher on the given roots.
///
/// On Create/Modify of a *.jsonl file: get-or-insert a parser for the file's
/// harness, call parse_to_end(), and if the session is Some upsert it into
/// state and emit "session-updated".
///
/// On Remove: drop the parser, remove from state, and emit "session-removed".
pub fn start(
    app: AppHandle,
    state: Arc<AppState>,
    session_roots: Vec<PathBuf>,
    archive_roots: Vec<PathBuf>,
    claude_session_roots: Vec<PathBuf>,
    session_index_path: PathBuf,
) -> anyhow::Result<WatcherHandle> {
    let parsers: Arc<DashMap<PathBuf, AnyParser>> = Arc::new(DashMap::new());
    let archive_roots_arc: Arc<Vec<PathBuf>> = Arc::new(archive_roots.clone());
    let claude_roots_arc: Arc<Vec<PathBuf>> = Arc::new(claude_session_roots.clone());
    let session_index_path_arc: Arc<PathBuf> = Arc::new(session_index_path.clone());

    let parsers_cb = parsers.clone();
    let archive_roots_cb = archive_roots_arc.clone();
    let claude_roots_cb = claude_roots_arc.clone();
    let session_index_path_cb = session_index_path_arc.clone();
    let state_cb = state.clone();
    let app_cb = app.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(250),
        None,
        move |result: DebounceEventResult| {
            let events = match result {
                Ok(evts) => evts,
                Err(errors) => {
                    for e in errors {
                        tracing::warn!("watcher error: {}", e);
                    }
                    return;
                }
            };

            for event in events {
                let kind = event.kind;
                for path in &event.paths {
                    // The session index lives next to the per-session files; handle it first
                    // so we don't try to parse it as a rollout JSONL.
                    // Use component-wise equality so mixed separators (notify on Windows
                    // delivers backslash paths; PathBuf::join from a slash literal does not
                    // normalize) still match.
                    if paths_equivalent(path, session_index_path_cb.as_path()) {
                        let names = crate::session_index::read(path);
                        let changed = crate::session_index::apply(&state_cb.sessions, &names);
                        for id in changed {
                            if let Some(session) = state_cb.sessions.get(&id) {
                                if let Err(e) = app_cb
                                    .emit("session-updated", &SessionSummary::of(session.value()))
                                {
                                    tracing::warn!("emit session-updated failed: {}", e);
                                }
                            }
                        }
                        continue;
                    }

                    // Only process .jsonl files.
                    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                        continue;
                    }

                    if is_remove(&kind) {
                        // Look up the session id before dropping the parser.
                        if let Some((_, parser)) = parsers_cb.remove(path) {
                            if let Some(session) = parser.session() {
                                let id = session.id.clone();
                                state_cb.sessions.remove(&id);
                                if let Err(e) = app_cb.emit("session-removed", &id) {
                                    tracing::warn!("emit session-removed failed: {}", e);
                                }
                            }
                        }
                    } else {
                        // Create or Modify — parse incrementally.
                        let is_claude = claude_roots_cb.iter().any(|root| path.starts_with(root));
                        let archived = archive_roots_cb.iter().any(|root| path.starts_with(root));

                        let mut entry = parsers_cb.entry(path.clone()).or_insert_with(|| {
                            if is_claude {
                                AnyParser::Claude(ClaudeSessionParser::new(path.clone()))
                            } else {
                                AnyParser::Codex(SessionParser::new(path.clone(), archived))
                            }
                        });

                        match entry.parse_to_end() {
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("parse error for {:?}: {}", path, e);
                                continue;
                            }
                        }

                        if let Some(session) = entry.session() {
                            let summary = SessionSummary::of(session);
                            state_cb
                                .sessions
                                .insert(session.id.clone(), session.clone());
                            if let Err(e) = app_cb.emit("session-updated", &summary) {
                                tracing::warn!("emit session-updated failed: {}", e);
                            }
                        }
                    }
                }
            }
        },
    )
    .map_err(|e| anyhow::anyhow!("failed to create debouncer: {}", e))?;

    // Watch all roots recursively. Skip roots that don't exist yet — the user
    // may not have Codex installed, or the directory will be created later.
    for root in session_roots
        .iter()
        .chain(archive_roots.iter())
        .chain(claude_session_roots.iter())
    {
        if !root.exists() {
            tracing::info!("watch root {:?} does not exist yet, skipping", root);
            continue;
        }
        if let Err(e) = debouncer.watch(root, RecursiveMode::Recursive) {
            tracing::warn!("could not watch {:?}: {}", root, e);
        }
    }

    // Watch the directory containing the session index non-recursively. We can't
    // watch a single file directly across platforms — atomic renames replace the
    // inode and the watch is lost — so we watch the parent and filter in the callback.
    if let Some(index_parent) = session_index_path.parent() {
        if index_parent.exists() {
            if let Err(e) = debouncer.watch(index_parent, RecursiveMode::NonRecursive) {
                tracing::warn!(
                    "could not watch session-index parent {:?}: {}",
                    index_parent,
                    e
                );
            }
        }
    }

    Ok(WatcherHandle {
        _inner: Box::new(debouncer),
    })
}

fn is_remove(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Remove(_))
}

/// Path equality that's robust against separator differences (notify produces
/// backslash paths on Windows; our config paths may carry forward slashes from
/// string literals joined onto the home directory) and against Windows
/// verbatim prefixes (`\\?\`) that notify may add when long-path support is
/// active. Operates only on path components after the prefix is stripped, so
/// it doesn't require the files to exist on disk.
fn paths_equivalent(a: &std::path::Path, b: &std::path::Path) -> bool {
    strip_verbatim_prefix(a)
        .components()
        .eq(strip_verbatim_prefix(b).components())
}

fn strip_verbatim_prefix(p: &std::path::Path) -> &std::path::Path {
    if let Some(s) = p.to_str() {
        if let Some(rest) = s.strip_prefix(r"\\?\") {
            return std::path::Path::new(rest);
        }
    }
    p
}
