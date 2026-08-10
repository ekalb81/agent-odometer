use crate::model::SessionSummary;
use crate::paths::strip_verbatim_prefix;
use crate::provider::{IncrementalProviderParser, ProviderRegistry, ProviderSourceSet};
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

/// A parser plus when it last saw activity. Each parser holds a full Session
/// accumulator (a second copy of the session besides AppState), so idle
/// entries are evicted; the only cost of eviction is one full re-parse if
/// that file ever changes again.
struct ParserSlot {
    parser: Box<dyn IncrementalProviderParser>,
    last_touch: Instant,
}

/// Idle parsers are dropped after this long without file activity.
const PARSER_IDLE_TTL: Duration = Duration::from_secs(30 * 60);

/// Starts a debounced recursive watcher on the given roots.
///
/// On Create/Modify of a *.jsonl file: get-or-insert a parser for the file's
/// harness, call parse_to_end(), and if the session is Some upsert it into
/// state and emit "session-updated".
///
/// On Remove: drop the parser, retain its durable history, mark its source
/// missing, and emit a refreshed "session-updated" summary.
pub fn start(
    app: AppHandle,
    state: Arc<AppState>,
    sources: ProviderSourceSet,
    session_index_path: PathBuf,
) -> anyhow::Result<WatcherHandle> {
    let parsers: Arc<DashMap<PathBuf, ParserSlot>> = Arc::new(DashMap::new());
    let sources_arc = Arc::new(sources);
    let session_index_path_arc: Arc<PathBuf> = Arc::new(session_index_path.clone());

    let parsers_cb = parsers.clone();
    let sources_cb = sources_arc.clone();
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
                            let Some(summary) = state_cb
                                .sessions
                                .get(&id)
                                .map(|entry| entry.summary.clone())
                            else {
                                continue;
                            };
                            // Issue #141 field regression: this used to load
                            // the full session (`full_session`, a ledger
                            // read of turns/token histories/tool
                            // observations) purely to hand it back to
                            // `persist_session_metadata` for a one-field
                            // change. `persist_thread_name_overlay_batch`
                            // writes `thread_name` straight from the resident
                            // summary instead — see the matching comment on
                            // `commands::spawn_scan`'s session-index overlay
                            // pass and `HistoryStore::overlay_thread_names`'s
                            // doc comment for the investigation behind that.
                            state_cb.persist_thread_name_overlay_batch(std::slice::from_ref(&(
                                id.clone(),
                                summary.thread_name.clone(),
                            )));
                            if let Err(e) = app_cb.emit("session-updated", &summary) {
                                tracing::warn!("emit session-updated failed: {}", e);
                            }
                        }
                        continue;
                    }

                    let Some(source) = sources_cb.resolve(path) else {
                        continue;
                    };
                    let provider_id = source.provider_id().clone();
                    let source_kind = source.kind();
                    let adapter = ProviderRegistry::builtin()
                        .adapter(&provider_id)
                        .expect("validated provider source has a registered adapter");
                    if !adapter.accepts_path(path) {
                        continue;
                    }

                    if is_remove(&kind) {
                        // Bulk-scanned and idle-evicted files may not have a
                        // parser slot, so path ownership in AppState is the
                        // source of truth for removal.
                        let _ = parsers_cb.remove(path);
                        if let Some(summary) = state_cb.mark_source_missing(path) {
                            if let Err(e) = app_cb.emit("session-updated", &summary) {
                                tracing::warn!("emit session-updated failed: {}", e);
                            }
                        }
                    } else {
                        // Create or Modify — parse incrementally.
                        let mut entry = match parsers_cb.entry(path.clone()).or_try_insert_with(
                            || -> anyhow::Result<ParserSlot> {
                                Ok(ParserSlot {
                                    parser: adapter
                                        .incremental_parser(path.clone(), source_kind)?,
                                    last_touch: Instant::now(),
                                })
                            },
                        ) {
                            Ok(entry) => entry,
                            Err(error) => {
                                tracing::warn!(
                                    "could not create '{}' parser for {:?}: {}",
                                    provider_id,
                                    path,
                                    error
                                );
                                continue;
                            }
                        };
                        entry.last_touch = Instant::now();

                        let parse_started = Instant::now();
                        let parse_result = entry.parser.parse_to_end();
                        state_cb.performance.record_backend(
                            "watcher.incremental_parse",
                            parse_started,
                            parse_result.is_ok(),
                            std::collections::BTreeMap::from([
                                ("harness".into(), provider_id.as_str().into()),
                                // Corpus size makes cost-vs-corpus scaling
                                // measurable inside a single recording.
                                ("corpus".into(), state_cb.sessions.len().to_string()),
                            ]),
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
                            let reconciled =
                                state_cb.reconcile_observed_session(path, session.clone());
                            let summary = SessionSummary::of(&reconciled.session);
                            state_cb.publish_watched_session(path, reconciled.session);
                            if let Err(e) = app_cb.emit("session-updated", &summary) {
                                tracing::warn!("emit session-updated failed: {}", e);
                            }
                            if let Some(displaced) = reconciled.displaced {
                                if let Err(e) =
                                    app_cb.emit("session-updated", &SessionSummary::of(&displaced))
                                {
                                    tracing::warn!("emit displaced session-updated failed: {}", e);
                                }
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
    for source in sources_arc.iter() {
        let root = source.root();
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

#[cfg(test)]
mod tests {
    use super::paths_equivalent;
    use std::path::Path;

    /// The session-index watcher compares a notify event path against the
    /// configured path. On a UNC root with long-path support active, notify
    /// delivers the verbatim spelling while configuration carries the plain
    /// one; a prefix strip that is not UNC-aware turns the former into a
    /// relative-looking `UNC\server\share\…` and the comparison fails, so
    /// index changes go undetected.
    #[test]
    fn verbatim_unc_event_paths_match_their_plain_configured_form() {
        assert!(paths_equivalent(
            Path::new(r"\\?\UNC\server\share\.codex\sessions.json"),
            Path::new(r"\\server\share\.codex\sessions.json"),
        ));
    }

    #[test]
    fn verbatim_disk_event_paths_match_their_plain_configured_form() {
        assert!(paths_equivalent(
            Path::new(r"\\?\C:\Users\dev\.codex\sessions.json"),
            Path::new(r"C:\Users\dev\.codex\sessions.json"),
        ));
    }

    #[test]
    fn distinct_paths_still_compare_unequal() {
        assert!(!paths_equivalent(
            Path::new(r"\\?\UNC\server\share\.codex\sessions.json"),
            Path::new(r"\\server\other\.codex\sessions.json"),
        ));
    }
}
