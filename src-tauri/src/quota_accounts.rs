//! Consent and scheduling for the installed CLI's current account. No credentials,
//! response bodies, or readings are persisted. Transcript history stays unattributed.
use crate::quota_live::{self, DiscoveredQuotaAccount, LiveQuotaError, LiveQuotaReading};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tauri::Emitter;

const POLL_INTERVAL: Duration = Duration::from_secs(300);
const MAX_BACKOFF: Duration = Duration::from_secs(3600);
const CANDIDATE_TTL: Duration = Duration::from_secs(300);
const MAX_ACCOUNTS: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaAccountConsent {
    pub account_id: String,
    pub label: String,
    pub consented_at: DateTime<Utc>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ConsentFile {
    #[serde(default = "version")]
    version: u32,
    #[serde(default)]
    accounts: Vec<QuotaAccountConsent>,
}
fn version() -> u32 {
    1
}

#[derive(Clone, Serialize)]
pub struct LiveQuotaAccountView {
    pub provider: &'static str,
    pub consent: QuotaAccountConsent,
    pub observed_at: Option<DateTime<Utc>>,
    pub ordinary_usage_allowed: Option<bool>,
    pub buckets: Vec<LiveQuotaBucketView>,
    pub unavailable: Option<&'static str>,
}
#[derive(Clone, Serialize)]
pub struct LiveQuotaBucketView {
    pub limit_id: String,
    pub limit_name: Option<String>,
    pub spend_control_reached: Option<bool>,
    pub snapshot: crate::quota::QuotaSnapshot,
}
#[derive(Serialize)]
pub struct LiveQuotaStatus {
    pub accounts: Vec<LiveQuotaAccountView>,
    pub busy: bool,
    pub configuration_error: Option<&'static str>,
}

struct Runtime {
    loaded: bool,
    file: ConsentFile,
    durable_file: Option<ConsentFile>,
    locally_paused: bool,
    configuration_error: Option<&'static str>,
    candidate: Option<(DiscoveredQuotaAccount, Instant)>,
    generation: u64,
    busy: bool,
    next_poll: Instant,
    backoff: Duration,
    reading: Option<LiveQuotaReading>,
    error: Option<LiveQuotaError>,
}
impl Default for Runtime {
    fn default() -> Self {
        Self {
            loaded: false,
            file: ConsentFile {
                version: 1,
                accounts: vec![],
            },
            durable_file: None,
            locally_paused: false,
            configuration_error: None,
            candidate: None,
            generation: 0,
            busy: false,
            next_poll: Instant::now(),
            backoff: POLL_INTERVAL,
            reading: None,
            error: None,
        }
    }
}

#[derive(Default)]
pub struct LiveQuotaService {
    runtime: Mutex<Runtime>,
    // Private injection points isolate synthetic tests from user state and sign-in.
    settings_path: Option<PathBuf>,
    executable: Option<PathBuf>,
}

fn consent_path() -> Result<PathBuf, &'static str> {
    dirs::config_dir()
        .map(|dir| dir.join("agent-odometer").join("quota-live-v1.json"))
        .ok_or("Live quota settings location is unavailable.")
}
fn validate_file(file: &ConsentFile) -> Result<(), &'static str> {
    if file.version != 1
        || file.accounts.len() > MAX_ACCOUNTS
        || file.accounts.iter().filter(|a| a.enabled).count() > 1
    {
        return Err("Live quota settings are unsupported; existing file preserved.");
    }
    let mut seen = std::collections::HashSet::new();
    for account in &file.accounts {
        if account.account_id.is_empty()
            || account.account_id.len() > 256
            || account.account_id.chars().any(char::is_control)
            || account.label.trim().is_empty()
            || account.label.len() > 80
            || account.label.chars().any(char::is_control)
            || !seen.insert(&account.account_id)
        {
            return Err("Live quota settings are invalid; existing file preserved.");
        }
    }
    Ok(())
}
fn read_file(path: &Path) -> Result<ConsentFile, &'static str> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    regular_file_options(&mut options);
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ConsentFile {
                version: 1,
                accounts: vec![],
            })
        }
        Err(_) => return Err("Live quota settings are unreadable; polling is off."),
    };
    require_regular(&file)?;
    let mut bytes = Vec::new();
    file.take(32_769)
        .read_to_end(&mut bytes)
        .map_err(|_| "Live quota settings are unreadable; polling is off.")?;
    if bytes.len() > 32_768 {
        return Err("Live quota settings are too large; polling is off.");
    }
    let file: ConsentFile = serde_json::from_slice(&bytes)
        .map_err(|_| "Live quota settings are invalid; polling is off.")?;
    validate_file(&file)?;
    Ok(file)
}
// Reject links and special files before any read can block on them.
fn regular_file_options(options: &mut std::fs::OpenOptions) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
}
fn require_regular(file: &std::fs::File) -> Result<(), &'static str> {
    let metadata = file
        .metadata()
        .map_err(|_| "Live quota settings are unreadable; polling is off.")?;
    if !metadata.is_file() {
        return Err("Live quota settings must be a regular file; polling is off.");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
        {
            return Err("Live quota settings must not be a link; polling is off.");
        }
    }
    Ok(())
}
// Explicitly release locks even if a concurrently spawned child temporarily
// retains a duplicated descriptor. Closing only this descriptor is insufficient.
struct ConsentLock(std::fs::File);
impl Drop for ConsentLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

fn save_file(path: &Path, expected: &ConsentFile, file: &ConsentFile) -> Result<(), &'static str> {
    use std::io::Write;
    validate_file(file)?;
    let parent = path
        .parent()
        .ok_or("Live quota settings location is unavailable.")?;
    std::fs::create_dir_all(parent).map_err(|_| "Live quota settings could not be saved.")?;
    // Serialize cooperating app instances and compare the last observed consent
    // before writing, so a stale approval cannot resurrect another app's revoke.
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    regular_file_options(&mut options);
    let lock = options
        .open(path.with_extension("lock"))
        .map_err(|_| "Live quota settings could not be locked; retry the action.")?;
    require_regular(&lock)?;
    lock.try_lock()
        .map_err(|_| "Live quota settings are changing in another app; retry the action.")?;
    let _lock = ConsentLock(lock);
    if &read_file(path)? != expected {
        return Err("Live quota consent changed in another app; review it and retry.");
    }
    let bytes = serde_json::to_vec(file).map_err(|_| "Live quota settings could not be saved.")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|_| "Live quota settings could not be saved.")?;
    temporary
        .write_all(&bytes)
        .and_then(|()| temporary.as_file().sync_all())
        .map_err(|_| "Live quota settings could not be saved.")?;
    temporary
        .persist(path)
        .map_err(|_| "Live quota settings could not be saved.")?;
    Ok(())
}

impl Runtime {
    fn load(&mut self, path: Result<PathBuf, &'static str>) -> bool {
        // Check every action and before publishing a completed poll. Consent is
        // shared between app instances; a startup-only cache can outlive revoke.
        let mut changed = false;
        match path.and_then(|path| read_file(&path)) {
            Ok(file) => {
                if !self.loaded || self.durable_file.as_ref() != Some(&file) {
                    self.invalidate();
                    self.file = file.clone();
                    self.durable_file = Some(file);
                    changed = self.loaded;
                }
                changed |= self.configuration_error.take().is_some();
            }
            Err(error) => {
                if !self.loaded || self.configuration_error != Some(error) {
                    self.invalidate();
                    self.file
                        .accounts
                        .iter_mut()
                        .for_each(|account| account.enabled = false);
                    changed = true;
                }
                self.configuration_error = Some(error);
            }
        }
        self.enforce_local_pause();
        self.loaded = true;
        changed
    }
    fn enforce_local_pause(&mut self) {
        if self.locally_paused {
            self.file
                .accounts
                .iter_mut()
                .for_each(|account| account.enabled = false);
        }
    }
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.reading = None;
        self.error = None;
        self.candidate = None;
        self.next_poll = Instant::now();
        self.backoff = POLL_INTERVAL;
    }
    fn finish_read(
        &mut self,
        generation: u64,
        result: Result<LiveQuotaReading, LiveQuotaError>,
        now: Instant,
    ) {
        self.busy = false;
        if generation != self.generation {
            return;
        }
        match result {
            Ok(reading) => {
                self.reading = Some(reading);
                self.error = None;
                self.backoff = POLL_INTERVAL;
            }
            Err(error) => {
                self.reading = None;
                self.error = Some(error);
                self.backoff = (self.backoff * 2).min(MAX_BACKOFF);
            }
        }
        self.next_poll = now + self.backoff;
    }
}

impl LiveQuotaService {
    fn path(&self) -> Result<PathBuf, &'static str> {
        self.settings_path
            .clone()
            .map(Ok)
            .unwrap_or_else(consent_path)
    }
    fn codex(&self) -> Result<PathBuf, LiveQuotaError> {
        self.executable
            .clone()
            .map(Ok)
            .unwrap_or_else(codex_executable)
    }
    /// Starts a desktop-only timer. With default/no consent this never launches a CLI.
    pub fn start(service: &Arc<Self>, app: tauri::AppHandle) {
        let weak = Arc::downgrade(service);
        std::thread::spawn(move || {
            while let Some(service) = weak.upgrade() {
                if service.poll() {
                    let _ = app.emit("live-quota-updated", ());
                }
                drop(service);
                std::thread::sleep(Duration::from_secs(30));
            }
        });
    }

    /// Explicit user action authorizes one identity lookup, not periodic polling.
    pub fn identify(&self) -> Result<DiscoveredQuotaAccount, &'static str> {
        let generation = {
            let mut runtime = self.runtime.lock().unwrap();
            runtime.load(self.path());
            if let Some(error) = runtime.configuration_error {
                return Err(error);
            }
            if runtime.busy {
                return Err("A quota request is already running.");
            }
            runtime.busy = true;
            runtime.candidate = None;
            runtime.generation
        };
        let result = self
            .codex()
            .and_then(|path| quota_live::discover_codex_account(&path));
        let mut runtime = self.runtime.lock().unwrap();
        runtime.load(self.path());
        runtime.busy = false;
        if runtime.generation != generation {
            return Err("Quota consent changed during this request.");
        }
        match result {
            Ok(account) => {
                runtime.candidate = Some((account.clone(), Instant::now()));
                Ok(account)
            }
            Err(error) => Err(error.code()),
        }
    }

    /// Persist only a recently discovered identity plus a local display label.
    pub fn approve(&self, account_id: &str, label: &str) -> Result<(), &'static str> {
        let mut runtime = self.runtime.lock().unwrap();
        runtime.load(self.path());
        if let Some(error) = runtime.configuration_error {
            return Err(error);
        }
        let valid = runtime.candidate.as_ref().is_some_and(|(candidate, at)| {
            candidate.account_id == account_id && at.elapsed() <= CANDIDATE_TTL
        });
        if !valid {
            return Err("Identify this account again before approving polling.");
        }
        let mut accounts = runtime.file.accounts.clone();
        accounts.retain(|account| account.account_id != account_id);
        for account in &mut accounts {
            account.enabled = false;
        }
        accounts.push(QuotaAccountConsent {
            account_id: account_id.into(),
            label: label.trim().into(),
            consented_at: Utc::now(),
            enabled: true,
        });
        let file = ConsentFile {
            version: 1,
            accounts,
        };
        let expected = runtime
            .durable_file
            .as_ref()
            .ok_or("Live quota settings are unavailable.")?;
        save_file(&self.path()?, expected, &file)?;
        runtime.durable_file = Some(file.clone());
        runtime.file = file;
        runtime.locally_paused = false;
        runtime.invalidate();
        Ok(())
    }

    /// Enable/disable is account-scoped. Revocation removes consent altogether.
    /// One installed CLI has one current account, so enabling one pauses the others.
    pub fn change(
        &self,
        account_id: &str,
        enabled: bool,
        revoke: bool,
    ) -> Result<(), &'static str> {
        let mut runtime = self.runtime.lock().unwrap();
        runtime.load(self.path());
        if let Some(error) = runtime.configuration_error {
            return Err(error);
        }
        if !runtime
            .file
            .accounts
            .iter()
            .any(|account| account.account_id == account_id)
        {
            return Err("Account consent was not found.");
        }
        let mut accounts = runtime.file.accounts.clone();
        for account in &mut accounts {
            if account.account_id == account_id {
                account.enabled = enabled && !revoke;
            } else if enabled {
                account.enabled = false;
            }
        }
        if revoke {
            accounts.retain(|account| account.account_id != account_id);
        }
        let file = ConsentFile {
            version: 1,
            accounts,
        };
        // Disable in memory even if persistence fails; surface the failure and
        // retain the previous disk file, never claim durable revocation succeeded.
        runtime.invalidate();
        runtime
            .file
            .accounts
            .iter_mut()
            .for_each(|account| account.enabled = false);
        let expected = runtime
            .durable_file
            .as_ref()
            .ok_or("Live quota settings are unavailable.")?;
        if let Err(error) = self
            .path()
            .and_then(|path| save_file(&path, expected, &file))
        {
            // A failed revoke/pause is a local denial. An unrelated change in
            // another app must not silently resume this instance's polling.
            runtime.locally_paused = true;
            return Err(error);
        }
        runtime.durable_file = Some(file.clone());
        runtime.file = file;
        if enabled && !revoke {
            runtime.locally_paused = false;
        }
        runtime.enforce_local_pause();
        Ok(())
    }

    pub fn poll(&self) -> bool {
        let (generation, account_id) = {
            let mut runtime = self.runtime.lock().unwrap();
            let changed = runtime.load(self.path());
            if runtime.configuration_error.is_some()
                || runtime.busy
                || Instant::now() < runtime.next_poll
            {
                return changed;
            }
            let Some(account) = runtime.file.accounts.iter().find(|account| account.enabled) else {
                return changed;
            };
            let account_id = account.account_id.clone();
            runtime.busy = true;
            (runtime.generation, account_id)
        };
        let result = self
            .codex()
            .and_then(|path| quota_live::read_codex_quota(&path, &account_id));
        let mut runtime = self.runtime.lock().unwrap();
        runtime.load(self.path());
        runtime.finish_read(generation, result, Instant::now());
        true
    }

    pub fn status(&self, now: DateTime<Utc>) -> LiveQuotaStatus {
        let mut runtime = self.runtime.lock().unwrap();
        runtime.load(self.path());
        let accounts = runtime
            .file
            .accounts
            .iter()
            .map(|consent| {
                let reading = runtime
                    .reading
                    .as_ref()
                    .filter(|reading| consent.enabled && reading.account_id == consent.account_id);
                let buckets = reading
                    .map(|reading| {
                        reading
                            .limit_buckets
                            .iter()
                            .map(|bucket| LiveQuotaBucketView {
                                limit_id: bucket.limit_id.clone(),
                                limit_name: bucket.limit_name.clone(),
                                spend_control_reached: bucket.spend_control_reached,
                                snapshot: crate::quota::live_bucket_snapshot(
                                    bucket,
                                    reading.observed_at,
                                    now,
                                ),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let unavailable = if !consent.enabled {
                    Some("disabled")
                } else if let Some(error) = runtime.error {
                    Some(error.code())
                } else if reading.is_none_or(|reading| reading.limit_buckets.is_empty()) {
                    Some("no_observation")
                } else if reading.is_some_and(|reading| reading.observed_at > now) {
                    Some("clock_skew")
                } else if reading.is_some_and(|reading| {
                    now.signed_duration_since(reading.observed_at) > chrono::Duration::minutes(10)
                }) {
                    Some("stale_observation")
                } else {
                    None
                };
                LiveQuotaAccountView {
                    provider: "codex",
                    consent: consent.clone(),
                    observed_at: reading.map(|r| r.observed_at),
                    ordinary_usage_allowed: reading
                        .filter(|_| unavailable.is_none())
                        .and_then(|r| r.ordinary_usage_allowed),
                    buckets,
                    unavailable,
                }
            })
            .collect();
        LiveQuotaStatus {
            accounts,
            busy: runtime.busy,
            configuration_error: runtime.configuration_error,
        }
    }
}

fn codex_executable() -> Result<PathBuf, LiveQuotaError> {
    // Native executable only: no shell shim, command string, or credential-file discovery.
    let name = if cfg!(windows) { "codex.exe" } else { "codex" };
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .ok_or(LiveQuotaError::Offline)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(service: &LiveQuotaService, id: &str) {
        service.runtime.lock().unwrap().candidate = Some((
            DiscoveredQuotaAccount {
                account_id: id.into(),
                plan_type: None,
            },
            Instant::now(),
        ));
    }
    fn isolated(path: &Path) -> LiveQuotaService {
        LiveQuotaService {
            settings_path: Some(path.into()),
            executable: Some(path.with_extension("test-cli-missing")),
            ..Default::default()
        }
    }
    #[test]
    fn approval_pause_switch_and_revoke_persist_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let service = isolated(&path);
        assert!(service.status(Utc::now()).accounts.is_empty());
        assert!(!service.poll());
        assert!(service.approve("a", "A").is_err());
        candidate(&service, "a");
        assert!(service.approve("different", "A").is_err());
        assert!(service.approve("a", "").is_err());
        service.approve("a", "Account A").unwrap();
        candidate(&service, "b");
        service.approve("b", "Account B").unwrap();
        let restarted = isolated(&path);
        let status = restarted.status(Utc::now());
        assert_eq!(status.accounts.len(), 2);
        assert!(!status.accounts[0].consent.enabled);
        assert!(status.accounts[1].consent.enabled);
        restarted.change("a", true, false).unwrap();
        assert_eq!(
            restarted.status(Utc::now()).accounts[1].unavailable,
            Some("disabled")
        );
        restarted.change("a", false, false).unwrap();
        assert!(!restarted.poll());
        assert!(restarted.change("unknown", true, false).is_err());
        restarted.change("a", false, true).unwrap();
        let final_state = isolated(&path).status(Utc::now());
        assert_eq!(final_state.accounts.len(), 1);
        assert_eq!(final_state.accounts[0].consent.account_id, "b");
        assert!(!final_state.accounts[0].consent.enabled);
        assert!(!std::fs::read_to_string(path).unwrap().contains("Account A"));
    }
    #[test]
    fn failed_revoke_pauses_this_run_and_preserves_durable_consent_for_retry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let service = isolated(&path);
        service.status(Utc::now());
        candidate(&service, "a");
        service.approve("a", "A").unwrap();
        let before = std::fs::read(&path).unwrap();
        let lock = std::fs::File::open(path.with_extension("lock")).unwrap();
        // Model a Unix child retaining the open-file description after fork.
        // Explicit unlock must permit retry even while that descriptor lives.
        #[cfg(unix)]
        let _inherited_descriptor = lock.try_clone().unwrap();
        lock.try_lock().unwrap();
        assert!(service.change("a", false, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(
            service.status(Utc::now()).accounts[0].unavailable,
            Some("disabled")
        );
        assert!(!service.poll());
        lock.unlock().unwrap();
        drop(lock);
        service.change("a", false, true).unwrap();
        assert!(isolated(&path).status(Utc::now()).accounts.is_empty());
    }
    #[test]
    fn failed_revocation_stays_paused_across_unrelated_external_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let first = isolated(&path);
        first.status(Utc::now());
        candidate(&first, "b");
        first.approve("b", "B").unwrap();
        candidate(&first, "a");
        first.approve("a", "A").unwrap();
        let lock = std::fs::File::open(path.with_extension("lock")).unwrap();
        lock.try_lock().unwrap();
        assert!(first.change("a", false, true).is_err());
        lock.unlock().unwrap();
        drop(lock);
        let second = isolated(&path);
        second.change("b", false, true).unwrap();
        assert!(second.status(Utc::now()).accounts[0].consent.enabled);
        let local = first.status(Utc::now());
        assert_eq!(local.accounts[0].consent.account_id, "a");
        assert!(
            !local.accounts[0].consent.enabled,
            "unrelated disk edits cannot cancel a failed local revoke"
        );
        assert!(!first.poll());
        first.change("a", true, false).unwrap();
        assert!(
            first.status(Utc::now()).accounts[0].consent.enabled,
            "an explicit local enable can resume polling"
        );
    }
    #[test]
    fn another_instances_revoke_invalidates_cached_and_inflight_readings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let first = isolated(&path);
        first.status(Utc::now());
        candidate(&first, "a");
        first.approve("a", "A").unwrap();
        let second = isolated(&path);
        assert!(second.status(Utc::now()).accounts[0].consent.enabled);
        let (generation, before) = {
            let runtime = second.runtime.lock().unwrap();
            (runtime.generation, runtime.durable_file.clone().unwrap())
        };
        first.change("a", false, true).unwrap();
        assert!(
            save_file(&path, &before, &before).is_err(),
            "stale writes cannot resurrect consent"
        );
        assert!(
            second.poll(),
            "publish the externally changed consent without launching a CLI"
        );
        assert!(second.status(Utc::now()).accounts.is_empty());
        let mut runtime = second.runtime.lock().unwrap();
        runtime.finish_read(
            generation,
            Ok(LiveQuotaReading {
                account_id: "a".into(),
                ordinary_usage_allowed: Some(true),
                observed_at: Utc::now(),
                limit_buckets: vec![],
            }),
            Instant::now(),
        );
        assert!(
            runtime.reading.is_none(),
            "discard a response from before external revocation"
        );
        drop(runtime);
        assert!(!second.poll());
    }
    #[test]
    fn unreadable_consent_stops_a_previously_enabled_instance() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let service = isolated(&path);
        service.status(Utc::now());
        candidate(&service, "a");
        service.approve("a", "A").unwrap();
        std::fs::write(&path, "{broken").unwrap();
        let status = service.status(Utc::now());
        assert!(status.configuration_error.is_some());
        assert!(!status.accounts[0].consent.enabled);
        assert!(!service.poll());
        assert!(service.change("a", true, false).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{broken");
    }
    #[cfg(unix)]
    #[test]
    fn consent_reads_reject_links_and_pipes_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let regular = dir.path().join("regular.json");
        std::fs::write(&regular, r#"{"version":1,"accounts":[]}"#).unwrap();
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&regular, &link).unwrap();
        assert!(read_file(&link).is_err());
        let pipe = dir.path().join("pipe.json");
        let name = std::ffi::CString::new(pipe.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a live, NUL-terminated path inside the test's temp dir.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(read_file(&pipe).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn consented_service_reads_only_the_selected_synthetic_account_and_obeys_backoff() {
        let _fixture = crate::quota_live::tests::synthetic_process_test_guard();
        let dir = tempfile::tempdir().unwrap();
        let account = r#"{"id":2,"result":{"requiresOpenaiAuth":true,"account":{"type":"chatgpt","planType":"pro"},"workspaceRouting":{"chatgptAccountId":"approved"}}}"#;
        let rates = r#"{"id":3,"result":{"accountId":"approved","ordinaryUsageAllowed":true,"rateLimits":{"limitId":"codex","primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":1893456000}}}}"#;
        let executable = crate::quota_live::tests::fake_app_server(dir.path(), account, rates);
        let service = LiveQuotaService {
            executable: Some(executable.clone()),
            ..isolated(&dir.path().join("consent.json"))
        };
        assert!(!service.poll());
        let found = service.identify().unwrap();
        assert_eq!(found.account_id, "approved");
        assert!(!service.poll(), "lookup must not enable polling");
        service.approve("approved", "A").unwrap();
        assert!(service.poll());
        assert!(!service.poll(), "a successful read is rate-limited");
        let status = service.status(Utc::now());
        assert_eq!(status.accounts[0].ordinary_usage_allowed, Some(true));
        assert_eq!(status.accounts[0].buckets.len(), 1);
        service.change("approved", false, false).unwrap();
        std::fs::remove_file(&executable).unwrap();
        service.change("approved", true, false).unwrap();
        assert!(service.poll());
        assert!(!service.poll(), "failed reads must back off too");
        assert_eq!(
            service.status(Utc::now()).accounts[0].unavailable,
            Some("offline")
        );
        assert!(service.status(Utc::now()).accounts[0].buckets.is_empty());
        service.runtime.lock().unwrap().busy = true;
        assert!(service.identify().is_err());
    }
    #[test]
    fn account_permission_expires_with_the_reading_and_rejects_clock_skew() {
        let observed_at = Utc::now();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        let service = LiveQuotaService {
            runtime: Mutex::new(Runtime {
                loaded: true,
                file: ConsentFile {
                    version: 1,
                    accounts: vec![QuotaAccountConsent {
                        account_id: "synthetic-account".into(),
                        label: "Test".into(),
                        consented_at: observed_at,
                        enabled: true,
                    }],
                },
                reading: Some(LiveQuotaReading {
                    account_id: "synthetic-account".into(),
                    ordinary_usage_allowed: Some(false),
                    observed_at,
                    limit_buckets: vec![crate::quota_live::LiveQuotaBucket {
                        limit_id: "codex".into(),
                        limit_name: None,
                        spend_control_reached: Some(true),
                        primary: Some(crate::quota_live::LiveQuotaWindow {
                            used_percent: 80.0,
                            window_minutes: Some(300),
                            resets_at: None,
                        }),
                        secondary: None,
                        credits: None,
                    }],
                }),
                ..Default::default()
            }),
            ..isolated(&path)
        };
        {
            let mut runtime = service.runtime.lock().unwrap();
            std::fs::write(&path, serde_json::to_vec(&runtime.file).unwrap()).unwrap();
            runtime.durable_file = Some(runtime.file.clone());
        }
        let current = service.status(observed_at + chrono::Duration::minutes(5));
        assert_eq!(current.accounts[0].ordinary_usage_allowed, Some(false));
        assert_eq!(current.accounts[0].unavailable, None);

        let stale = service.status(observed_at + chrono::Duration::minutes(11));
        assert_eq!(stale.accounts[0].ordinary_usage_allowed, None);
        assert_eq!(stale.accounts[0].unavailable, Some("stale_observation"));
        assert!(stale.accounts[0].buckets[0].snapshot.windows[0].stale);

        let skewed = service.status(observed_at - chrono::Duration::seconds(1));
        assert_eq!(skewed.accounts[0].ordinary_usage_allowed, None);
        assert_eq!(skewed.accounts[0].unavailable, Some("clock_skew"));
        assert!(skewed.accounts[0].buckets[0].snapshot.windows[0]
            .unavailable
            .is_some());
    }
    #[test]
    fn consent_file_fails_closed_and_does_not_create_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("consent.json");
        assert!(read_file(&path).unwrap().accounts.is_empty());
        assert!(!path.exists());
        std::fs::write(&path, br#"{"version":99,"accounts":[]}"#).unwrap();
        assert!(read_file(&path).is_err());
        std::fs::write(&path, vec![b'x'; 32769]).unwrap();
        assert!(read_file(&path).is_err());
    }
    #[test]
    fn revoked_inflight_results_are_discarded_and_errors_back_off() {
        let mut runtime = Runtime {
            busy: true,
            ..Default::default()
        };
        let old_generation = runtime.generation;
        runtime.invalidate();
        runtime.finish_read(
            old_generation,
            Ok(LiveQuotaReading {
                account_id: "old".into(),
                ordinary_usage_allowed: None,
                observed_at: Utc::now(),
                limit_buckets: vec![],
            }),
            Instant::now(),
        );
        assert!(runtime.reading.is_none());
        assert!(!runtime.busy);
        let now = Instant::now();
        for _ in 0..20 {
            runtime.finish_read(runtime.generation, Err(LiveQuotaError::RateLimited), now);
        }
        assert_eq!(runtime.backoff, MAX_BACKOFF);
        assert_eq!(runtime.next_poll, now + MAX_BACKOFF);
        assert_eq!(runtime.error, Some(LiveQuotaError::RateLimited));
    }
}
