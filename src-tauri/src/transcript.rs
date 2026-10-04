//! Explicit, bounded source inspection. This never feeds accounting or retained snapshots.
use crate::{config::Config, store::AppState};
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Component, Path},
    time::UNIX_EPOCH,
};

const MAX_PAGE_BYTES: usize = 256 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_SCAN_BYTES: usize = 1024 * 1024;
const MAX_RECORDS: usize = 100;

pub(crate) struct TranscriptSource {
    pub path: String,
    pub source_id: String,
    pub identity: String,
}
pub(crate) struct TranscriptSources {
    pub locations: Vec<TranscriptSource>,
    pub limited: bool,
    pub ambiguous: bool,
}

pub(crate) fn source_generation(path: &Path) -> Option<String> {
    open_regular(path).and_then(|file| generation(&file)).ok()
}
pub(crate) fn generation_identity(stamp: &str) -> &str {
    stamp
        .rsplit_once(':')
        .and_then(|(left, _)| left.rsplit_once(':'))
        .map_or("", |(identity, _)| identity)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptCursor {
    pub session_id: String,
    pub source_id: String,
    pub generation: String,
    pub offset: u64,
    /// An oversized record can span several bounded reads.
    pub record_start: u64,
    pub partial: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TranscriptRequest {
    pub session_id: String,
    pub cursor: Option<TranscriptCursor>,
    pub max_records: Option<usize>,
    pub max_bytes: Option<usize>,
    pub record_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptAvailability {
    Available,
    Partial,
    Missing,
    Unreadable,
    Unsupported,
    UnknownSession,
    CursorInvalid,
}

#[derive(Debug, Serialize)]
pub struct TranscriptRecord {
    pub id: String,
    pub byte_offset: u64,
    pub byte_length: u64,
    /// Complete raw JSON, preserving provider-specific fields and original ordering.
    pub raw_json: Option<String>,
    pub kind: Option<String>,
    pub message_id: Option<String>,
    pub issue: Option<String>,
    pub presentation: Option<crate::transcript_view::TranscriptPresentation>,
    pub context_evidence: Option<crate::context_evidence::ContextEvidence>,
}

#[derive(Debug, Serialize)]
pub struct TranscriptPage {
    pub provider: Option<String>,
    pub availability: TranscriptAvailability,
    pub issues: Vec<String>,
    pub records: Vec<TranscriptRecord>,
    pub next_cursor: Option<TranscriptCursor>,
    /// True only when this page reached the source end without omitted records.
    pub source_complete: bool,
}

fn empty(availability: TranscriptAvailability, issue: &str) -> TranscriptPage {
    TranscriptPage {
        provider: None,
        availability,
        issues: vec![issue.into()],
        records: vec![],
        next_cursor: None,
        source_complete: false,
    }
}

pub fn edit_record_bookmark(
    state: &AppState,
    edit: crate::history_store::RecordBookmark,
) -> Result<crate::history_store::RecordBookmark, String> {
    let history = state
        .history_ready()
        .ok_or("Private bookmarks require ready durable history")?;
    if edit.bookmarked {
        let page = read_for_session(
            state,
            TranscriptRequest {
                session_id: edit.identity.session_key.clone(),
                record_id: Some(edit.identity.anchor.clone()),
                max_records: Some(1),
                max_bytes: Some(MAX_PAGE_BYTES),
                ..Default::default()
            },
        );
        if !matches!(
            page.availability,
            TranscriptAvailability::Available | TranscriptAvailability::Partial
        ) || !page
            .records
            .iter()
            .any(|record| record.id == edit.identity.anchor && record.raw_json.is_some())
        {
            return Err("Bookmark target is missing or changed; reload the source record".into());
        }
    }
    history.edit_record_bookmark(&edit).map_err(|error| {
        let message = error.to_string();
        if message == "At most 500 record bookmarks are supported per session" {
            message
        } else {
            "Bookmark target or revision changed; reload bookmarks".into()
        }
    })
}

/// Accept IDs, never caller-provided paths. Durable locations prevent a displaced
/// transcript from being read through an older session's retained file_path.
pub fn read_for_session(state: &AppState, request: TranscriptRequest) -> TranscriptPage {
    if request.session_id.len() > 256 {
        return empty(TranscriptAvailability::UnknownSession, "invalid_session_id");
    }
    let sources = if let Some(history) = state.history_ready() {
        match history.transcript_sources(&request.session_id) {
            Ok(sources) => sources
                .or_else(|| {
                    state
                        .registered_transcript_source(&request.session_id)
                        .map(|source| TranscriptSources {
                            locations: vec![source],
                            limited: false,
                            ambiguous: false,
                        })
                })
                .or_else(|| {
                    state
                        .sessions
                        .contains_key(&request.session_id)
                        .then(|| TranscriptSources {
                            locations: vec![],
                            limited: false,
                            ambiguous: false,
                        })
                }),
            Err(_) => return empty(TranscriptAvailability::Unreadable, "source_lookup_failed"),
        }
    } else if let Some(source) = state.registered_transcript_source(&request.session_id) {
        Some(TranscriptSources {
            locations: vec![source],
            limited: false,
            ambiguous: false,
        })
    } else if state.sessions.contains_key(&request.session_id) {
        Some(TranscriptSources {
            locations: vec![],
            limited: false,
            ambiguous: false,
        })
    } else {
        None
    };
    let Some(sources) = sources else {
        return empty(TranscriptAvailability::UnknownSession, "unknown_session");
    };
    if sources.ambiguous {
        return empty(
            TranscriptAvailability::CursorInvalid,
            "ambiguous_source_identity",
        );
    }
    let config = match Config::load_read_only().and_then(|config| config.provider_sources()) {
        Ok(config) => config,
        Err(_) => return empty(TranscriptAvailability::Unreadable, "roots_unavailable"),
    };
    let mut failure = empty(TranscriptAvailability::Missing, "source_missing");
    let expected_start = state
        .sessions
        .get(&request.session_id)
        .map(|session| session.summary.started_at);
    if expected_start.is_none() {
        return empty(
            TranscriptAvailability::CursorInvalid,
            "source_identity_unverified",
        );
    }
    for TranscriptSource {
        path,
        source_id,
        identity,
    } in sources.locations
    {
        if request
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.source_id != source_id)
        {
            continue;
        }
        let path = Path::new(&path);
        let Some(source) = config.resolve(path) else {
            failure = empty(TranscriptAvailability::Unsupported, "source_outside_roots");
            continue;
        };
        if !request
            .session_id
            .starts_with(&format!("{}:", source.provider_id()))
        {
            failure = empty(
                TranscriptAvailability::Unsupported,
                "provider_ownership_changed",
            );
            continue;
        }
        if !matches!(
            source.provider_id().as_str(),
            "codex" | "claude_code" | "gemini_cli"
        ) || path.extension().and_then(|ext| ext.to_str()) != Some("jsonl")
        {
            failure = empty(TranscriptAvailability::Unsupported, "unsupported_format");
            continue;
        }
        match open_source(path, source.root()) {
            Ok(mut file) => {
                let observed = state.transcript_observation(&request.session_id, path);
                if observed.is_none() || generation(&file).ok() != observed {
                    failure = empty(
                        TranscriptAvailability::CursorInvalid,
                        "source_changed_or_unobserved",
                    );
                    continue;
                }
                if !matches_source_identity(
                    &mut file,
                    path,
                    source.provider_id().as_str(),
                    &identity,
                    expected_start,
                ) {
                    failure = empty(
                        TranscriptAvailability::CursorInvalid,
                        "source_identity_changed_or_unverified",
                    );
                    continue;
                }
                let mut page = read_page(file, &source_id, &request, observed.as_deref());
                page.provider = Some(source.provider_id().to_string());
                if sources.limited {
                    page.issues.push("source_location_limit".into());
                    page.availability = TranscriptAvailability::Partial;
                    page.source_complete = false;
                    if let Some(cursor) = &mut page.next_cursor {
                        cursor.partial = true;
                    }
                }
                return page;
            }
            Err(error) => {
                failure = if error.kind() == std::io::ErrorKind::NotFound {
                    empty(TranscriptAvailability::Missing, "source_missing")
                } else {
                    empty(TranscriptAvailability::Unreadable, "source_not_readable")
                }
            }
        }
    }
    if sources.limited {
        empty(TranscriptAvailability::Partial, "source_location_limit")
    } else if request.cursor.is_some() {
        empty(TranscriptAvailability::CursorInvalid, "source_changed")
    } else {
        failure
    }
}

fn denied() -> std::io::Error {
    std::io::Error::from(std::io::ErrorKind::PermissionDenied)
}

/// Bounded identity verification is independent of aggregate parsing.
fn matches_source_identity(
    file: &mut File,
    path: &Path,
    provider: &str,
    expected: &str,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
) -> bool {
    let result = (|| -> std::io::Result<bool> {
        file.seek(SeekFrom::Start(0))?;
        let mut reader = BufReader::with_capacity(8192, &mut *file);
        let mut scanned = 0;
        while scanned < MAX_SCAN_BYTES {
            let mut raw = Vec::new();
            let read = reader
                .by_ref()
                .take((MAX_RECORD_BYTES + 1) as u64)
                .read_until(b'\n', &mut raw)?;
            scanned += read;
            if read == 0 || raw.last() != Some(&b'\n') || read > MAX_RECORD_BYTES {
                return Ok(false);
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
                continue;
            };
            let (identity, timestamp) = match provider {
                "codex"
                    if value.get("type").and_then(|value| value.as_str())
                        == Some("session_meta") =>
                {
                    let Some(id) = value
                        .pointer("/payload/id")
                        .and_then(|value| value.as_str())
                    else {
                        return Ok(false);
                    };
                    (
                        format!("codex:thread:{id}"),
                        value.pointer("/payload/timestamp"),
                    )
                }
                "gemini_cli" => {
                    let Some(id) = value.get("sessionId").and_then(|value| value.as_str()) else {
                        continue;
                    };
                    (format!("gemini_cli:session:{id}"), value.get("startTime"))
                }
                "claude_code" => {
                    if value
                        .get("timestamp")
                        .and_then(|value| value.as_str())
                        .and_then(|timestamp| chrono::DateTime::parse_from_rfc3339(timestamp).ok())
                        .is_none()
                    {
                        continue;
                    }
                    let Some(id) = value.get("sessionId").and_then(|value| value.as_str()) else {
                        continue;
                    };
                    if expected.starts_with("claude_code:subagent:") {
                        let agent = value
                            .get("agentId")
                            .and_then(|value| value.as_str())
                            .map(str::to_owned)
                            .or_else(|| {
                                path.file_stem()
                                    .map(|stem| stem.to_string_lossy().into_owned())
                            });
                        let Some(agent) = agent else {
                            return Ok(false);
                        };
                        (
                            format!("claude_code:subagent:{id}:{agent}"),
                            value.get("timestamp"),
                        )
                    } else {
                        (format!("claude_code:session:{id}"), value.get("timestamp"))
                    }
                }
                _ => continue,
            };
            let actual_start = timestamp
                .and_then(|value| value.as_str())
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok());
            return Ok(identity == expected
                && started_at
                    .is_none_or(|start| actual_start.is_some_and(|actual| actual == start)));
        }
        Ok(false)
    })()
    .unwrap_or(false);
    let reset = file.seek(SeekFrom::Start(0)).is_ok();
    result && reset
}

fn open_source(path: &Path, root: &Path) -> std::io::Result<File> {
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(denied());
    }
    let root = root.canonicalize()?;
    if !root.is_dir() || !path.metadata()?.is_file() {
        return Err(denied());
    }
    let canonical = path.canonicalize()?;
    if !canonical.starts_with(&root) {
        return Err(denied());
    }
    let file = open_regular(path)?;
    if !file.metadata()?.is_file() {
        return Err(denied());
    }
    // Recheck containment and identity after opening, so a symlink/path switch
    // cannot redirect the opened handle outside the approved source root.
    let after = path.canonicalize()?;
    if !after.starts_with(&root) || after != canonical {
        return Err(denied());
    }
    let current = open_regular(&after)?;
    if file_identity(&file)? != file_identity(&current)? {
        return Err(denied());
    }
    Ok(file)
}

fn open_regular(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(denied());
    }
    Ok(file)
}

#[cfg(unix)]
fn file_identity(file: &File) -> std::io::Result<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &File) -> std::io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(format!(
        "{}:{}:{}",
        info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
    ))
}

fn generation(file: &File) -> std::io::Result<String> {
    let metadata = file.metadata()?;
    let modified = metadata
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map_err(|_| denied())?
        .as_nanos();
    Ok(format!(
        "{}:{}:{modified}",
        file_identity(file)?,
        metadata.len()
    ))
}

fn read_page(
    mut file: File,
    source_id: &str,
    request: &TranscriptRequest,
    expected_generation: Option<&str>,
) -> TranscriptPage {
    match read_page_inner(&mut file, source_id, request, expected_generation) {
        Ok(page) => page,
        Err(_) => empty(TranscriptAvailability::Unreadable, "source_read_failed"),
    }
}

fn read_page_inner(
    file: &mut File,
    source_id: &str,
    request: &TranscriptRequest,
    expected_generation: Option<&str>,
) -> std::io::Result<TranscriptPage> {
    let source_generation = generation(file)?;
    if expected_generation.is_some_and(|expected| expected != source_generation) {
        return Ok(empty(
            TranscriptAvailability::CursorInvalid,
            "source_changed_during_verification",
        ));
    }
    let identity = file_identity(file)?;
    let length = file.metadata()?.len();
    let mut offset = 0;
    let mut start = 0;
    if let Some(anchor) = &request.record_id {
        if request.cursor.is_some() || anchor.len() > 1024 {
            return Ok(empty(
                TranscriptAvailability::CursorInvalid,
                "invalid_record_anchor",
            ));
        }
        let mut parts = anchor.rsplitn(3, ':');
        let _hash = parts.next();
        let seek = parts.next().and_then(|part| part.parse::<u64>().ok());
        let namespace = parts.next();
        if namespace != Some(format!("{source_id}:{identity}").as_str())
            || seek.is_none_or(|seek| seek > length)
        {
            return Ok(empty(
                TranscriptAvailability::CursorInvalid,
                "record_anchor_changed",
            ));
        }
        offset = seek.unwrap();
        start = offset;
    }
    if let Some(cursor) = &request.cursor {
        if cursor.session_id != request.session_id
            || cursor.source_id != source_id
            || cursor.generation != source_generation
            || cursor.offset > length
            || cursor.record_start > cursor.offset
        {
            return Ok(empty(
                TranscriptAvailability::CursorInvalid,
                "source_changed",
            ));
        }
        offset = cursor.offset;
        start = cursor.record_start;
    }
    if start > 0 {
        file.seek(SeekFrom::Start(start - 1))?;
        let mut previous = [0];
        file.read_exact(&mut previous)?;
        if previous[0] != b'\n' {
            return Ok(empty(
                TranscriptAvailability::CursorInvalid,
                "invalid_boundary",
            ));
        }
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::with_capacity(8192, file);
    let max_records = request.max_records.unwrap_or(50).clamp(1, MAX_RECORDS);
    let max_bytes = request
        .max_bytes
        .unwrap_or(128 * 1024)
        .clamp(4096, MAX_PAGE_BYTES);
    let mut page = TranscriptPage {
        provider: None,
        availability: TranscriptAvailability::Available,
        issues: vec![],
        records: vec![],
        next_cursor: None,
        source_complete: false,
    };
    if request.cursor.as_ref().is_some_and(|cursor| cursor.partial) {
        page.availability = TranscriptAvailability::Partial;
        page.issues.push("earlier_records_omitted".into());
    }
    if request.record_id.is_some() && offset > 0 {
        page.availability = TranscriptAvailability::Partial;
        page.issues.push("earlier_records_not_inspected".into());
    }
    let mut scanned = 0;
    let envelope = TranscriptPage {
        provider: Some("claude_code".into()),
        availability: TranscriptAvailability::CursorInvalid,
        issues: [
            "earlier_records_omitted",
            "incomplete_trailing_record",
            "record_too_large",
            "invalid_json",
            "record_exceeds_page_limit",
            "oversized_record_continues",
            "source_location_limit",
            "earlier_records_not_inspected",
        ]
        .map(String::from)
        .to_vec(),
        records: vec![],
        source_complete: false,
        next_cursor: Some(TranscriptCursor {
            session_id: request.session_id.clone(),
            source_id: source_id.into(),
            generation: source_generation.clone(),
            offset: u64::MAX,
            record_start: u64::MAX,
            partial: false,
        }),
    };
    let mut returned = serde_json::to_vec(&envelope)?.len();
    if returned >= max_bytes {
        return Ok(empty(
            TranscriptAvailability::Partial,
            "page_budget_too_small",
        ));
    }
    let mut raw = Vec::new();
    let mut oversized = offset > start;
    while scanned < MAX_SCAN_BYTES && page.records.len() < max_records {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            if offset > start {
                page.availability = TranscriptAvailability::Partial;
                page.issues.push("incomplete_trailing_record".into());
            } else {
                page.source_complete = page.issues.is_empty();
            }
            break;
        }
        let count = buffer
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(buffer.len(), |index| index + 1)
            .min(MAX_SCAN_BYTES - scanned);
        let complete = buffer[count - 1] == b'\n';
        if !oversized && raw.len() + count <= MAX_RECORD_BYTES {
            raw.extend_from_slice(&buffer[..count]);
        } else {
            oversized = true;
            raw.clear();
        }
        reader.consume(count);
        offset += count as u64;
        scanned += count;
        if !complete {
            continue;
        }
        let (raw_json, issue) = if oversized {
            (None, Some("record_too_large".into()))
        } else {
            match String::from_utf8(std::mem::take(&mut raw)) {
                Ok(text) if serde_json::from_str::<serde_json::Value>(&text).is_ok() => {
                    (Some(text), None)
                }
                _ => (None, Some("invalid_json".into())),
            }
        };
        let parsed = raw_json
            .as_ref()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok());
        let field = |pointer: &str| {
            parsed
                .as_ref()
                .and_then(|value| value.pointer(pointer))
                .and_then(|value| value.as_str())
                .map(|value| value.chars().take(64).collect::<String>())
        };
        let anchor = crate::stable_hash::fnv1a64(
            raw_json.as_deref().unwrap_or(&source_generation).as_bytes(),
        );
        let mut record = TranscriptRecord {
            id: format!("{source_id}:{identity}:{start}:{anchor:016x}"),
            byte_offset: start,
            byte_length: offset - start,
            raw_json,
            kind: field("/type"),
            message_id: field("/message/id")
                .or_else(|| field("/payload/id"))
                .or_else(|| field("/uuid"))
                .or_else(|| field("/id")),
            issue,
            presentation: parsed.as_ref().map(crate::transcript_view::present),
            context_evidence: parsed.as_ref().map(crate::context_evidence::project),
        };
        let mut bytes = serde_json::to_vec(&record)?.len();
        if page.records.is_empty()
            && request
                .record_id
                .as_ref()
                .is_some_and(|anchor| anchor != &record.id)
        {
            return Ok(empty(
                TranscriptAvailability::CursorInvalid,
                "record_anchor_changed",
            ));
        }
        if returned + bytes > max_bytes {
            if !page.records.is_empty() {
                offset = start;
                break;
            }
            record.raw_json = None;
            record.presentation = None;
            record.context_evidence = None;
            record.issue = Some("record_exceeds_page_limit".into());
            bytes = serde_json::to_vec(&record)?.len();
            if returned + bytes > max_bytes {
                return Ok(empty(
                    TranscriptAvailability::Partial,
                    "page_budget_too_small",
                ));
            }
        }
        if let Some(issue) = &record.issue {
            page.availability = TranscriptAvailability::Partial;
            if !page.issues.contains(issue) {
                page.issues.push(issue.clone());
            }
        }
        returned += bytes + 1;
        page.records.push(record);
        start = offset;
        oversized = false;
    }
    if generation(reader.get_ref())? != source_generation {
        return Ok(empty(
            TranscriptAvailability::CursorInvalid,
            "source_changed_during_read",
        ));
    }
    if offset < length {
        page.next_cursor = Some(TranscriptCursor {
            session_id: request.session_id.clone(),
            source_id: source_id.into(),
            generation: source_generation,
            offset,
            record_start: start,
            partial: page.availability == TranscriptAvailability::Partial,
        });
    } else if offset == start {
        page.source_complete = page.issues.is_empty();
    }
    if oversized && page.next_cursor.is_some() {
        page.availability = TranscriptAvailability::Partial;
        page.issues.push("oversized_record_continues".into());
        if let Some(cursor) = &mut page.next_cursor {
            cursor.partial = true;
        }
    }
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[cfg(unix)]
    #[test]
    fn bookmarks_validate_exact_records_and_missing_sources_in_isolated_process() {
        let Some(root) = std::env::var_os("ODOMETER_BOOKMARK_TEST_ROOT") else {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "transcript::tests::bookmarks_validate_exact_records_and_missing_sources_in_isolated_process", "--nocapture"])
                .env("ODOMETER_BOOKMARK_TEST_ROOT", root.path())
                .env("HOME", root.path().join("home"))
                .env("XDG_CONFIG_HOME", root.path().join("config"))
                .env("XDG_DATA_HOME", root.path().join("data"))
                .env("XDG_CACHE_HOME", root.path().join("cache"))
                .output().unwrap();
            assert!(
                output.status.success(),
                "{} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = std::path::PathBuf::from(root);
        let sources = root.join("sources");
        std::fs::create_dir_all(&sources).unwrap();
        let mut config = Config::default().normalized();
        config.config_version = crate::config::CONFIG_VERSION;
        for settings in config.providers.values_mut() {
            settings.live_roots.clear();
            settings.archive_roots.clear();
        }
        config
            .providers
            .get_mut(&crate::provider::codex_provider_id())
            .unwrap()
            .live_roots = vec![sources.clone()];
        config.save().unwrap();
        let path = sources.join("bookmark.jsonl");
        let raw = concat!(
            "{\"type\":\"session_meta\",\"payload\":{\"id\":\"bookmark-synthetic\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n",
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"BOOKMARK_PRIVATE_BODY\"}]}}\n"
        ).replace("BOOKMARK_PRIVATE_BODY", &format!("BOOKMARK_PRIVATE_BODY{}", "x".repeat(40 * 1024)));
        std::fs::write(&path, &raw).unwrap();
        let history = std::sync::Arc::new(
            crate::history_store::HistoryStore::open(&root.join("history.sqlite")).unwrap(),
        );
        let session = crate::parser::parse_file(&path, false).unwrap().unwrap();
        let key = history.observe(&path, &session, 1).unwrap().key;
        let state = AppState::new();
        state.set_history_ready(Some(history.clone()));
        state.publish_watched_session(&path, session.clone());
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            source_generation(&path),
        );
        let request = || TranscriptRequest {
            session_id: key.clone(),
            max_records: Some(1),
            max_bytes: Some(131_072),
            ..Default::default()
        };
        let first = read_for_session(&state, request());
        assert!(
            !first.records.is_empty(),
            "{:?} {:?}",
            first.availability,
            first.issues
        );
        let second = read_for_session(
            &state,
            TranscriptRequest {
                cursor: first.next_cursor,
                ..request()
            },
        );
        assert!(
            !second.records.is_empty(),
            "{:?} {:?}",
            second.availability,
            second.issues
        );
        let anchor = second.records[0].id.clone();
        let list = history.record_bookmarks(&key).unwrap();
        let edit = crate::history_store::RecordBookmark {
            identity: crate::history_store::AnnotationIdentity {
                anchor: anchor.clone(),
                ..list.identity
            },
            revision: 0,
            bookmarked: true,
        };
        let saved = edit_record_bookmark(&state, edit.clone()).unwrap();
        assert!(edit_record_bookmark(
            &state,
            crate::history_store::RecordBookmark {
                identity: crate::history_store::AnnotationIdentity {
                    anchor: format!("{anchor}changed"),
                    ..edit.identity.clone()
                },
                ..edit.clone()
            }
        )
        .is_err());
        std::fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n").unwrap();
        let appended = crate::parser::parse_file(&path, false).unwrap().unwrap();
        history.observe(&path, &appended, 2).unwrap();
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            source_generation(&path),
        );
        let landing = read_for_session(
            &state,
            TranscriptRequest {
                record_id: Some(anchor.clone()),
                ..request()
            },
        );
        assert_eq!(landing.records[0].id, anchor);
        assert!(landing.records[0]
            .raw_json
            .as_ref()
            .unwrap()
            .contains("BOOKMARK_PRIVATE_BODY"));
        assert!(
            !serde_json::to_string(&history.record_bookmarks(&key).unwrap())
                .unwrap()
                .contains("BOOKMARK_PRIVATE_BODY")
        );
        assert!(
            !serde_json::to_string(&history.session_summaries().unwrap())
                .unwrap()
                .contains(&anchor)
        );
        // Keep the same inode, metadata and offset; only the record hash changes.
        std::fs::write(
            &path,
            raw.replace("BOOKMARK_PRIVATE_BODY", "BOOKMARK_REPLACED_BODY"),
        )
        .unwrap();
        let replaced = crate::parser::parse_file(&path, false).unwrap().unwrap();
        history.observe(&path, &replaced, 3).unwrap();
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            source_generation(&path),
        );
        assert!(edit_record_bookmark(
            &state,
            crate::history_store::RecordBookmark {
                revision: saved.revision,
                ..edit.clone()
            }
        )
        .is_err());
        let changed = read_for_session(
            &state,
            TranscriptRequest {
                record_id: Some(anchor),
                ..request()
            },
        );
        assert!(changed.records.is_empty());
        std::fs::remove_file(&path).unwrap();
        history.mark_path_missing(&path).unwrap();
        assert!(edit_record_bookmark(
            &state,
            crate::history_store::RecordBookmark {
                revision: saved.revision,
                ..edit.clone()
            }
        )
        .is_err());
        assert!(
            !edit_record_bookmark(
                &state,
                crate::history_store::RecordBookmark {
                    bookmarked: false,
                    ..saved
                }
            )
            .unwrap()
            .bookmarked
        );
        assert!(history
            .record_bookmarks(&key)
            .unwrap()
            .bookmarks
            .iter()
            .all(|row| !row.bookmarked));
    }

    /// Exercise production config loading in a child process. HOME/XDG roots
    /// are isolated without mutating the environment of parallel tests.
    #[cfg(unix)]
    #[test]
    fn source_selection_and_availability_are_verified_in_isolated_process() {
        let Some(root) = std::env::var_os("ODOMETER_TRANSCRIPT_TEST_ROOT") else {
            let root = tempfile::tempdir().unwrap();
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "transcript::tests::source_selection_and_availability_are_verified_in_isolated_process", "--nocapture"])
                .env("ODOMETER_TRANSCRIPT_TEST_ROOT", root.path())
                .env("HOME", root.path().join("home"))
                .env("XDG_CONFIG_HOME", root.path().join("config"))
                .env("XDG_DATA_HOME", root.path().join("data"))
                .env("XDG_CACHE_HOME", root.path().join("cache"))
                .env("CODEX_HOME", root.path().join("codex"))
                .env("CLAUDE_CONFIG_DIR", root.path().join("claude"))
                .output().unwrap();
            assert!(
                output.status.success(),
                "child failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        };
        let root = std::path::PathBuf::from(root);
        let sources = root.join("sources");
        std::fs::create_dir_all(&sources).unwrap();
        let mut config = Config::default().normalized();
        config.config_version = crate::config::CONFIG_VERSION;
        for settings in config.providers.values_mut() {
            settings.live_roots.clear();
            settings.archive_roots.clear();
        }
        config
            .providers
            .get_mut(&crate::provider::codex_provider_id())
            .unwrap()
            .live_roots = vec![sources.clone()];
        config.save().unwrap();
        let config_path = root.join("config/agent-odometer/config.json");
        assert!(
            config_path.is_file(),
            "config must stay inside the synthetic root"
        );
        let path = sources.join("b-observed.jsonl");
        let header = "{\"type\":\"session_meta\",\"payload\":{\"id\":\"synthetic\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n";
        let content = format!("{header}{{\"type\":\"response_item\",\"payload\":{{\"id\":\"tool\",\"output\":\"synthetic transcript-only body\"}}}}\n");
        std::fs::write(&path, &content).unwrap();
        let session = crate::parser::parse_file(&path, false).unwrap().unwrap();
        let id = session.effective_storage_id();
        let state = AppState::new();
        assert_eq!(
            read_for_session(&state, request()).availability,
            TranscriptAvailability::UnknownSession
        );
        assert_eq!(
            read_for_session(
                &state,
                TranscriptRequest {
                    session_id: "x".repeat(257),
                    ..Default::default()
                }
            )
            .issues,
            ["invalid_session_id"]
        );
        state.publish_watched_session(&path, session.clone());
        let unobserved = read_for_session(&state, request());
        assert_eq!(unobserved.issues, ["source_changed_or_unobserved"]);
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            source_generation(&path),
        );
        let available = read_for_session(&state, request());
        assert_eq!(available.availability, TranscriptAvailability::Available);
        assert_eq!(available.provider.as_deref(), Some("codex"));
        assert_eq!(available.records.len(), 2);
        assert!(available.source_complete);
        assert!(
            !serde_json::to_string(&state.sessions.get(&id).unwrap().summary)
                .unwrap()
                .contains("transcript-only body")
        );

        std::fs::write(&path, content.replace("synthetic\"", "other-session\"")).unwrap();
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            source_generation(&path),
        );
        assert_eq!(
            read_for_session(&state, request()).issues,
            ["source_identity_changed_or_unverified"]
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            read_for_session(&state, request()).availability,
            TranscriptAvailability::Missing
        );
        std::fs::create_dir(&path).unwrap();
        assert_eq!(
            read_for_session(&state, request()).availability,
            TranscriptAvailability::Unreadable
        );
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, &content).unwrap();
        state.record_transcript_observation(
            state.current_scan_generation(),
            &path,
            source_generation(&path),
        );
        std::fs::write(&config_path, b"malformed config").unwrap();
        assert_eq!(
            read_for_session(&state, request()).issues,
            ["roots_unavailable"]
        );
        config.save().unwrap();

        let outside = root.join("outside.jsonl");
        std::fs::write(&outside, &content).unwrap();
        let other_state = AppState::new();
        let outside_session = crate::parser::parse_file(&outside, false).unwrap().unwrap();
        other_state.publish_watched_session(&outside, outside_session);
        assert_eq!(
            read_for_session(&other_state, request()).issues,
            ["source_outside_roots"]
        );
        let unsupported = sources.join("unsupported.json");
        std::fs::write(&unsupported, &content).unwrap();
        let other_state = AppState::new();
        other_state.publish_watched_session(
            &unsupported,
            crate::parser::parse_file(&unsupported, false)
                .unwrap()
                .unwrap(),
        );
        assert_eq!(
            read_for_session(&other_state, request()).issues,
            ["unsupported_format"]
        );
        let other_state = AppState::new();
        let mut wrong_provider = session.clone();
        wrong_provider.harness = crate::provider::claude_code_provider_id();
        wrong_provider.storage_id = "claude_code:session:synthetic".into();
        other_state.publish_watched_session(&path, wrong_provider);
        assert_eq!(
            read_for_session(
                &other_state,
                TranscriptRequest {
                    session_id: "claude_code:session:synthetic".into(),
                    ..Default::default()
                }
            )
            .issues,
            ["provider_ownership_changed"]
        );
        state.mark_source_missing(&path);
        assert_eq!(
            read_for_session(&state, request()).availability,
            TranscriptAvailability::Missing
        );
        assert!(
            state.sessions.contains_key(&id),
            "missing bodies retain summaries"
        );

        let history_path = root.join("history.sqlite");
        let history =
            std::sync::Arc::new(crate::history_store::HistoryStore::open(&history_path).unwrap());
        // A live source remains usable before its first durable write when
        // the otherwise empty archive is Ready.
        let live_only = AppState::new();
        live_only.set_history_ready(Some(history.clone()));
        live_only.publish_watched_session(&path, session.clone());
        live_only.record_transcript_observation(
            live_only.current_scan_generation(),
            &path,
            source_generation(&path),
        );
        assert_eq!(
            read_for_session(&live_only, request()).availability,
            TranscriptAvailability::Available
        );
        live_only.mark_source_missing(&path);
        assert_eq!(
            read_for_session(&live_only, request()).availability,
            TranscriptAvailability::Missing
        );
        let first_copy = sources.join("a-unobserved.jsonl");
        std::fs::write(&first_copy, &content).unwrap();
        history.observe(&first_copy, &session, 1).unwrap();
        history.observe(&path, &session, 1).unwrap();
        let durable_state = AppState::new();
        durable_state.set_history_ready(Some(history.clone()));
        durable_state.publish_watched_session(&path, session.clone());
        durable_state.record_transcript_observation(
            durable_state.current_scan_generation(),
            &path,
            source_generation(&path),
        );
        // A first unobserved durable copy cannot hide a later verified copy.
        let mut one_record = request();
        one_record.max_records = Some(1);
        let first = read_for_session(&durable_state, one_record);
        assert_eq!(first.availability, TranscriptAvailability::Available);
        let mut next = request();
        next.cursor = first.next_cursor;
        let second = read_for_session(&durable_state, next);
        assert_eq!(second.records[0].message_id.as_deref(), Some("tool"));
        assert!(second.source_complete);
        durable_state.sessions.remove(&id);
        assert_eq!(
            read_for_session(&durable_state, request()).issues,
            ["source_identity_unverified"]
        );
        durable_state.publish_watched_session(&path, session.clone());
        for index in 0..9 {
            history
                .observe(&sources.join(format!("z-copy-{index}.jsonl")), &session, 1)
                .unwrap();
        }
        let mut one_record = request();
        one_record.max_records = Some(1);
        let limited = read_for_session(&durable_state, one_record);
        assert_eq!(limited.availability, TranscriptAvailability::Partial);
        assert!(limited
            .issues
            .iter()
            .any(|issue| issue == "source_location_limit"));
        assert!(!limited.source_complete);
        assert!(limited.next_cursor.unwrap().partial);
        durable_state.clear_sessions();
        let limited = read_for_session(&durable_state, request());
        assert_eq!(limited.availability, TranscriptAvailability::Partial);
        assert!(limited.records.is_empty());
        let mut invalid = request();
        invalid.cursor = available.next_cursor.or(Some(TranscriptCursor {
            session_id: id.clone(),
            source_id: "displaced-artifact".into(),
            generation: "old".into(),
            offset: 0,
            record_start: 0,
            partial: false,
        }));
        assert_eq!(
            read_for_session(&state, invalid).availability,
            TranscriptAvailability::CursorInvalid
        );
        // A durable collision fails closed even when this copy's header
        // matches the requested provider ID and timestamp.
        let connection = rusqlite::Connection::open(&history_path).unwrap();
        connection
            .execute(
                "UPDATE durable_sessions SET collision = 1 WHERE session_key = ?1",
                [&id],
            )
            .unwrap();
        let ambiguous = read_for_session(&durable_state, request());
        assert_eq!(ambiguous.issues, ["ambiguous_source_identity"]);
        assert!(ambiguous.records.is_empty());
        connection
            .execute(
                "UPDATE durable_sessions SET collision = 0 WHERE session_key = ?1",
                [&id],
            )
            .unwrap();
        // A corrupt location index cannot fall back to a plausible partial read.
        rusqlite::Connection::open(history_path)
            .unwrap()
            .execute_batch("DROP TABLE source_locations")
            .unwrap();
        assert_eq!(
            read_for_session(&durable_state, request()).issues,
            ["source_lookup_failed"]
        );
    }

    fn request() -> TranscriptRequest {
        TranscriptRequest {
            session_id: "codex:thread:synthetic".into(),
            ..Default::default()
        }
    }
    fn fixture(bytes: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(bytes).unwrap();
        file
    }
    fn page(file: &tempfile::NamedTempFile, request: &TranscriptRequest) -> TranscriptPage {
        read_page(file.reopen().unwrap(), "artifact-synthetic", request, None)
    }

    #[test]
    fn ordering_paging_metadata_and_stable_anchors_on_append() {
        let mut file = fixture(b"{\"type\":\"response_item\",\"payload\":{\"id\":\"m1\",\"content\":\"synthetic tool body\"}}\n{\"type\":\"user\",\"id\":\"m2\"}\n");
        let mut request = request();
        request.max_records = Some(1);
        let first = page(&file, &request);
        assert_eq!(first.records[0].kind.as_deref(), Some("response_item"));
        assert_eq!(first.records[0].message_id.as_deref(), Some("m1"));
        assert!(first.records[0]
            .raw_json
            .as_ref()
            .unwrap()
            .contains("synthetic tool body"));
        assert!(!first.source_complete);
        request.cursor = first.next_cursor.clone();
        let second = page(&file, &request);
        assert_eq!(second.records[0].message_id.as_deref(), Some("m2"));
        assert!(second.records[0].byte_offset > first.records[0].byte_offset);
        assert!(second.source_complete);
        file.write_all(b"{\"type\":\"assistant\"}\n").unwrap();
        assert_eq!(
            page(&file, &request).availability,
            TranscriptAvailability::CursorInvalid
        );
        request.cursor = None;
        assert_eq!(page(&file, &request).records[0].id, first.records[0].id);
        let replacement = fixture(b"{\"type\":\"response_item\",\"payload\":{\"id\":\"m1\",\"content\":\"synthetic tool body\"}}\n");
        assert_ne!(
            page(&replacement, &request).records[0].id,
            first.records[0].id
        );
    }

    #[test]
    fn empty_partial_and_invalid_records_remain_distinct() {
        assert!(page(&fixture(b""), &request()).source_complete);
        let file = fixture(b"{\"type\":\"user\"}\nnot-json\n{\"type\":\"assistant\"");
        let first = page(&file, &request());
        assert_eq!(first.availability, TranscriptAvailability::Partial);
        assert_eq!(first.records.len(), 2);
        assert_eq!(first.records[1].issue.as_deref(), Some("invalid_json"));
        assert!(first.issues.contains(&"incomplete_trailing_record".into()));
        assert!(!first.source_complete);
        assert!(first.next_cursor.is_none());
    }

    #[test]
    fn oversized_records_continue_with_bounded_scan_and_no_body_allocation() {
        let mut bytes = vec![b'x'; MAX_SCAN_BYTES * 2 + 10];
        bytes.extend_from_slice(b"\n{\"type\":\"user\"}\n");
        let file = fixture(&bytes);
        let mut request = request();
        let mut pages = 0;
        let mut records = vec![];
        loop {
            let result = page(&file, &request);
            assert!(serde_json::to_vec(&result).unwrap().len() <= MAX_PAGE_BYTES);
            records.extend(result.records);
            pages += 1;
            match result.next_cursor {
                Some(cursor) => request.cursor = Some(cursor),
                None => {
                    assert!(!result.source_complete);
                    break;
                }
            }
            assert!(pages < 5);
        }
        assert_eq!(pages, 3);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].issue.as_deref(), Some("record_too_large"));
        assert!(records[0].raw_json.is_none());
        assert_eq!(records[1].kind.as_deref(), Some("user"));
    }

    #[test]
    fn serialized_escaping_and_record_limits_are_bounded_without_skipping_next_record() {
        let text =
            serde_json::json!({"type":"user", "content":"\"\\\n".repeat(1000)}).to_string() + "\n";
        let file = fixture(text.repeat(3).as_bytes());
        let mut request = request();
        request.max_bytes = Some(4096);
        request.max_records = Some(usize::MAX);
        let first = page(&file, &request);
        assert!(serde_json::to_vec(&first).unwrap().len() <= 4096);
        assert_eq!(first.records.len(), 1);
        assert_eq!(
            first.records[0].issue.as_deref(),
            Some("record_exceeds_page_limit")
        );
        request.cursor = first.next_cursor;
        let second = page(&file, &request);
        assert_eq!(second.records.len(), 1);
        assert_eq!(second.records[0].byte_offset, text.len() as u64);
        assert!(!second.source_complete);
    }

    #[test]
    fn cursor_cannot_cross_session_source_or_record_boundaries() {
        let file = fixture(b"{}\n{}\n");
        let mut request = request();
        request.max_records = Some(1);
        let cursor = page(&file, &request).next_cursor.unwrap();
        for changed in [
            TranscriptCursor {
                session_id: "other".into(),
                ..cursor.clone()
            },
            TranscriptCursor {
                source_id: "other".into(),
                ..cursor.clone()
            },
            TranscriptCursor {
                offset: 1,
                record_start: 1,
                ..cursor.clone()
            },
        ] {
            request.cursor = Some(changed);
            assert_eq!(
                page(&file, &request).availability,
                TranscriptAvailability::CursorInvalid
            );
        }
    }

    #[test]
    fn missing_traversal_and_outside_sources_cannot_be_opened() {
        let root = tempfile::tempdir().unwrap();
        let outside = fixture(b"{}\n");
        assert!(open_source(outside.path(), root.path()).is_err());
        assert!(open_source(&root.path().join("missing.jsonl"), root.path()).is_err());
        assert!(open_source(&root.path().join("../escape.jsonl"), root.path()).is_err());
    }

    #[test]
    fn provider_identity_handles_metadata_prefix_and_rejects_replacement() {
        let timestamp: chrono::DateTime<chrono::Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        for (provider, identity, data) in [
            ("codex", "codex:thread:s", "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n{\"type\":\"session_meta\",\"payload\":{\"id\":\"s\",\"timestamp\":\"2026-01-02T00:00:00Z\"}}\n"),
            ("gemini_cli", "gemini_cli:session:s", "{\"sessionId\":\"s\",\"startTime\":\"2026-01-01T00:00:00Z\"}\n"),
            ("claude_code", "claude_code:session:s", "{\"type\":\"custom-title\",\"sessionId\":\"s\"}\n{\"type\":\"user\",\"sessionId\":\"s\",\"timestamp\":\"2026-01-01T00:00:00Z\"}\n"),
            ("claude_code", "claude_code:subagent:parent:agent-a", "{\"type\":\"assistant\",\"sessionId\":\"parent\",\"agentId\":\"agent-a\",\"timestamp\":\"2026-01-01T00:00:00Z\"}\n"),
        ] {
            let file = fixture(data.as_bytes());
            assert!(matches_source_identity(&mut file.reopen().unwrap(), file.path(), provider, identity, Some(timestamp)));
            assert!(!matches_source_identity(&mut file.reopen().unwrap(), file.path(), provider, identity, Some(timestamp + chrono::Duration::days(1))));
            assert!(!matches_source_identity(&mut file.reopen().unwrap(), file.path(), provider, "codex:thread:other", Some(timestamp)));
        }
    }

    #[test]
    fn anchors_can_seek_and_fail_closed_after_in_place_edit() {
        let mut file = fixture(b"{\"id\":\"first\"}\n{\"id\":\"second\"}\n");
        let all = page(&file, &request());
        let mut request = request();
        request.record_id = Some(all.records[1].id.clone());
        let sought = page(&file, &request);
        assert_eq!(sought.records[0].message_id.as_deref(), Some("second"));
        assert!(!sought.source_complete);
        file.as_file_mut().seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"{\"id\":\"first\"}\n{\"id\":\"edited\"}\n")
            .unwrap();
        assert_eq!(
            page(&file, &request).availability,
            TranscriptAvailability::CursorInvalid
        );
    }

    #[test]
    fn worst_case_serialized_envelope_and_unicode_metadata_stay_under_limit() {
        let text = serde_json::json!({"type":"😀".repeat(64), "id":"😀".repeat(64), "body":"\"\\\n".repeat(1000)}).to_string() + "\n";
        let file = fixture(text.repeat(5).as_bytes());
        let mut request = request();
        request.session_id = "\"\\".repeat(128);
        request.max_bytes = Some(4096);
        let source = "\"\\".repeat(128);
        loop {
            let result = read_page(file.reopen().unwrap(), &source, &request, None);
            assert!(serde_json::to_vec(&result).unwrap().len() <= 4096);
            match result.next_cursor {
                Some(cursor) => request.cursor = Some(cursor),
                None => break,
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn fifo_replacement_never_blocks_open() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let started = std::time::Instant::now();
        assert!(open_source(&path, root.path()).is_err());
        assert!(open_regular(&path).is_err());
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn observed_generation_rejects_same_id_start_swap_and_allows_observed_append() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("source.jsonl");
        let header = "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n";
        std::fs::write(&path, header).unwrap();
        let state = AppState::new();
        let before = source_generation(&path);
        let session = crate::parser::parse_file(&path, false).unwrap().unwrap();
        assert_eq!(before, source_generation(&path));
        state.publish_watched_session(&path, session);
        state.record_transcript_observation(state.current_scan_generation(), &path, before.clone());
        assert_eq!(
            state.transcript_observation("codex:thread:s", &path),
            before
        );
        let mut replacement = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        replacement.write_all(header.as_bytes()).unwrap();
        replacement.write_all(b"{\"type\":\"response_item\",\"payload\":{\"output\":\"different lineage synthetic\"}}\n").unwrap();
        replacement.persist(&path).unwrap();
        assert_ne!(
            source_generation(&path),
            state.transcript_observation("codex:thread:s", &path)
        );
        let before = source_generation(&path);
        let session = crate::parser::parse_file(&path, false).unwrap().unwrap();
        state.publish_watched_session(&path, session);
        state.record_transcript_observation(state.current_scan_generation(), &path, before);
        let mut writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writer.write_all(b"{\"type\":\"response_item\",\"timestamp\":\"2026-01-01T00:00:03Z\",\"payload\":{\"id\":\"append\"}}\n").unwrap();
        assert_ne!(
            source_generation(&path),
            state.transcript_observation("codex:thread:s", &path)
        );
        let before = source_generation(&path);
        let session = crate::parser::parse_file(&path, false).unwrap().unwrap();
        assert_eq!(before, source_generation(&path));
        state.publish_watched_session(&path, session);
        state.record_transcript_observation(state.current_scan_generation(), &path, before.clone());
        assert_eq!(
            state.transcript_observation("codex:thread:s", &path),
            before
        );
        state.record_transcript_observation(state.current_scan_generation(), &path, None);
        assert!(state
            .transcript_observation("codex:thread:s", &path)
            .is_none());
    }

    #[test]
    fn cache_identity_rejects_same_length_same_mtime_replacement_then_stays_warm() {
        use crate::provider::{
            codex_provider_id, ProviderSource, ProviderSourceKind, ProviderSourceSet,
        };
        let root = tempfile::tempdir().unwrap();
        let sources_root = root.path().join("sessions");
        std::fs::create_dir(&sources_root).unwrap();
        let path = sources_root.join("rollout-2026-01-01T00-00-00-synthetic.jsonl");
        let content = "{\"type\":\"session_meta\",\"payload\":{\"id\":\"s\",\"timestamp\":\"2026-01-01T00:00:00Z\"}}\n{\"type\":\"event_msg\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"payload\":{\"type\":\"user_message\",\"message\":\"alpha\"}}\n";
        std::fs::write(&path, content).unwrap();
        let original = path.metadata().unwrap();
        let sources = ProviderSourceSet::try_new([ProviderSource::new(
            codex_provider_id(),
            sources_root,
            ProviderSourceKind::Live,
        )])
        .unwrap();
        let cache_path = root.path().join("cache.sqlite");
        let scan = || {
            let result = std::sync::Mutex::new(Vec::new());
            let report = crate::scanner::scan_all(
                &sources,
                Some(crate::scan_cache::ScanCache::load(&cache_path)),
                |batch| result.lock().unwrap().extend(batch),
                |_, _| {},
            );
            (report, result.into_inner().unwrap())
        };
        let (first, _) = scan();
        assert_eq!(first.parsed_files, 1);
        let mut replacement = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        replacement
            .write_all(content.replace("alpha", "bravo").as_bytes())
            .unwrap();
        replacement
            .as_file()
            .set_modified(original.modified().unwrap())
            .unwrap();
        replacement.persist(&path).unwrap();
        assert_eq!(path.metadata().unwrap().len(), original.len());
        assert_eq!(
            path.metadata().unwrap().modified().unwrap(),
            original.modified().unwrap()
        );
        let (second, sessions) = scan();
        assert_eq!(second.cache_hits, 0);
        assert_eq!(second.parsed_files, 1);
        assert_eq!(sessions[0].1.first_user_message.as_deref(), Some("bravo"));
        let (third, sessions) = scan();
        assert_eq!(third.cache_hits, 1);
        assert_eq!(third.parsed_files, 0);
        assert_eq!(sessions[0].1.first_user_message.as_deref(), Some("bravo"));
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = fixture(b"{}\n");
        let link = root.path().join("escape.jsonl");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(open_source(&link, root.path()).is_err());
    }
}
