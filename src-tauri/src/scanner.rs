use std::collections::HashMap;
use std::path::{Path, PathBuf};
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

pub fn initial_scan(
    session_roots: &[PathBuf],
    archive_roots: &[PathBuf],
) -> HashMap<String, crate::model::Session> {
    let mut map = HashMap::new();

    let roots_with_flag: Vec<(&PathBuf, bool)> = session_roots
        .iter()
        .map(|r| (r, false))
        .chain(archive_roots.iter().map(|r| (r, true)))
        .collect();

    for (root, archived) in roots_with_flag {
        for path in scan_jsonl_files(root) {
            match crate::parser::parse_file(&path, archived) {
                Ok(Some(session)) => {
                    map.insert(session.id.clone(), session);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!("failed to parse {:?}: {}", path, e);
                }
            }
        }
    }

    map
}
