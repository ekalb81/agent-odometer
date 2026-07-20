use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::scan_cache::{self, CacheEntry, ScanCache};

pub fn scan_jsonl_files(root: &Path) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }

    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().and_then(|s| s.to_str()) == Some("jsonl")
        })
        .map(|e| e.path().to_path_buf())
        .collect()
}

#[derive(Clone, Copy)]
enum FileKind {
    Codex { archived: bool },
    Claude,
}

/// Scans all roots in parallel, invoking `on_session` from worker threads as
/// each file finishes, and `on_progress(done, total)` after every file.
/// When `cache_path` is set, files whose (size, mtime) match the cache are
/// served from it without being read or parsed, and a refreshed cache is
/// written back when anything changed. When duplicate session IDs exist
/// under multiple roots, callback order (and thus which one wins in the
/// caller's map) is nondeterministic.
pub fn scan_all<F, P>(
    session_roots: &[PathBuf],
    archive_roots: &[PathBuf],
    claude_session_roots: &[PathBuf],
    cache_path: Option<&Path>,
    on_session: F,
    on_progress: P,
) where
    F: Fn(crate::model::Session) + Send + Sync,
    P: Fn(usize, usize) + Send + Sync,
{
    let mut work: Vec<(PathBuf, FileKind)> = Vec::new();

    for root in session_roots {
        for path in scan_jsonl_files(root) {
            work.push((path, FileKind::Codex { archived: false }));
        }
    }
    for root in archive_roots {
        for path in scan_jsonl_files(root) {
            work.push((path, FileKind::Codex { archived: true }));
        }
    }
    for root in claude_session_roots {
        for path in scan_jsonl_files(root) {
            work.push((path, FileKind::Claude));
        }
    }

    let total = work.len();
    on_progress(0, total);

    let loaded = cache_path.map(ScanCache::load).unwrap_or_default();
    let loaded_len = loaded.len();
    // Hits are MOVED out of here (and into next_entries) rather than cloned,
    // so at most two full copies of the corpus exist during a scan — the
    // cache being rebuilt and the sessions handed to the caller.
    let cache_entries = Mutex::new(loaded.into_entries());
    let done = AtomicUsize::new(0);
    let hits = AtomicUsize::new(0);
    // The stamp is taken BEFORE parsing so a file that grows mid-parse looks
    // changed on the next launch rather than serving a stale cache entry.
    let next_entries: Mutex<std::collections::HashMap<String, CacheEntry>> =
        Mutex::new(std::collections::HashMap::new());

    work.par_iter().for_each(|(path, kind)| {
        let key = path.to_string_lossy().into_owned();
        let stamp = scan_cache::file_stamp(path);

        let taken = cache_entries.lock().unwrap().remove(&key);
        let cached = match (taken, stamp) {
            (Some(entry), Some((size, mtime_ms)))
                if entry.size == size && entry.mtime_ms == mtime_ms =>
            {
                Some(entry)
            }
            // Stamp mismatch or unreadable file: the stale entry just drops.
            _ => None,
        };

        let (session, entry) = match cached {
            Some(entry) => {
                hits.fetch_add(1, Ordering::Relaxed);
                (Some(entry.session.clone()), Some(entry))
            }
            None => {
                let result = match kind {
                    FileKind::Codex { archived } => crate::parser::parse_file(path, *archived),
                    FileKind::Claude => crate::claude_parser::parse_file(path),
                };
                match result {
                    Ok(Some(session)) => {
                        let entry = stamp.map(|(size, mtime_ms)| CacheEntry {
                            size,
                            mtime_ms,
                            session: session.clone(),
                        });
                        (Some(session), entry)
                    }
                    Ok(None) => (None, None),
                    Err(e) => {
                        tracing::warn!("failed to parse {:?}: {}", path, e);
                        (None, None)
                    }
                }
            }
        };

        if let Some(entry) = entry.filter(|_| cache_path.is_some()) {
            next_entries.lock().unwrap().insert(key, entry);
        }
        if let Some(session) = session {
            on_session(session);
        }

        let d = done.fetch_add(1, Ordering::Relaxed) + 1;
        on_progress(d, total);
    });

    if let Some(cache_path) = cache_path {
        let next_entries = next_entries.into_inner().unwrap();
        // Rewrite only when something actually changed: a parse happened
        // (miss) or entries disappeared relative to the loaded cache.
        let dirty =
            hits.load(Ordering::Relaxed) != next_entries.len() || next_entries.len() != loaded_len;
        if dirty {
            ScanCache::save(cache_path, next_entries);
        }
    }
}
