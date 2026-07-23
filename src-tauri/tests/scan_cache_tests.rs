use odometer_lib::scan_cache::{self, ScanCache};
use odometer_lib::scanner;
use std::path::PathBuf;
use std::sync::Mutex;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-session.jsonl")
}

fn scan_ids(
    claude_root: &PathBuf,
    cache_path: Option<&std::path::Path>,
) -> (Vec<String>, Vec<(usize, usize)>) {
    let sessions = Mutex::new(Vec::new());
    let progress = Mutex::new(Vec::new());
    scanner::scan_all(
        &[],
        &[],
        std::slice::from_ref(claude_root),
        cache_path,
        |_path, s| sessions.lock().unwrap().push(s.id.clone()),
        |done, total| progress.lock().unwrap().push((done, total)),
    );
    (
        sessions.into_inner().unwrap(),
        progress.into_inner().unwrap(),
    )
}

#[test]
fn scan_reports_progress_and_writes_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::copy(fixture(), root.join("session.jsonl")).unwrap();
    let cache_path = dir.path().join("cache.sqlite3");

    let (ids, progress) = scan_ids(&root, Some(&cache_path));
    assert_eq!(ids, vec!["11111111-2222-3333-4444-555555555555"]);
    // Progress starts at (0, total) and ends at (total, total).
    assert_eq!(progress.first(), Some(&(0, 1)));
    assert_eq!(progress.last(), Some(&(1, 1)));
    assert!(cache_path.exists(), "cache file written after a miss");

    let cache = ScanCache::load(&cache_path);
    assert_eq!(cache.len(), 1);
}

#[test]
fn matching_cache_entry_is_served_without_parsing() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("session.jsonl");
    std::fs::copy(fixture(), &file).unwrap();
    let cache_path = dir.path().join("cache.sqlite3");

    // Fabricate a cache entry with a marker id and the file's real stamp; a
    // hit must return the cached session, proving no re-parse happened.
    let (size, mtime_ms) = scan_cache::file_stamp(&file).unwrap();
    let (real_ids, _) = scan_ids(&root, None);
    let cache = ScanCache::load(&cache_path); // empty, path unused yet
    assert!(cache.is_empty());
    let mut marker = odometer_lib::claude_parser::parse_file(&file)
        .unwrap()
        .unwrap();
    marker.id = "from-the-cache".into();
    cache.store(&file.to_string_lossy(), size, mtime_ms, &marker);
    cache.finish_scan();

    let (ids, _) = scan_ids(&root, Some(&cache_path));
    assert_eq!(ids, vec!["from-the-cache"]);
    assert_ne!(ids, real_ids);

    // Change the file: the stamp no longer matches, so it re-parses.
    let mut contents = std::fs::read_to_string(&file).unwrap();
    contents.push('\n');
    std::fs::write(&file, contents).unwrap();
    let (ids, _) = scan_ids(&root, Some(&cache_path));
    assert_eq!(ids, real_ids);
}
