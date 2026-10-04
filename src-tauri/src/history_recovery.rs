//! Explicit recovery preserves the original database and its SQLite sidecars.
//! A durable marker makes the replacement's incomplete coverage survive a crash
//! between the preservation step and creating or populating the new ledger.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryReceipt {
    pub backup_directory: PathBuf,
    pub recovered_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryFailureKind {
    Corrupt,
    NewerSchema,
    Unavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryFailure {
    pub kind: HistoryFailureKind,
    pub message: String,
}

impl HistoryFailure {
    pub fn from_error(error: &anyhow::Error) -> Self {
        let corrupt = error.chain().any(|cause| cause.downcast_ref::<rusqlite::Error>().is_some_and(|error| matches!(error,
            rusqlite::Error::SqliteFailure(code, _) if matches!(code.code, rusqlite::ErrorCode::DatabaseCorrupt | rusqlite::ErrorCode::NotADatabase))));
        if corrupt {
            Self { kind: HistoryFailureKind::Corrupt, message: "The history database could not be read. Live source transcripts remain accessible.".into() }
        } else if error.to_string().contains("newer than") {
            Self { kind: HistoryFailureKind::NewerSchema, message: "This history was written by a newer Odometer. Update Odometer to read it, or preserve it and rebuild readable sources separately.".into() }
        } else {
            Self { kind: HistoryFailureKind::Unavailable, message: "Durable history could not be prepared. Check local disk permissions and free space, then retry. Live source transcripts remain accessible.".into() }
        }
    }
}

fn marker_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".recovery.json");
    PathBuf::from(name)
}

pub(super) fn apply_marker(connection: &mut Connection, path: &Path) -> Result<()> {
    let marker = marker_path(path);
    let raw = match std::fs::read(marker) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let receipt: RecoveryReceipt =
        serde_json::from_slice(&raw).context("invalid history recovery marker")?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    transaction.execute("INSERT INTO history_meta(key,value) VALUES('coverage_complete','0') ON CONFLICT(key) DO UPDATE SET value='0'", [])?;
    for (key, value) in [
        ("recovered_at_ms", receipt.recovered_at_ms.to_string()),
        (
            "recovery_backup_directory",
            receipt.backup_directory.to_string_lossy().into_owned(),
        ),
    ] {
        transaction.execute("INSERT INTO history_meta(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;
    }
    transaction.commit()?;
    Ok(())
}

impl HistoryStore {
    /// Only call after the app has marked the archive unavailable and excluded
    /// internal writers. No provider transcript or existing backup is deleted.
    pub fn recover_unavailable(path: &Path) -> Result<(Self, RecoveryReceipt)> {
        if !path.is_file() {
            bail!("there is no history database to preserve; retry opening it instead");
        }
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("history database has no parent directory"))?;
        let name = path
            .file_name()
            .ok_or_else(|| anyhow!("history database has no filename"))?;
        let now = now_ms();
        let mut backup = None;
        for suffix in 0..100 {
            let candidate = parent.join(format!("history-recovery-{now}-{suffix}"));
            match std::fs::create_dir(&candidate) {
                Ok(()) => {
                    backup = Some(candidate);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let receipt = RecoveryReceipt {
            backup_directory: backup
                .ok_or_else(|| anyhow!("could not reserve a history backup directory"))?,
            recovered_at_ms: now,
        };
        let marker = marker_path(path);
        // create_new refuses overwriting an earlier recovery record. The marker
        // is durable before any source rename and deliberately remains if the
        // replacement cannot be opened, so restart never claims complete data.
        let mut marker_file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .context("an existing recovery must be inspected before another rebuild")?;
        use std::io::Write;
        marker_file.write_all(&serde_json::to_vec(&receipt)?)?;
        marker_file.sync_all()?;
        drop(marker_file);
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        for suffix in ["", "-wal", "-shm"] {
            let mut source_name = path.as_os_str().to_os_string();
            source_name.push(suffix);
            let source = PathBuf::from(source_name);
            if !source.exists() {
                continue;
            }
            let mut target_name = name.to_os_string();
            target_name.push(suffix);
            let target = receipt.backup_directory.join(target_name);
            if let Err(error) = std::fs::rename(&source, &target) {
                let mut rollback_complete = true;
                for (original, backup) in moved.iter().rev() {
                    rollback_complete &= std::fs::rename(backup, original).is_ok();
                }
                if rollback_complete {
                    let _ = std::fs::remove_file(&marker);
                }
                bail!(
                    "could not preserve all database files ({error}); preserved files remain in {}",
                    receipt.backup_directory.display()
                );
            }
            moved.push((source, target));
        }
        let store = Self::open(path)
            .context("original history was preserved, but the replacement could not be prepared")?;
        Ok((store, receipt))
    }

    pub fn recovery_receipt(&self) -> Result<Option<RecoveryReceipt>> {
        let raw = match std::fs::read(marker_path(&self.path)) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        Ok(Some(serde_json::from_slice(&raw)?))
    }
}
