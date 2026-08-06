use odometer_lib::provider::{
    claude_code_provider_id, codex_provider_id, ProviderSource, ProviderSourceKind,
    ProviderSourceSet,
};
use odometer_lib::scan_cache::{self, ScanCache};
use odometer_lib::scanner;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-session.jsonl")
}

fn scan_ids(
    claude_root: &Path,
    cache_path: Option<&std::path::Path>,
) -> (Vec<String>, Vec<(usize, usize)>, scanner::ScanReport) {
    let sessions = Mutex::new(Vec::new());
    let progress = Mutex::new(Vec::new());
    let sources = ProviderSourceSet::try_new([ProviderSource::new(
        claude_code_provider_id(),
        claude_root.to_path_buf(),
        ProviderSourceKind::Live,
    )])
    .unwrap();
    let cache = cache_path.map(ScanCache::load);
    let report = scanner::scan_all(
        &sources,
        cache,
        |batch| {
            let mut sessions = sessions.lock().unwrap();
            for (_path, s) in batch {
                sessions.push(s.id.clone());
            }
        },
        |done, total| progress.lock().unwrap().push((done, total)),
    );
    let sessions = sessions.into_inner().unwrap();
    let progress = progress.into_inner().unwrap();
    (sessions, progress, report)
}

#[test]
fn scan_reports_progress_and_writes_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::copy(fixture(), root.join("session.jsonl")).unwrap();
    let cache_path = dir.path().join("cache.sqlite3");

    let (ids, progress, _) = scan_ids(&root, Some(&cache_path));
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
    let (real_ids, _, _) = scan_ids(&root, None);
    let cache = ScanCache::load(&cache_path); // empty, path unused yet
    assert!(cache.is_empty());
    let mut marker = odometer_lib::claude_parser::parse_file(&file)
        .unwrap()
        .unwrap();
    marker.id = "from-the-cache".into();
    cache.store(&file.to_string_lossy(), size, mtime_ms, &marker);
    cache.finish_scan();

    let (ids, _, _) = scan_ids(&root, Some(&cache_path));
    assert_eq!(ids, vec!["from-the-cache"]);
    assert_ne!(ids, real_ids);

    // Change the file: the stamp no longer matches, so it re-parses.
    let mut contents = std::fs::read_to_string(&file).unwrap();
    contents.push('\n');
    std::fs::write(&file, contents).unwrap();
    let (ids, _, _) = scan_ids(&root, Some(&cache_path));
    assert_eq!(ids, real_ids);
}

#[test]
fn cache_hit_from_a_different_provider_is_reparsed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("session.jsonl");
    std::fs::copy(fixture(), &file).unwrap();
    let cache_path = dir.path().join("cache.sqlite3");
    let (size, mtime_ms) = scan_cache::file_stamp(&file).unwrap();

    let wrong_provider = odometer_lib::parser::parse_file(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-session.jsonl"),
        false,
    )
    .unwrap()
    .unwrap();
    let cache = ScanCache::load(&cache_path);
    cache.store(&file.to_string_lossy(), size, mtime_ms, &wrong_provider);
    cache.finish_scan();

    let (ids, _, report) = scan_ids(&root, Some(&cache_path));
    assert_eq!(ids, vec!["11111111-2222-3333-4444-555555555555"]);
    assert_eq!(report.cache_hits, 0);
    assert_eq!(report.cache_misses, 1);
    assert_eq!(report.parsed_files, 1);
}

#[test]
fn cache_hit_with_a_stale_archive_classification_is_reparsed() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("archived");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("session.jsonl");
    let codex_fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample-session.jsonl");
    std::fs::copy(&codex_fixture, &file).unwrap();
    let cache_path = dir.path().join("cache.sqlite3");
    let (size, mtime_ms) = scan_cache::file_stamp(&file).unwrap();

    let live_session = odometer_lib::parser::parse_file(&file, false)
        .unwrap()
        .unwrap();
    let cache = ScanCache::load(&cache_path);
    cache.store(&file.to_string_lossy(), size, mtime_ms, &live_session);
    cache.finish_scan();

    let sources = ProviderSourceSet::try_new([ProviderSource::new(
        codex_provider_id(),
        root,
        ProviderSourceKind::Archived,
    )])
    .unwrap();
    let archived = Mutex::new(Vec::new());
    let report = scanner::scan_all(
        &sources,
        Some(ScanCache::load(&cache_path)),
        |batch| {
            let mut archived = archived.lock().unwrap();
            for (_path, session) in batch {
                archived.push(session.archived);
            }
        },
        |_done, _total| {},
    );

    assert_eq!(archived.into_inner().unwrap(), vec![true]);
    assert_eq!(report.cache_hits, 0);
    assert_eq!(report.cache_misses, 1);
    assert_eq!(report.parsed_files, 1);
}
