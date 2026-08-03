use odometer_lib::model::Harness;
use odometer_lib::provider::{
    claude_code_provider_id, codex_provider_id, IncrementalProviderParser, ProviderAdapter,
    ProviderId, ProviderRegistry, ProviderSource, ProviderSourceKind, ProviderSourceSet,
};
use odometer_lib::scanner;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone)]
struct ProviderCase {
    id: &'static str,
    fixture: &'static str,
    expected_session_id: &'static str,
    harness: Harness,
}

fn cases() -> [ProviderCase; 2] {
    [
        ProviderCase {
            id: "codex",
            fixture: "sample-session.jsonl",
            expected_session_id: "019e2ba6-95be-7bd2-a255-238cdf02936c",
            harness: codex_provider_id(),
        },
        ProviderCase {
            id: "claude_code",
            fixture: "claude-session.jsonl",
            expected_session_id: "11111111-2222-3333-4444-555555555555",
            harness: claude_code_provider_id(),
        },
    ]
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn adapter(id: &str) -> &'static dyn ProviderAdapter {
    let id = ProviderId::new(id).unwrap();
    ProviderRegistry::builtin().adapter(&id).unwrap()
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn registry_has_unique_stable_ids_and_unknowns_fail_closed() {
    assert_send_sync::<ProviderRegistry>();
    assert_send_sync::<ProviderSourceSet>();
    assert_send_sync::<Box<dyn IncrementalProviderParser>>();

    let registry = ProviderRegistry::builtin();
    let descriptors: Vec<_> = registry.descriptors().collect();
    let ids: Vec<_> = descriptors
        .iter()
        .map(|descriptor| descriptor.id.as_str())
        .collect();
    assert_eq!(ids, vec!["codex", "claude_code"]);
    assert_eq!(ids.iter().copied().collect::<HashSet<_>>().len(), ids.len());
    assert_eq!(registry.adapters().len(), descriptors.len());
    assert!(descriptors[0].capabilities.archived_sources);
    assert!(descriptors[0].capabilities.session_index);
    assert!(!descriptors[1].capabilities.archived_sources);
    assert!(!descriptors[1].capabilities.session_index);

    let unknown = ProviderId::new("future_provider").unwrap();
    assert!(registry.adapter(&unknown).is_none());
    let error = ProviderSourceSet::try_new([ProviderSource::new(
        unknown,
        PathBuf::from("future-sessions"),
        ProviderSourceKind::Live,
    )])
    .unwrap_err()
    .to_string();
    assert!(error.contains("future_provider"));
    assert!(ProviderId::new("FutureProvider").is_err());
    assert!(ProviderId::new("").is_err());
}

#[test]
fn source_ownership_collapses_redundancy_and_rejects_ambiguity() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("sessions");
    let child = parent.join("nested");

    let collapsed = ProviderSourceSet::try_new([
        ProviderSource::new(codex_provider_id(), child.clone(), ProviderSourceKind::Live),
        ProviderSource::new(
            codex_provider_id(),
            parent.clone(),
            ProviderSourceKind::Live,
        ),
    ])
    .unwrap();
    assert_eq!(collapsed.len(), 1);
    assert_eq!(collapsed.iter().next().unwrap().root(), parent);
    assert_eq!(
        collapsed
            .resolve(&child.join("session.jsonl"))
            .unwrap()
            .root(),
        parent
    );

    let cross_provider = ProviderSourceSet::try_new([
        ProviderSource::new(
            codex_provider_id(),
            parent.clone(),
            ProviderSourceKind::Live,
        ),
        ProviderSource::new(
            claude_code_provider_id(),
            child.clone(),
            ProviderSourceKind::Live,
        ),
    ]);
    assert!(cross_provider
        .unwrap_err()
        .to_string()
        .contains("ambiguous ownership"));

    let cross_kind = ProviderSourceSet::try_new([
        ProviderSource::new(
            codex_provider_id(),
            parent.clone(),
            ProviderSourceKind::Live,
        ),
        ProviderSource::new(codex_provider_id(), child, ProviderSourceKind::Archived),
    ]);
    assert!(cross_kind
        .unwrap_err()
        .to_string()
        .contains("ambiguous ownership"));

    let unsupported_archive = ProviderSourceSet::try_new([ProviderSource::new(
        claude_code_provider_id(),
        dir.path().join("claude-archive"),
        ProviderSourceKind::Archived,
    )]);
    assert!(unsupported_archive
        .unwrap_err()
        .to_string()
        .contains("does not support archived sources"));
}

#[cfg(windows)]
#[test]
fn source_resolution_matches_verbatim_unc_watcher_paths() {
    let root = PathBuf::from(r"\\server\share\sessions");
    let sources = ProviderSourceSet::try_new([ProviderSource::new(
        codex_provider_id(),
        root.clone(),
        ProviderSourceKind::Live,
    )])
    .unwrap();

    let resolved = sources
        .resolve(Path::new(r"\\?\UNC\server\share\sessions\thread.jsonl"))
        .expect("verbatim UNC watcher path should match the configured UNC root");
    assert_eq!(resolved.root(), root);
}

#[test]
fn every_builtin_provider_passes_full_incremental_and_truncation_contracts() {
    for case in cases() {
        let adapter = adapter(case.id);
        let source_path = fixture(case.fixture);
        assert!(adapter.accepts_path(&source_path));
        assert!(!adapter.accepts_path(Path::new("session.txt")));

        let full = adapter
            .parse_file(&source_path, ProviderSourceKind::Live)
            .unwrap()
            .unwrap();
        assert_eq!(full.id, case.expected_session_id);
        assert_eq!(full.harness, case.harness);
        assert!(!full.archived);
        assert!(adapter.accepts_cached_session(&full, ProviderSourceKind::Live));

        let bytes = std::fs::read(&source_path).unwrap();
        let line_ends: Vec<_> = bytes
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == b'\n').then_some(index))
            .collect();
        assert!(line_ends.len() >= 3);
        let complete_end = line_ends[1] + 1;
        let third_end = line_ends[2];
        let partial_end = complete_end + (third_end - complete_end) / 2;
        assert!(partial_end > complete_end);

        let temp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temp.path(), &bytes[..partial_end]).unwrap();
        let mut incremental = adapter
            .incremental_parser(temp.path().to_path_buf(), ProviderSourceKind::Live)
            .unwrap();
        assert!(incremental.parse_to_end().unwrap());
        let partial = incremental.session().unwrap();
        assert!(partial.tokens_history.is_empty());

        std::fs::write(temp.path(), &bytes).unwrap();
        assert!(incremental.parse_to_end().unwrap());
        let parsed = incremental.session().unwrap();
        assert_eq!(parsed.harness, full.harness);
        assert_eq!(parsed.tokens_total, full.tokens_total);
        assert_eq!(parsed.tokens_by_model, full.tokens_by_model);
        assert_eq!(parsed.tokens_history, full.tokens_history);
        assert_eq!(parsed.total_turns, full.total_turns);
        assert_eq!(parsed.tool_observations, full.tool_observations);

        std::fs::write(temp.path(), &bytes[..complete_end]).unwrap();
        assert!(incremental.parse_to_end().unwrap());
        let truncated = incremental.session().unwrap();
        assert_eq!(truncated.harness, case.harness);
        assert_eq!(truncated.tokens_total.total_tokens, 0);
        assert!(truncated.tokens_history.is_empty());
        assert!(truncated.tool_observations.is_empty());
    }
}

#[test]
fn archived_source_behavior_stays_provider_specific() {
    let codex = adapter("codex");
    let archived = codex
        .parse_file(
            &fixture("sample-session.jsonl"),
            ProviderSourceKind::Archived,
        )
        .unwrap()
        .unwrap();
    assert!(archived.archived);
    assert!(codex.accepts_cached_session(&archived, ProviderSourceKind::Archived));
    assert!(!codex.accepts_cached_session(&archived, ProviderSourceKind::Live));

    let claude = adapter("claude_code");
    assert!(claude
        .parse_file(
            &fixture("claude-session.jsonl"),
            ProviderSourceKind::Archived
        )
        .is_err());
}

#[test]
fn mixed_provider_scan_uses_adapters_and_keeps_progress_monotonic() {
    let dir = tempfile::tempdir().unwrap();
    let codex_root = dir.path().join("codex");
    let claude_root = dir.path().join("claude");
    std::fs::create_dir_all(&codex_root).unwrap();
    std::fs::create_dir_all(&claude_root).unwrap();
    std::fs::copy(
        fixture("sample-session.jsonl"),
        codex_root.join("codex-session.jsonl"),
    )
    .unwrap();
    std::fs::copy(
        fixture("claude-session.jsonl"),
        claude_root.join("claude-session.jsonl"),
    )
    .unwrap();
    std::fs::write(codex_root.join("ignored.txt"), "not a transcript").unwrap();

    let sources = ProviderSourceSet::try_new([
        ProviderSource::new(codex_provider_id(), codex_root, ProviderSourceKind::Live),
        ProviderSource::new(
            claude_code_provider_id(),
            claude_root,
            ProviderSourceKind::Live,
        ),
    ])
    .unwrap();
    let sessions = Mutex::new(Vec::new());
    let progress = Mutex::new(Vec::new());
    let report = scanner::scan_all(
        &sources,
        None,
        |_path, session| {
            sessions.lock().unwrap().push((session.harness, session.id));
        },
        |done, total| progress.lock().unwrap().push((done, total)),
    );

    let mut sessions = sessions.into_inner().unwrap();
    sessions.sort_by(|left, right| left.1.cmp(&right.1));
    assert_eq!(sessions.len(), 2);
    assert!(sessions
        .iter()
        .any(|session| session.0 == codex_provider_id()));
    assert!(sessions
        .iter()
        .any(|session| session.0 == claude_code_provider_id()));
    assert_eq!(report.files, 2);
    assert_eq!(report.parsed_files, 2);
    assert_eq!(report.parse_failures, 0);

    let progress = progress.into_inner().unwrap();
    assert_eq!(progress.first(), Some(&(0, 2)));
    assert_eq!(progress.last(), Some(&(2, 2)));
    assert!(progress
        .windows(2)
        .all(|pair| pair[0].0 <= pair[1].0 && pair[0].1 == pair[1].1));
}
