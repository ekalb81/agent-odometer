//! Internal action transaction engine. No provider target is registered and
//! the production entry point always refuses writes. The synthetic target
//! constructor is test-only; preview IPC never calls this module.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
#[cfg(not(windows))]
use std::fs::{File, OpenOptions};
#[cfg(windows)]
use std::io::Read;
use std::io::Write;
#[cfg(not(windows))]
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_TARGET_BYTES: u64 = 128 * 1024;
const MAX_JOURNAL_BYTES: u64 = 4096;
const JOURNAL_VERSION: u32 = 1;
const ACTION_VERSION: u32 = 1;
const JOURNAL_SUFFIXES: [&str; 6] = [
    "prepared",
    "applied",
    "aborted",
    "conflict",
    "undo-conflict",
    "undone",
];
const PRODUCTION_WRITES_ENABLED: bool = false;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, PartialEq, Eq)]
enum ActionError {
    Disabled,
    Conflict,
    UnsafeTarget,
    InvalidJournal,
    Interrupted,
    Io,
}

impl From<std::io::Error> for ActionError {
    fn from(_: std::io::Error) -> Self {
        Self::Io
    }
}

#[cfg(windows)]
impl From<crate::action_windows_privacy::PrivacyError> for ActionError {
    fn from(error: crate::action_windows_privacy::PrivacyError) -> Self {
        match error {
            crate::action_windows_privacy::PrivacyError::Unsafe => Self::UnsafeTarget,
            crate::action_windows_privacy::PrivacyError::Io => Self::Io,
        }
    }
}

/// Only reviewed adapters may construct targets. There is deliberately no
/// path or replacement field in the public action draft or Tauri IPC.
struct ResolvedTarget {
    root: PathBuf,
    file: PathBuf,
    journal: PathBuf,
    redacted_identity: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct JournalRecord {
    version: u32,
    action_id: String,
    action_version: u32,
    redacted_target: String,
    source_revision: String,
    before_sha256: String,
    after_sha256: String,
    result: JournalResult,
    undo_state: UndoState,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalResult {
    Prepared,
    Applied,
    Aborted,
    Conflict,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum UndoState {
    NotApplied,
    Available,
    Undone,
    BlockedExternalEdit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Recovery {
    Aborted,
    Applied,
    Undone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InterruptAt {
    Never,
    AfterPrepared,
    AfterTargetReplace,
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn action_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:032x}-{:08x}-{sequence:016x}", std::process::id())
}

fn checked_target(target: &ResolvedTarget) -> Result<(), ActionError> {
    let root = target.root.canonicalize()?;
    let parent = target.file.parent().ok_or(ActionError::UnsafeTarget)?;
    if parent.canonicalize()? != root || target.file.file_name().is_none() {
        return Err(ActionError::UnsafeTarget);
    }
    let metadata = fs::symlink_metadata(&target.file)?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_TARGET_BYTES {
        return Err(ActionError::UnsafeTarget);
    }
    if target.redacted_identity.is_empty()
        || target.redacted_identity.len() > 128
        || target.redacted_identity.chars().any(char::is_control)
    {
        return Err(ActionError::UnsafeTarget);
    }
    if target
        .journal
        .parent()
        .ok_or(ActionError::UnsafeTarget)?
        .canonicalize()?
        != root
        || target.journal.file_name().is_none()
    {
        return Err(ActionError::UnsafeTarget);
    }
    Ok(())
}

#[cfg(not(windows))]
fn ensure_journal_dir(target: &ResolvedTarget) -> Result<(), ActionError> {
    checked_target(target)?;
    if !target.journal.exists() {
        #[cfg(unix)]
        let mut builder = fs::DirBuilder::new();
        #[cfg(not(unix))]
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&target.journal) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ActionError::Io),
        }
    }
    let metadata = fs::symlink_metadata(&target.journal)?;
    if !metadata.file_type().is_dir() {
        return Err(ActionError::UnsafeTarget);
    }
    if target.journal.canonicalize()?
        != target.root.canonicalize()?.join(
            target
                .journal
                .file_name()
                .ok_or(ActionError::UnsafeTarget)?,
        )
    {
        return Err(ActionError::UnsafeTarget);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(ActionError::UnsafeTarget);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn open_private_journal(
    target: &ResolvedTarget,
) -> Result<crate::action_windows_privacy::PrivateJournal, ActionError> {
    checked_target(target)?;
    Ok(
        crate::action_windows_privacy::PrivateJournal::open_or_create(
            &target.root,
            &target.journal,
        )?,
    )
}

#[cfg(windows)]
fn open_existing_private_journal(
    target: &ResolvedTarget,
) -> Result<Option<crate::action_windows_privacy::PrivateJournal>, ActionError> {
    checked_target(target)?;
    Ok(
        crate::action_windows_privacy::PrivateJournal::open_existing(
            &target.root,
            &target.journal,
        )?,
    )
}

fn read_target(target: &ResolvedTarget) -> Result<Vec<u8>, ActionError> {
    checked_target(target)?;
    crate::harness_integration::read_bounded_regular_file(&target.file, MAX_TARGET_BYTES)
        .map_err(|_| ActionError::UnsafeTarget)
}

fn valid_id(id: &str) -> bool {
    id.len() == 58
        && id.as_bytes()[32] == b'-'
        && id.as_bytes()[41] == b'-'
        && id
            .bytes()
            .enumerate()
            .all(|(index, byte)| index == 32 || index == 41 || byte.is_ascii_hexdigit())
}

fn journal_path(target: &ResolvedTarget, id: &str, suffix: &str) -> Result<PathBuf, ActionError> {
    if !valid_id(id)
        || !matches!(
            suffix,
            "prepared" | "applied" | "aborted" | "conflict" | "undo-conflict" | "undone"
        )
    {
        return Err(ActionError::InvalidJournal);
    }
    Ok(target.journal.join(format!("{id}.{suffix}.json")))
}

fn backup_path(target: &ResolvedTarget, id: &str) -> Result<PathBuf, ActionError> {
    if !valid_id(id) {
        return Err(ActionError::InvalidJournal);
    }
    Ok(target.journal.join(format!("{id}.backup")))
}

#[cfg(not(windows))]
fn private_new_file(path: &Path) -> Result<File, ActionError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn canonical_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_event(
    target: &ResolvedTarget,
    id: &str,
    suffix: &str,
    record: &JournalRecord,
) -> Result<(), ActionError> {
    let expected = match suffix {
        "prepared" => (JournalResult::Prepared, UndoState::NotApplied),
        "applied" => (JournalResult::Applied, UndoState::Available),
        "aborted" => (JournalResult::Aborted, UndoState::NotApplied),
        "conflict" | "undo-conflict" => (JournalResult::Conflict, UndoState::BlockedExternalEdit),
        "undone" => (JournalResult::Applied, UndoState::Undone),
        _ => return Err(ActionError::InvalidJournal),
    };
    if record.version != JOURNAL_VERSION
        || record.action_version != ACTION_VERSION
        || record.action_id != id
        || !valid_id(id)
        || record.redacted_target != target.redacted_identity
        || record.source_revision.is_empty()
        || record.source_revision.len() > 128
        || record.source_revision.chars().any(char::is_control)
        || !canonical_hash(&record.before_sha256)
        || !canonical_hash(&record.after_sha256)
        || (record.result, record.undo_state) != expected
    {
        return Err(ActionError::InvalidJournal);
    }
    Ok(())
}

fn read_journal_events(
    target: &ResolvedTarget,
    id: &str,
) -> Result<BTreeMap<String, JournalRecord>, ActionError> {
    if !valid_id(id) {
        return Err(ActionError::InvalidJournal);
    }
    let mut events = BTreeMap::new();
    match fs::symlink_metadata(&target.journal) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(events),
        Err(_) => return Err(ActionError::Io),
        Ok(_) => {}
    }
    #[cfg(windows)]
    let _journal = open_existing_private_journal(target)?.ok_or(ActionError::Io)?;
    for suffix in JOURNAL_SUFFIXES {
        if let Some(record) = read_event(target, id, suffix)? {
            events.insert(suffix.to_owned(), record);
        }
    }
    Ok(events)
}

fn validate_journal(events: &BTreeMap<String, JournalRecord>) -> Result<(), ActionError> {
    let prepared = events.get("prepared").ok_or(ActionError::InvalidJournal)?;
    if events.values().any(|record| {
        record.version != prepared.version
            || record.action_version != prepared.action_version
            || record.action_id != prepared.action_id
            || record.redacted_target != prepared.redacted_target
            || record.source_revision != prepared.source_revision
            || record.before_sha256 != prepared.before_sha256
            || record.after_sha256 != prepared.after_sha256
    }) {
        return Err(ActionError::InvalidJournal);
    }
    let applied = events.contains_key("applied");
    // A refused undo can later succeed once exact after-bytes are restored;
    // keep that audit event alongside undone, preserving the existing retry.
    if (events.contains_key("aborted") && events.contains_key("conflict"))
        || (applied && (events.contains_key("aborted") || events.contains_key("conflict")))
        || (!applied && (events.contains_key("undo-conflict") || events.contains_key("undone")))
    {
        return Err(ActionError::InvalidJournal);
    }
    Ok(())
}

fn read_validated_journal(
    target: &ResolvedTarget,
    id: &str,
) -> Result<BTreeMap<String, JournalRecord>, ActionError> {
    let events = read_journal_events(target, id)?;
    validate_journal(&events)?;
    Ok(events)
}

fn write_event(
    target: &ResolvedTarget,
    id: &str,
    suffix: &str,
    record: &JournalRecord,
) -> Result<(), ActionError> {
    validate_event(target, id, suffix, record)?;
    let mut events = read_journal_events(target, id)?;
    if !events.is_empty() {
        validate_journal(&events)?;
    }
    if events.contains_key(suffix) {
        return Err(ActionError::Io);
    }
    events.insert(suffix.to_owned(), record.clone());
    validate_journal(&events)?;
    let bytes = serde_json::to_vec(record).map_err(|_| ActionError::InvalidJournal)?;
    if bytes.len() as u64 > MAX_JOURNAL_BYTES {
        return Err(ActionError::InvalidJournal);
    }
    let path = journal_path(target, id, suffix)?;
    #[cfg(windows)]
    let journal = open_private_journal(target)?;
    #[cfg(not(windows))]
    ensure_journal_dir(target)?;
    #[cfg(windows)]
    let mut staged = journal.create_event_stage()?;
    #[cfg(not(windows))]
    let mut staged = tempfile::NamedTempFile::new_in(&target.journal)?;
    staged.write_all(&bytes)?;
    staged.as_file().sync_all()?;
    #[cfg(windows)]
    journal.verify_binding()?;
    staged
        .persist_noclobber(&path)
        .map_err(|_| ActionError::Io)?;
    #[cfg(windows)]
    {
        journal.verify_binding()?;
        let file = journal.open_file(&path)?;
        if file.metadata()?.len() > MAX_JOURNAL_BYTES {
            return Err(ActionError::InvalidJournal);
        }
        journal.verify_binding()?;
    }
    #[cfg(unix)]
    File::open(&target.journal)?.sync_all()?;
    Ok(())
}

fn read_event(
    target: &ResolvedTarget,
    id: &str,
    suffix: &str,
) -> Result<Option<JournalRecord>, ActionError> {
    let path = journal_path(target, id, suffix)?;
    #[cfg(windows)]
    let Some(journal) = open_existing_private_journal(target)?
    else {
        return Ok(None);
    };
    #[cfg(not(windows))]
    ensure_journal_dir(target)?;
    #[cfg(not(windows))]
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(ActionError::Io),
    };
    #[cfg(not(windows))]
    if !metadata.file_type().is_file() {
        return Err(ActionError::InvalidJournal);
    }
    #[cfg(windows)]
    let bytes = {
        let Some(file) = journal.open_file_optional(&path)? else {
            return Ok(None);
        };
        if file.metadata()?.len() > MAX_JOURNAL_BYTES {
            return Err(ActionError::InvalidJournal);
        }
        let mut bytes = Vec::new();
        file.take(MAX_JOURNAL_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_JOURNAL_BYTES {
            return Err(ActionError::InvalidJournal);
        }
        journal.verify_binding()?;
        bytes
    };
    #[cfg(not(windows))]
    let bytes = crate::harness_integration::read_bounded_regular_file(&path, MAX_JOURNAL_BYTES)
        .map_err(|_| ActionError::InvalidJournal)?;
    let record: JournalRecord =
        serde_json::from_slice(&bytes).map_err(|_| ActionError::InvalidJournal)?;
    validate_event(target, id, suffix, &record)?;
    Ok(Some(record))
}

fn read_backup(target: &ResolvedTarget, record: &JournalRecord) -> Result<Vec<u8>, ActionError> {
    let path = backup_path(target, &record.action_id)?;
    #[cfg(windows)]
    let journal = open_existing_private_journal(target)?.ok_or(ActionError::InvalidJournal)?;
    #[cfg(not(windows))]
    ensure_journal_dir(target)?;
    #[cfg(windows)]
    let bytes = {
        let file = journal.open_file(&path)?;
        if file.metadata()?.len() > MAX_TARGET_BYTES {
            return Err(ActionError::InvalidJournal);
        }
        let mut bytes = Vec::new();
        file.take(MAX_TARGET_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_TARGET_BYTES {
            return Err(ActionError::InvalidJournal);
        }
        journal.verify_binding()?;
        bytes
    };
    #[cfg(not(windows))]
    let bytes = {
        let metadata = fs::symlink_metadata(&path)?;
        if !metadata.file_type().is_file() || metadata.len() > MAX_TARGET_BYTES {
            return Err(ActionError::InvalidJournal);
        }
        crate::harness_integration::read_bounded_regular_file(&path, MAX_TARGET_BYTES)
            .map_err(|_| ActionError::InvalidJournal)?
    };
    if bytes.len() as u64 > MAX_TARGET_BYTES || sha256(&bytes) != record.before_sha256 {
        return Err(ActionError::InvalidJournal);
    }
    Ok(bytes)
}

fn atomic_replace(target: &ResolvedTarget, bytes: &[u8]) -> Result<(), ActionError> {
    if bytes.len() as u64 > MAX_TARGET_BYTES {
        return Err(ActionError::UnsafeTarget);
    }
    checked_target(target)?;
    let parent = target.file.parent().ok_or(ActionError::UnsafeTarget)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged.persist(&target.file).map_err(|_| ActionError::Io)?;
    Ok(())
}

/// This is the only production-facing gate. No caller can construct a
/// provider target yet; even a future internal caller gets `Disabled`.
fn apply(
    target: &ResolvedTarget,
    source_revision: &str,
    expected_before_sha256: &str,
    replacement: &[u8],
) -> Result<String, ActionError> {
    if !PRODUCTION_WRITES_ENABLED {
        return Err(ActionError::Disabled);
    }
    apply_transaction(
        target,
        source_revision,
        expected_before_sha256,
        replacement,
        InterruptAt::Never,
    )
}

fn undo(target: &ResolvedTarget, id: &str) -> Result<(), ActionError> {
    if !PRODUCTION_WRITES_ENABLED {
        return Err(ActionError::Disabled);
    }
    undo_transaction(target, id)
}

struct BatchItem<'a> {
    target: &'a ResolvedTarget,
    source_revision: &'a str,
    expected_before_sha256: &'a str,
    replacement: &'a [u8],
}

/// Internal all-or-nothing attempt. A rollback conflict is returned without
/// overwriting the external edit; the immutable journal remains for review.
fn apply_batch_transaction(items: &[BatchItem<'_>]) -> Result<Vec<String>, ActionError> {
    apply_batch_using(items, |item, id| {
        apply_transaction_with_id(
            item.target,
            item.source_revision,
            item.expected_before_sha256,
            item.replacement,
            InterruptAt::Never,
            id,
        )
        .map(|_| ())
    })
}

fn apply_batch_using(
    items: &[BatchItem<'_>],
    mut apply_item: impl FnMut(&BatchItem<'_>, &str) -> Result<(), ActionError>,
) -> Result<Vec<String>, ActionError> {
    if items.is_empty() || items.len() > 16 {
        return Err(ActionError::UnsafeTarget);
    }
    let mut applied: Vec<(&ResolvedTarget, String)> = Vec::new();
    for item in items {
        let id = action_id();
        match apply_item(item, &id) {
            Ok(()) => applied.push((item.target, id)),
            Err(error) => {
                // Failure can happen after replacement but before the applied
                // event is published. Reconcile this attempt before unwinding
                // earlier items, rather than assuming Err means no write.
                let mut rollback_error = rollback_failed_attempt(item.target, &id).err();
                for (target, id) in applied.iter().rev() {
                    if let Err(error) = undo_transaction(target, id) {
                        // A conflict on one target must not prevent rollback
                        // of the remaining independent targets.
                        rollback_error.get_or_insert(error);
                    }
                }
                return Err(rollback_error.unwrap_or(error));
            }
        }
    }
    Ok(applied.into_iter().map(|(_, id)| id).collect())
}

fn rollback_failed_attempt(target: &ResolvedTarget, id: &str) -> Result<(), ActionError> {
    #[cfg(windows)]
    let _journal = open_existing_private_journal(target)?;
    // A preparation-free failure has no markers. Orphan later events are
    // an invalid transaction, not evidence that the attempt wrote nothing.
    let events = read_journal_events(target, id)?;
    if events.is_empty() {
        return Ok(());
    }
    validate_journal(&events)?;
    match recover_transaction(target, id)? {
        Recovery::Applied => undo_transaction(target, id),
        Recovery::Aborted | Recovery::Undone => Ok(()),
    }
}

/// Core algorithm exercised only by synthetic, allowlisted test fixtures.
/// It is not reachable through a production action or Tauri command.
fn apply_transaction(
    target: &ResolvedTarget,
    source_revision: &str,
    expected_before_sha256: &str,
    replacement: &[u8],
    interrupt_at: InterruptAt,
) -> Result<String, ActionError> {
    apply_transaction_with_id(
        target,
        source_revision,
        expected_before_sha256,
        replacement,
        interrupt_at,
        &action_id(),
    )
}

fn apply_transaction_with_id(
    target: &ResolvedTarget,
    source_revision: &str,
    expected_before_sha256: &str,
    replacement: &[u8],
    interrupt_at: InterruptAt,
    id: &str,
) -> Result<String, ActionError> {
    if !valid_id(id) {
        return Err(ActionError::InvalidJournal);
    }
    if source_revision.is_empty()
        || source_revision.len() > 128
        || source_revision.chars().any(char::is_control)
        || replacement.len() as u64 > MAX_TARGET_BYTES
        || !canonical_hash(expected_before_sha256)
    {
        return Err(ActionError::UnsafeTarget);
    }
    let before = read_target(target)?;
    if sha256(&before) != expected_before_sha256 {
        return Err(ActionError::Conflict);
    }
    let id = id.to_owned();
    #[cfg(windows)]
    let journal = open_private_journal(target)?;
    #[cfg(not(windows))]
    ensure_journal_dir(target)?;
    // Refuse an existing transaction before creating a backup or any event.
    let existing = read_journal_events(target, &id)?;
    if !existing.is_empty() {
        validate_journal(&existing)?;
        return Err(ActionError::Conflict);
    }
    #[cfg(windows)]
    let mut backup = journal.create_file(&backup_path(target, &id)?)?;
    #[cfg(not(windows))]
    let mut backup = private_new_file(&backup_path(target, &id)?)?;
    backup.write_all(&before)?;
    backup.sync_all()?;
    #[cfg(windows)]
    journal.verify_binding()?;
    let record = JournalRecord {
        version: JOURNAL_VERSION,
        action_id: id.clone(),
        action_version: ACTION_VERSION,
        redacted_target: target.redacted_identity.clone(),
        source_revision: source_revision.into(),
        before_sha256: expected_before_sha256.into(),
        after_sha256: sha256(replacement),
        result: JournalResult::Prepared,
        undo_state: UndoState::NotApplied,
    };
    write_event(target, &id, "prepared", &record)?;
    if interrupt_at == InterruptAt::AfterPrepared {
        return Err(ActionError::Interrupted);
    }
    // Immediate revalidation catches an edit before replacement. A provider
    // adapter would still need a reviewed lock/CAS strategy for the final race.
    if sha256(&read_target(target)?) != record.before_sha256 {
        write_event(
            target,
            &id,
            "conflict",
            &JournalRecord {
                result: JournalResult::Conflict,
                undo_state: UndoState::BlockedExternalEdit,
                ..record
            },
        )?;
        return Err(ActionError::Conflict);
    }
    atomic_replace(target, replacement)?;
    if interrupt_at == InterruptAt::AfterTargetReplace {
        return Err(ActionError::Interrupted);
    }
    if sha256(&read_target(target)?) != record.after_sha256 {
        return Err(ActionError::Conflict);
    }
    write_event(
        target,
        &id,
        "applied",
        &JournalRecord {
            result: JournalResult::Applied,
            undo_state: UndoState::Available,
            ..record
        },
    )?;
    Ok(id)
}

fn undo_transaction(target: &ResolvedTarget, id: &str) -> Result<(), ActionError> {
    #[cfg(windows)]
    let _journal = open_existing_private_journal(target)?;
    let journal = read_validated_journal(target, id)?;
    let prepared = journal["prepared"].clone();
    if journal.contains_key("undone") {
        return Err(ActionError::Conflict);
    }
    if !journal.contains_key("applied") {
        return Err(ActionError::InvalidJournal);
    }
    let backup = read_backup(target, &prepared)?;
    if sha256(&read_target(target)?) != prepared.after_sha256 {
        write_event(
            target,
            id,
            "undo-conflict",
            &JournalRecord {
                result: JournalResult::Conflict,
                undo_state: UndoState::BlockedExternalEdit,
                ..prepared
            },
        )?;
        return Err(ActionError::Conflict);
    }
    atomic_replace(target, &backup)?;
    if sha256(&read_target(target)?) != prepared.before_sha256 {
        return Err(ActionError::Conflict);
    }
    write_event(
        target,
        id,
        "undone",
        &JournalRecord {
            result: JournalResult::Applied,
            undo_state: UndoState::Undone,
            ..prepared
        },
    )?;
    Ok(())
}

fn recover_transaction(target: &ResolvedTarget, id: &str) -> Result<Recovery, ActionError> {
    #[cfg(windows)]
    let _journal = open_existing_private_journal(target)?;
    let journal = read_validated_journal(target, id)?;
    let prepared = journal["prepared"].clone();
    read_backup(target, &prepared)?;
    if journal.contains_key("undone") {
        return Ok(Recovery::Undone);
    }
    if journal.contains_key("conflict") || journal.contains_key("undo-conflict") {
        return Err(ActionError::Conflict);
    }
    if journal.contains_key("aborted") {
        return Ok(Recovery::Aborted);
    }
    let applied = journal.contains_key("applied");
    let current = sha256(&read_target(target)?);
    if current == prepared.before_sha256 {
        if applied {
            write_event(
                target,
                id,
                "undone",
                &JournalRecord {
                    result: JournalResult::Applied,
                    undo_state: UndoState::Undone,
                    ..prepared
                },
            )?;
            Ok(Recovery::Undone)
        } else {
            write_event(
                target,
                id,
                "aborted",
                &JournalRecord {
                    result: JournalResult::Aborted,
                    undo_state: UndoState::NotApplied,
                    ..prepared
                },
            )?;
            Ok(Recovery::Aborted)
        }
    } else if current == prepared.after_sha256 {
        if !applied {
            write_event(
                target,
                id,
                "applied",
                &JournalRecord {
                    result: JournalResult::Applied,
                    undo_state: UndoState::Available,
                    ..prepared
                },
            )?;
        }
        Ok(Recovery::Applied)
    } else {
        Err(ActionError::Conflict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fixture(dir: &TempDir, file: &str, bytes: &[u8]) -> ResolvedTarget {
        assert!(matches!(file, "guard.json" | "workflow.json"));
        let target = ResolvedTarget {
            root: dir.path().to_path_buf(),
            file: dir.path().join(file),
            journal: dir.path().join("private-action-journal"),
            redacted_identity: format!(
                "target:{:016x}",
                crate::stable_hash::fnv1a64(file.as_bytes())
            ),
        };
        fs::write(&target.file, bytes).unwrap();
        target
    }

    fn write_new_journal_fixture(target: &ResolvedTarget, path: &std::path::Path, bytes: &[u8]) {
        #[cfg(windows)]
        {
            let journal = open_private_journal(target).unwrap();
            let mut file = journal.create_file(path).unwrap();
            file.write_all(bytes).unwrap();
            file.sync_all().unwrap();
        }
        #[cfg(not(windows))]
        {
            let _ = target;
            fs::write(path, bytes).unwrap();
        }
    }

    fn bytes_on_disk(target: &ResolvedTarget) -> (Vec<u8>, BTreeMap<std::ffi::OsString, Vec<u8>>) {
        let entries = if target.journal.exists() {
            fs::read_dir(&target.journal)
                .unwrap()
                .map(|entry| {
                    let entry = entry.unwrap();
                    (entry.file_name(), fs::read(entry.path()).unwrap())
                })
                .collect()
        } else {
            BTreeMap::new()
        };
        (fs::read(&target.file).unwrap(), entries)
    }

    #[test]
    fn malformed_action_inputs_and_unprepared_markers_create_no_journal() {
        let oversized_revision = "r".repeat(129);
        for (revision, expected) in [
            ("", sha256(b"before")),
            (oversized_revision.as_str(), sha256(b"before")),
            ("bad\nrevision", sha256(b"before")),
            ("revision", "A".repeat(64)),
            ("revision", "0".repeat(63)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let target = fixture(&dir, "guard.json", b"before");
            assert_eq!(
                apply_transaction(&target, revision, &expected, b"after", InterruptAt::Never),
                Err(ActionError::UnsafeTarget)
            );
            assert!(!target.journal.exists());
            assert_eq!(fs::read(&target.file).unwrap(), b"before");
        }
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "guard.json", b"before");
        let id = action_id();
        let record = JournalRecord {
            version: JOURNAL_VERSION,
            action_id: id.clone(),
            action_version: ACTION_VERSION,
            redacted_target: target.redacted_identity.clone(),
            source_revision: "revision".into(),
            before_sha256: sha256(b"before"),
            after_sha256: sha256(b"after"),
            result: JournalResult::Applied,
            undo_state: UndoState::Available,
        };
        assert_eq!(
            write_event(&target, &id, "applied", &record),
            Err(ActionError::InvalidJournal)
        );
        assert!(!target.journal.exists());
        assert_eq!(fs::read(&target.file).unwrap(), b"before");
        assert_eq!(rollback_failed_attempt(&target, &id), Ok(()));
        assert!(!target.journal.exists());
    }

    #[test]
    fn tampered_events_cannot_recover_undo_or_rollback_and_leave_every_byte_unchanged() {
        let changes = [
            ("version", serde_json::json!(2)),
            ("action_version", serde_json::json!(2)),
            (
                "source_revision",
                serde_json::json!("another-valid-revision"),
            ),
            ("source_revision", serde_json::json!("")),
            ("source_revision", serde_json::json!("r".repeat(129))),
            ("source_revision", serde_json::json!("control\nrevision")),
            ("before_sha256", serde_json::json!("0".repeat(64))),
            ("after_sha256", serde_json::json!("0".repeat(64))),
            ("after_sha256", serde_json::json!("A".repeat(64))),
            ("before_sha256", serde_json::json!("short")),
            ("result", serde_json::json!("aborted")),
            ("undo_state", serde_json::json!("undone")),
        ];
        for (field, value) in changes {
            let dir = tempfile::tempdir().unwrap();
            let target = fixture(&dir, "guard.json", b"before");
            let id = apply_transaction(
                &target,
                "revision",
                &sha256(b"before"),
                b"after",
                InterruptAt::Never,
            )
            .unwrap();
            let path = journal_path(&target, &id, "applied").unwrap();
            let mut record: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            record[field] = value;
            fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
            let before = bytes_on_disk(&target);
            assert_eq!(
                recover_transaction(&target, &id),
                Err(ActionError::InvalidJournal),
                "{field}"
            );
            assert_eq!(
                undo_transaction(&target, &id),
                Err(ActionError::InvalidJournal),
                "{field}"
            );
            assert_eq!(
                rollback_failed_attempt(&target, &id),
                Err(ActionError::InvalidJournal),
                "{field}"
            );
            assert_eq!(bytes_on_disk(&target), before, "{field}");
        }
    }

    #[test]
    fn contradictory_terminal_chains_are_rejected_before_any_early_return_or_write() {
        for suffixes in [
            vec!["aborted"],
            vec!["conflict"],
            vec!["undone", "conflict"],
            vec!["undone", "aborted"],
        ] {
            let dir = tempfile::tempdir().unwrap();
            let target = fixture(&dir, "guard.json", b"before");
            let id = apply_transaction(
                &target,
                "revision",
                &sha256(b"before"),
                b"after",
                InterruptAt::Never,
            )
            .unwrap();
            let prepared = read_event(&target, &id, "prepared").unwrap().unwrap();
            for suffix in suffixes {
                let (result, undo_state) = match suffix {
                    "aborted" => (JournalResult::Aborted, UndoState::NotApplied),
                    "undone" => (JournalResult::Applied, UndoState::Undone),
                    _ => (JournalResult::Conflict, UndoState::BlockedExternalEdit),
                };
                let record = JournalRecord {
                    result,
                    undo_state,
                    ..prepared.clone()
                };
                write_new_journal_fixture(
                    &target,
                    &journal_path(&target, &id, suffix).unwrap(),
                    &serde_json::to_vec(&record).unwrap(),
                );
            }
            let before = bytes_on_disk(&target);
            assert_eq!(
                recover_transaction(&target, &id),
                Err(ActionError::InvalidJournal)
            );
            assert_eq!(
                undo_transaction(&target, &id),
                Err(ActionError::InvalidJournal)
            );
            assert_eq!(
                rollback_failed_attempt(&target, &id),
                Err(ActionError::InvalidJournal)
            );
            assert_eq!(bytes_on_disk(&target), before);
        }
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "guard.json", b"before");
        let id = apply_transaction(
            &target,
            "revision",
            &sha256(b"before"),
            b"after",
            InterruptAt::Never,
        )
        .unwrap();
        fs::remove_file(journal_path(&target, &id, "applied").unwrap()).unwrap();
        let prepared = read_event(&target, &id, "prepared").unwrap().unwrap();
        let undone = JournalRecord {
            result: JournalResult::Applied,
            undo_state: UndoState::Undone,
            ..prepared
        };
        write_new_journal_fixture(
            &target,
            &journal_path(&target, &id, "undone").unwrap(),
            &serde_json::to_vec(&undone).unwrap(),
        );
        let before = bytes_on_disk(&target);
        assert_eq!(
            recover_transaction(&target, &id),
            Err(ActionError::InvalidJournal)
        );
        assert_eq!(
            undo_transaction(&target, &id),
            Err(ActionError::InvalidJournal)
        );
        assert_eq!(bytes_on_disk(&target), before);
    }

    #[test]
    fn an_orphan_event_refuses_new_preparation_before_creating_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "guard.json", b"before");
        #[cfg(windows)]
        let _journal = open_private_journal(&target).unwrap();
        #[cfg(not(windows))]
        ensure_journal_dir(&target).unwrap();
        let id = action_id();
        let record = JournalRecord {
            version: JOURNAL_VERSION,
            action_version: ACTION_VERSION,
            action_id: id.clone(),
            redacted_target: target.redacted_identity.clone(),
            source_revision: "revision".into(),
            before_sha256: sha256(b"before"),
            after_sha256: sha256(b"after"),
            result: JournalResult::Applied,
            undo_state: UndoState::Available,
        };
        write_new_journal_fixture(
            &target,
            &journal_path(&target, &id, "applied").unwrap(),
            &serde_json::to_vec(&record).unwrap(),
        );
        let before = bytes_on_disk(&target);
        assert_eq!(
            recover_transaction(&target, &id),
            Err(ActionError::InvalidJournal)
        );
        assert_eq!(
            undo_transaction(&target, &id),
            Err(ActionError::InvalidJournal)
        );
        assert_eq!(
            rollback_failed_attempt(&target, &id),
            Err(ActionError::InvalidJournal)
        );
        assert_eq!(
            apply_transaction_with_id(
                &target,
                "revision",
                &sha256(b"before"),
                b"after",
                InterruptAt::Never,
                &id
            ),
            Err(ActionError::InvalidJournal)
        );
        assert_eq!(bytes_on_disk(&target), before);
        assert!(!backup_path(&target, &id).unwrap().exists());
    }

    #[test]
    fn production_gate_refuses_apply_and_undo_without_a_journal() {
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "guard.json", b"original");
        assert_eq!(
            apply(&target, "revision-1", &sha256(b"original"), b"replacement"),
            Err(ActionError::Disabled)
        );
        assert_eq!(undo(&target, "action"), Err(ActionError::Disabled));
        assert_eq!(fs::read(&target.file).unwrap(), b"original");
        assert!(!target.journal.exists());
    }

    #[test]
    fn exact_backup_journal_and_undo_refuse_external_edits() {
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "guard.json", b"original");
        let id = apply_transaction(
            &target,
            "revision-1",
            &sha256(b"original"),
            b"replacement",
            InterruptAt::Never,
        )
        .unwrap();
        assert_eq!(fs::read(&target.file).unwrap(), b"replacement");
        let prepared = read_event(&target, &id, "prepared").unwrap().unwrap();
        let applied = read_event(&target, &id, "applied").unwrap().unwrap();
        assert_eq!(prepared.before_sha256, sha256(b"original"));
        assert_eq!(applied.after_sha256, sha256(b"replacement"));
        assert_eq!(applied.undo_state, UndoState::Available);
        assert_eq!(read_backup(&target, &prepared).unwrap(), b"original");
        let journal_text =
            fs::read_to_string(journal_path(&target, &id, "prepared").unwrap()).unwrap();
        assert!(!journal_text.contains(dir.path().to_str().unwrap()));
        fs::write(&target.file, b"external-edit").unwrap();
        assert_eq!(undo_transaction(&target, &id), Err(ActionError::Conflict));
        assert_eq!(fs::read(&target.file).unwrap(), b"external-edit");
        fs::write(&target.file, b"replacement").unwrap();
        undo_transaction(&target, &id).unwrap();
        assert_eq!(fs::read(&target.file).unwrap(), b"original");
        assert_eq!(recover_transaction(&target, &id).unwrap(), Recovery::Undone);
    }

    #[test]
    fn interrupted_prepare_and_replace_recover_without_guessing() {
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "workflow.json", b"before");
        assert_eq!(
            apply_transaction(
                &target,
                "revision-1",
                &sha256(b"before"),
                b"after",
                InterruptAt::AfterPrepared
            ),
            Err(ActionError::Interrupted)
        );
        let first = fs::read_dir(&target.journal)
            .unwrap()
            .filter_map(Result::ok)
            .find_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".prepared.json"))
                    .map(str::to_owned)
            })
            .unwrap();
        assert_eq!(
            recover_transaction(&target, &first).unwrap(),
            Recovery::Aborted
        );
        assert_eq!(
            recover_transaction(&target, &first).unwrap(),
            Recovery::Aborted
        );
        assert_eq!(fs::read(&target.file).unwrap(), b"before");
        assert_eq!(
            apply_transaction(
                &target,
                "revision-2",
                &sha256(b"before"),
                b"after",
                InterruptAt::AfterTargetReplace
            ),
            Err(ActionError::Interrupted)
        );
        let second = fs::read_dir(&target.journal)
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".prepared.json"))
                    .map(str::to_owned)
            })
            .find(|id| id != &first)
            .unwrap();
        assert_eq!(
            recover_transaction(&target, &second).unwrap(),
            Recovery::Applied
        );
        undo_transaction(&target, &second).unwrap();
        assert_eq!(fs::read(&target.file).unwrap(), b"before");
    }

    #[test]
    fn partial_unpublished_event_does_not_block_recovery_and_ids_cannot_escape_journal() {
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "guard.json", b"before");
        assert_eq!(
            undo_transaction(&target, "../escape"),
            Err(ActionError::InvalidJournal)
        );
        assert!(!target.journal.exists());
        assert_eq!(
            apply_transaction(
                &target,
                "revision-1",
                &sha256(b"before"),
                b"after",
                InterruptAt::AfterTargetReplace,
            ),
            Err(ActionError::Interrupted)
        );
        let id = fs::read_dir(&target.journal)
            .unwrap()
            .filter_map(Result::ok)
            .find_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".prepared.json"))
                    .map(str::to_owned)
            })
            .unwrap();
        fs::write(
            target.journal.join(".incomplete-event.tmp"),
            b"{\"result\":",
        )
        .unwrap();
        assert_eq!(
            recover_transaction(&target, &id).unwrap(),
            Recovery::Applied
        );
        assert!(read_event(&target, &id, "applied").unwrap().is_some());
        assert_eq!(fs::read(&target.file).unwrap(), b"after");
    }

    #[test]
    fn stale_second_batch_item_rolls_back_first_without_touching_external_edit() {
        let dir = tempfile::tempdir().unwrap();
        let first = fixture(&dir, "guard.json", b"first-before");
        let second = fixture(&dir, "workflow.json", b"second-before");
        fs::write(&second.file, b"external-edit").unwrap();
        let first_hash = sha256(b"first-before");
        let second_hash = sha256(b"second-before");
        let second_result = apply_batch_transaction(&[
            BatchItem {
                target: &first,
                source_revision: "revision-1",
                expected_before_sha256: &first_hash,
                replacement: b"first-after",
            },
            BatchItem {
                target: &second,
                source_revision: "revision-1",
                expected_before_sha256: &second_hash,
                replacement: b"second-after",
            },
        ]);
        assert_eq!(second_result, Err(ActionError::Conflict));
        assert_eq!(fs::read(&first.file).unwrap(), b"first-before");
        assert_eq!(fs::read(&second.file).unwrap(), b"external-edit");
        let id = fs::read_dir(&first.journal)
            .unwrap()
            .filter_map(Result::ok)
            .find_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".undone.json"))
                    .map(str::to_owned)
            })
            .unwrap();
        assert_eq!(
            read_event(&first, &id, "undone")
                .unwrap()
                .unwrap()
                .undo_state,
            UndoState::Undone
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_target_is_rejected_before_backup_or_journal() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "guard.json", b"original");
        fs::remove_file(&target.file).unwrap();
        let outside = dir.path().join("outside");
        fs::write(&outside, b"outside").unwrap();
        symlink(&outside, &target.file).unwrap();
        assert_eq!(
            apply_transaction(
                &target,
                "revision-1",
                &sha256(b"outside"),
                b"new",
                InterruptAt::Never
            ),
            Err(ActionError::UnsafeTarget)
        );
        assert!(!target.journal.exists());
        assert_eq!(fs::read(outside).unwrap(), b"outside");
    }

    #[test]
    fn batch_recovers_the_failing_item_after_replacement_before_rolling_back_prior_items() {
        let dir = tempfile::tempdir().unwrap();
        let first = fixture(&dir, "guard.json", b"first-before");
        let second = fixture(&dir, "workflow.json", b"second-before");
        let first_hash = sha256(b"first-before");
        let second_hash = sha256(b"second-before");
        let result = apply_batch_using(
            &[
                BatchItem {
                    target: &first,
                    source_revision: "revision-1",
                    expected_before_sha256: &first_hash,
                    replacement: b"first-after",
                },
                BatchItem {
                    target: &second,
                    source_revision: "revision-1",
                    expected_before_sha256: &second_hash,
                    replacement: b"second-after",
                },
            ],
            |item, id| {
                apply_transaction_with_id(
                    item.target,
                    item.source_revision,
                    item.expected_before_sha256,
                    item.replacement,
                    if item.target.file == second.file {
                        InterruptAt::AfterTargetReplace
                    } else {
                        InterruptAt::Never
                    },
                    id,
                )
                .map(|_| ())
            },
        );
        assert_eq!(result, Err(ActionError::Interrupted));
        assert_eq!(fs::read(&first.file).unwrap(), b"first-before");
        assert_eq!(fs::read(&second.file).unwrap(), b"second-before");
    }

    #[test]
    fn rollback_conflict_does_not_skip_other_independent_targets() {
        let first_dir = tempfile::tempdir().unwrap();
        let second_dir = tempfile::tempdir().unwrap();
        let third_dir = tempfile::tempdir().unwrap();
        let first = fixture(&first_dir, "guard.json", b"first-before");
        let second = fixture(&second_dir, "guard.json", b"second-before");
        let third = fixture(&third_dir, "guard.json", b"third-before");
        let first_hash = sha256(b"first-before");
        let second_hash = sha256(b"second-before");
        let third_hash = sha256(b"third-before");
        let result = apply_batch_using(
            &[
                BatchItem {
                    target: &first,
                    source_revision: "revision-1",
                    expected_before_sha256: &first_hash,
                    replacement: b"first-after",
                },
                BatchItem {
                    target: &second,
                    source_revision: "revision-1",
                    expected_before_sha256: &second_hash,
                    replacement: b"second-after",
                },
                BatchItem {
                    target: &third,
                    source_revision: "revision-1",
                    expected_before_sha256: &third_hash,
                    replacement: b"third-after",
                },
            ],
            |item, id| {
                if item.target.file == third.file {
                    fs::write(&second.file, b"external-edit").unwrap();
                    return Err(ActionError::Io);
                }
                apply_transaction_with_id(
                    item.target,
                    item.source_revision,
                    item.expected_before_sha256,
                    item.replacement,
                    InterruptAt::Never,
                    id,
                )
                .map(|_| ())
            },
        );
        assert_eq!(result, Err(ActionError::Conflict));
        assert_eq!(fs::read(&first.file).unwrap(), b"first-before");
        assert_eq!(fs::read(&second.file).unwrap(), b"external-edit");
        assert_eq!(fs::read(&third.file).unwrap(), b"third-before");
    }

    #[cfg(unix)]
    #[test]
    fn journal_directory_requires_private_owner_and_no_symlink() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = tempfile::tempdir().unwrap();
        let target = fixture(&dir, "guard.json", b"original");
        fs::create_dir(&target.journal).unwrap();
        fs::set_permissions(&target.journal, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(ensure_journal_dir(&target), Err(ActionError::UnsafeTarget));
        fs::set_permissions(&target.journal, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(ensure_journal_dir(&target), Ok(()));
        fs::remove_dir(&target.journal).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        symlink(&elsewhere, &target.journal).unwrap();
        assert_eq!(ensure_journal_dir(&target), Err(ActionError::UnsafeTarget));
    }
}
