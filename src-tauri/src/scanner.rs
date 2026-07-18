use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rayon::prelude::*;
use walkdir::WalkDir;

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
/// each file finishes parsing. When duplicate session IDs exist under
/// multiple roots, callback order (and thus which one wins in the caller's
/// map) is nondeterministic.
pub fn scan_all<F>(
    session_roots: &[PathBuf],
    archive_roots: &[PathBuf],
    claude_session_roots: &[PathBuf],
    on_session: F,
) where
    F: Fn(crate::model::Session) + Send + Sync,
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

    work.par_iter().for_each(|(path, kind)| {
        let result = match kind {
            FileKind::Codex { archived } => crate::parser::parse_file(path, *archived),
            FileKind::Claude => crate::claude_parser::parse_file(path),
        };
        match result {
            Ok(Some(session)) => on_session(session),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("failed to parse {:?}: {}", path, e);
            }
        }
    });
}

pub fn initial_scan(
    session_roots: &[PathBuf],
    archive_roots: &[PathBuf],
    claude_session_roots: &[PathBuf],
) -> HashMap<String, crate::model::Session> {
    let map = Mutex::new(HashMap::new());

    scan_all(
        session_roots,
        archive_roots,
        claude_session_roots,
        |session| {
            map.lock().unwrap().insert(session.id.clone(), session);
        },
    );

    map.into_inner().unwrap()
}
