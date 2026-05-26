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

/// Starts a debounced recursive watcher on the given roots.
///
/// On Create/Modify of a *.jsonl file: get-or-insert a SessionParser, call
/// parse_to_end(), and if the session is Some upsert it into state and emit
/// "session-updated".
///
/// On Remove: drop the parser, remove from state, and emit "session-removed".
pub fn start(
    app: AppHandle,
    state: Arc<AppState>,
    session_roots: Vec<PathBuf>,
    archive_roots: Vec<PathBuf>,
) -> anyhow::Result<WatcherHandle> {
    let parsers: Arc<DashMap<PathBuf, SessionParser>> = Arc::new(DashMap::new());
    let archive_roots_arc: Arc<Vec<PathBuf>> = Arc::new(archive_roots.clone());

    let parsers_cb = parsers.clone();
    let archive_roots_cb = archive_roots_arc.clone();
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
                    // Only process .jsonl files.
                    if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                        continue;
                    }

                    if is_remove(&kind) {
                        // Look up the session id before dropping the parser.
                        if let Some((_, parser)) = parsers_cb.remove(path) {
                            if let Some(session) = &parser.session {
                                let id = session.id.clone();
                                state_cb.sessions.remove(&id);
                                if let Err(e) = app_cb.emit("session-removed", &id) {
                                    tracing::warn!("emit session-removed failed: {}", e);
                                }
                            }
                        }
                    } else {
                        // Create or Modify — parse incrementally.
                        let archived = archive_roots_cb
                            .iter()
                            .any(|root| path.starts_with(root));

                        let mut entry = parsers_cb
                            .entry(path.clone())
                            .or_insert_with(|| SessionParser::new(path.clone(), archived));

                        match entry.parse_to_end() {
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("parse error for {:?}: {}", path, e);
                                return;
                            }
                        }

                        if let Some(session) = &entry.session {
                            state_cb
                                .sessions
                                .insert(session.id.clone(), session.clone());
                            if let Err(e) = app_cb.emit("session-updated", session) {
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
    for root in session_roots.iter().chain(archive_roots.iter()) {
        if !root.exists() {
            tracing::info!("watch root {:?} does not exist yet, skipping", root);
            continue;
        }
        if let Err(e) = debouncer.watch(root, RecursiveMode::Recursive) {
            tracing::warn!("could not watch {:?}: {}", root, e);
        }
    }

    Ok(WatcherHandle {
        _inner: Box::new(debouncer),
    })
}

fn is_remove(kind: &EventKind) -> bool {
    matches!(kind, EventKind::Remove(_))
}
