use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Deserialize)]
struct IndexEntry {
    id: String,
    thread_name: Option<String>,
}

/// Reads the Codex session index (`~/.codex/session_index.jsonl` by default) and
/// returns a map of session id → thread_name. Missing file returns an empty map;
/// malformed lines are logged and skipped.
pub fn read(path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return out,
        Err(e) => {
            tracing::warn!("could not read session index {:?}: {}", path, e);
            return out;
        }
    };
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<IndexEntry>(line) {
            Ok(entry) => {
                if let Some(name) = entry.thread_name {
                    if !name.is_empty() {
                        out.insert(entry.id, name);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("session index {:?} line {}: {}", path, i + 1, e);
            }
        }
    }
    out
}

/// Patches `thread_name` on every Session whose id appears in `names`.
/// Returns the list of session ids that were actually updated (so callers can
/// emit fine-grained `session-updated` events).
pub fn apply(
    sessions: &dashmap::DashMap<String, std::sync::Arc<crate::model::Session>>,
    names: &HashMap<String, String>,
) -> Vec<String> {
    let mut updated = Vec::new();
    for (id, name) in names {
        if let Some(mut entry) = sessions.get_mut(id) {
            if entry.value().thread_name.as_ref() != Some(name) {
                std::sync::Arc::make_mut(entry.value_mut()).thread_name = Some(name.clone());
                updated.push(id.clone());
            }
        }
    }
    updated
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn reads_well_formed_entries() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session_index.jsonl");
        std::fs::write(
            &path,
            r#"{"id":"a","thread_name":"Alpha","updated_at":"2026-01-01T00:00:00Z"}
{"id":"b","thread_name":"Beta","updated_at":"2026-01-02T00:00:00Z"}
"#,
        )
        .unwrap();
        let map = read(&path);
        assert_eq!(map.len(), 2);
        assert_eq!(map["a"], "Alpha");
        assert_eq!(map["b"], "Beta");
    }

    #[test]
    fn missing_file_returns_empty_map() {
        let dir = tempdir().unwrap();
        let map = read(&dir.path().join("nope.jsonl"));
        assert!(map.is_empty());
    }

    #[test]
    fn skips_malformed_lines_and_empty_names() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session_index.jsonl");
        std::fs::write(
            &path,
            r#"{"id":"a","thread_name":"Alpha"}
not-json
{"id":"b"}
{"id":"c","thread_name":""}
{"id":"d","thread_name":"Delta"}
"#,
        )
        .unwrap();
        let map = read(&path);
        assert_eq!(map.len(), 2);
        assert_eq!(map["a"], "Alpha");
        assert_eq!(map["d"], "Delta");
    }
}
