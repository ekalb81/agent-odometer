use odometer_lib::provider::{
    claude_code_provider_id, codex_provider_id, ProviderSource, ProviderSourceKind,
    ProviderSourceSet,
};
use odometer_lib::scan_cache::{self, ScanCache};
use odometer_lib::scanner;
use rusqlite::Connection;
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
fn version_five_cache_cannot_hide_recorded_codex_cache_writes_after_upgrade() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    std::fs::create_dir(&root).unwrap();
    let file = root.join("rollout-2026-11-18T12-00-00-01900000-0000-7000-8000-000000000001.jsonl");
    let records = [
        r#"{"timestamp":"2026-11-18T12:00:00Z","type":"session_meta","payload":{"id":"01900000-0000-7000-8000-000000000001","timestamp":"2026-11-18T12:00:00Z","cli_version":"0.170.0","source":"exec","model_provider":"openai"}}"#,
        r#"{"timestamp":"2026-11-18T12:00:01Z","type":"turn_context","payload":{"turn_id":"turn-1","model":"gpt-6.2-sol"}}"#,
        r#"{"timestamp":"2026-11-18T12:00:02Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":40,"cache_write_input_tokens":15,"output_tokens":20,"total_tokens":120},"last_token_usage":{"input_tokens":100,"cached_input_tokens":40,"cache_write_input_tokens":15,"output_tokens":20,"total_tokens":120}}}}"#,
    ];
    std::fs::write(&file, records.join("\n") + "\n").unwrap();
    let cache_path = temp.path().join("cache.sqlite3");
    let sources = ProviderSourceSet::try_new([ProviderSource::new(
        codex_provider_id(),
        root,
        ProviderSourceKind::Live,
    )])
    .unwrap();
    let scan = || {
        let writes = Mutex::new(Vec::new());
        let report = scanner::scan_all(
            &sources,
            Some(ScanCache::load(&cache_path)),
            |batch| {
                writes.lock().unwrap().extend(
                    batch
                        .into_iter()
                        .map(|(_, session)| session.tokens_total.cache_creation_input_tokens),
                );
            },
            |_, _| {},
        );
        (report, writes.into_inner().unwrap())
    };
    assert_eq!(scan().1, [15]);

    // Preserve the real cached file identity and stamps. Replay the row
    // written by the previous parser, whose token totals lacked cache writes.
    let connection = Connection::open(&cache_path).unwrap();
    let cached: Vec<u8> = connection
        .query_row("SELECT session_json FROM sessions", [], |row| row.get(0))
        .unwrap();
    let old = String::from_utf8(cached).unwrap().replace(
        "\"cache_creation_input_tokens\":15",
        "\"cache_creation_input_tokens\":0",
    );
    connection
        .execute("UPDATE sessions SET session_json = ?1", [old.as_bytes()])
        .unwrap();
    connection
        .execute_batch(
            "UPDATE cache_meta SET value='5' WHERE key='parse_version';
             UPDATE cache_meta SET value='0.8.29' WHERE key='app_version';",
        )
        .unwrap();
    drop(connection);

    let (upgraded, writes) = scan();
    assert_eq!(upgraded.cache_hits, 0);
    assert_eq!(upgraded.parsed_files, 1);
    assert_eq!(writes, [15]);
    let (warm, writes) = scan();
    assert_eq!(warm.cache_hits, 1);
    assert_eq!(writes, [15]);
}

#[test]
fn legacy_cache_entry_is_reparsed_once_then_warms_normally() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projects");
    std::fs::create_dir_all(&root).unwrap();
    let file = root.join("session.jsonl");
    std::fs::copy(fixture(), &file).unwrap();
    let cache_path = dir.path().join("cache.sqlite3");

    // Legacy entries have size/mtime but no evidence of the parsed OS artifact.
    // They must miss once, rather than vouch for a replacement with that stamp.
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

    let (ids, _, report) = scan_ids(&root, Some(&cache_path));
    assert_eq!(ids, real_ids);
    assert_eq!(report.cache_hits, 0);
    assert_eq!(report.parsed_files, 1);

    let (ids, _, report) = scan_ids(&root, Some(&cache_path));
    assert_eq!(ids, real_ids);
    assert_eq!(report.cache_hits, 1);
    assert_eq!(report.parsed_files, 0);

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
