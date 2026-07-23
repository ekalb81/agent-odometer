use crate::claude_parser::ClaudeSessionParser;
use crate::model::{Session, SessionSummary};
use crate::parser::SessionParser;
use crate::store::AppState;
use dashmap::DashMap;
use notify::EventKind;
use notify_debouncer_full::{new_debouncer, notify::RecursiveMode, DebounceEventResult};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
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

/// A parser plus when it last saw activity. Each parser holds a full Session
/// accumulator (a second copy of the session besides AppState), so idle
/// entries are evicted; the only cost of eviction is one full re-parse if
/// that file ever changes again.
struct ParserSlot {
    parser: AnyParser,
    last_touch: Instant,
}

/// Idle parsers are dropped after this long without file activity.
const PARSER_IDLE_TTL: Duration = Duration::from_secs(30 * 60);

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
    let parsers: Arc<DashMap<PathBuf, ParserSlot>> = Arc::new(DashMap::new());
    let archive_roots_arc: Arc<Vec<PathBuf>> = Arc::new(archive_roots.clone());
    let claude_roots_arc: Arc<Vec<PathBuf>> = Arc::new(claude_session_roots.clone());
    let session_index_path_arc: Arc<PathBuf> = Arc::new(session_index_path.clone());

    let parsers_cb = parsers.clone();
    let archive_roots_cb = archive_roots_arc.clone();
    let claude_roots_cb = claude_roots_arc.clone();
    let session_index_path_cb = session_index_path_arc.clone();
    // AppState owns the watcher handle; a strong capture here would create a
    // self-cycle that prevents watcher and recorder teardown.
    let state_cb = Arc::downgrade(&state);
    let app_cb = app.clone();

    let mut debouncer = new_debouncer(
        Duration::from_millis(250),
        None,
        move |result: DebounceEventResult| {
            let Some(state_cb) = state_cb.upgrade() else {
                return;
            };
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
                        let started = Instant::now();
                        let names = crate::session_index::read(path);
                        let changed = crate::session_index::apply(&state_cb.sessions, &names);
                        state_cb.performance.record_backend(
                            "watcher.session_index_refresh",
                            started,
                            true,
                            std::collections::BTreeMap::from([
                                ("names".into(), names.len().to_string()),
                                ("changed".into(), changed.len().to_string()),
                            ]),
                        );
                        for id in changed {
                            if let Some(session) = state_cb.sessions.get(&id) {
                                if let Err(e) = app_cb
                                    .emit("session-updated", &SessionSummary::of(session.as_ref()))
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
                        // Bulk-scanned and idle-evicted files may not have a
                        // parser slot, so path ownership in AppState is the
                        // source of truth for removal.
                        let parser_id = parsers_cb
                            .remove(path)
                            .and_then(|(_, slot)| slot.parser.session().map(|s| s.id.clone()));
                        if let Some((id, removed)) = state_cb.remove_session_path(path) {
                            if removed {
                                if let Err(e) = app_cb.emit("session-removed", &id) {
                                    tracing::warn!("emit session-removed failed: {}", e);
                                }
                            }
                        } else if let Some(id) = parser_id {
                            if state_cb.sessions.remove(&id).is_some() {
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
                            let parser = if is_claude {
                                AnyParser::Claude(ClaudeSessionParser::new(path.clone()))
                            } else {
                                AnyParser::Codex(SessionParser::new(path.clone(), archived))
                            };
                            ParserSlot {
                                parser,
                                last_touch: Instant::now(),
                            }
                        });
                        entry.last_touch = Instant::now();

                        let parse_started = Instant::now();
                        let parse_result = entry.parser.parse_to_end();
                        state_cb.performance.record_backend(
                            "watcher.incremental_parse",
                            parse_started,
                            parse_result.is_ok(),
                            std::collections::BTreeMap::from([(
                                "harness".into(),
                                if is_claude { "claude_code" } else { "codex" }.into(),
                            )]),
                        );
                        match parse_result {
                            Ok(true) => {}
                            Ok(false) => continue,
                            Err(e) => {
                                tracing::warn!("parse error for {:?}: {}", path, e);
                                continue;
                            }
                        }

                        if let Some(session) = entry.parser.session() {
                            let summary = SessionSummary::of(session);
                            state_cb.publish_watched_session(path, session.clone());
                            if let Err(e) = app_cb.emit("session-updated", &summary) {
                                tracing::warn!("emit session-updated failed: {}", e);
                            }
                        }
                    }
                }
            }

            // Sweep idle parsers so long-running apps don't hold a second
            // copy of every session ever touched. AppState keeps the parsed
            // session; only the incremental byte-offset state is lost.
            parsers_cb.retain(|_, slot| slot.last_touch.elapsed() < PARSER_IDLE_TTL);
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
