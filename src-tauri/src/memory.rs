//! Opt-in process and heap memory sampling.
//!
//! Three consecutive releases (v0.8.9-v0.8.11) tried to reduce this app's
//! resident memory using time-based instrumentation and synthetic probes
//! alone; none moved the real number, because nothing measured memory on the
//! machine where it actually mattered. This module gives a running process
//! two independent, greppable signals instead:
//!
//! - **OS-reported resident memory** (RSS / working set), sampled on demand
//!   via a syscall ([`sample_process_memory`]). Cheap enough to sample at a
//!   handful of phase boundaries with no ongoing cost; the sampling call
//!   itself only runs while performance tracking (`performance.rs`) is
//!   enabled, so the default (disabled) path never touches the OS here.
//! - **Allocator-tracked heap**, current and peak ([`heap_sample`]). This
//!   requires wrapping the global allocator, which is unavoidably compiled
//!   into every allocation/free — but the wrapper is gated by a runtime
//!   toggle ([`configure_heap_tracking`]), off by default: disabled, it costs
//!   one relaxed atomic load per alloc/free (and nothing else); enabled, it
//!   additionally costs the two relaxed atomics (`fetch_add`/`fetch_sub` and
//!   `fetch_max`) needed to track current and peak bytes.
//!
//! Comparing the two answers the question this instrumentation exists to
//! answer: high RSS with low tracked heap points at something outside the
//! allocator (most plausibly SQLite's page cache or an OS-level mapping);
//! high RSS *and* high tracked heap points at allocator retention instead.
//! See [`SqlitePragmaSnapshot`] for the SQLite side of that comparison.
//!
//! Neither signal is ever recorded — sampled or not — unless
//! `PerformanceRecorder::is_enabled` is true; the two opt-ins are
//! deliberately the same one, so this module can never add cost on the
//! default disabled path. No sample here ever carries a prompt, path, id, or
//! any other content — only phase names and byte counts, matching
//! `performance.rs`'s existing privacy contract.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::performance::PerformanceRecorder;

static HEAP_TRACKING_ENABLED: AtomicBool = AtomicBool::new(false);
static HEAP_ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static HEAP_PEAK_BYTES: AtomicU64 = AtomicU64::new(0);

/// Wraps the system allocator with a runtime-toggled pair of relaxed
/// atomics. Declared once, unconditionally, as this crate's sole
/// `#[global_allocator]` (see `lib.rs`) — it replaces the `#[cfg(test)]`-only
/// counting allocator earlier probes (`model.rs`, `history_store.rs`) used to
/// define for themselves: a binary can only ever have one global allocator,
/// so a test-only one could never coexist with a production one. Those
/// probes now drive this same allocator through [`configure_heap_tracking`]
/// and the raw accessors below instead of owning a private copy.
///
/// Behavior is identical to the default system allocator whenever tracking
/// is disabled (the default): every call still delegates to [`System`], and
/// the only extra work is one `Ordering::Relaxed` load to check the toggle.
pub struct TrackingAllocator;

unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() && HEAP_TRACKING_ENABLED.load(Ordering::Relaxed) {
            let now = HEAP_ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed)
                + layout.size() as u64;
            HEAP_PEAK_BYTES.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        if HEAP_TRACKING_ENABLED.load(Ordering::Relaxed) {
            HEAP_ALLOCATED_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
        }
    }
    // `GlobalAlloc::realloc`'s default body calls `self.alloc`/`self.dealloc`
    // rather than the system allocator directly, so it already routes
    // through the two overrides above and needs no override of its own.
}

/// Enables or disables heap tracking. Wired to the `memory_heap_tracking_enabled`
/// config field alongside every `PerformanceRecorder::configure` call site, so
/// it shares the performance recorder's off-by-default, explicit-opt-in
/// contract. Disabling clears both counters so a later re-enable starts from
/// a known baseline instead of carrying over a stale peak from a previous
/// enabled window.
pub fn configure_heap_tracking(enabled: bool) {
    HEAP_TRACKING_ENABLED.store(enabled, Ordering::Relaxed);
    if !enabled {
        HEAP_ALLOCATED_BYTES.store(0, Ordering::Relaxed);
        HEAP_PEAK_BYTES.store(0, Ordering::Relaxed);
    }
}

pub fn heap_tracking_enabled() -> bool {
    HEAP_TRACKING_ENABLED.load(Ordering::Relaxed)
}

/// Raw current heap-byte reading, regardless of whether tracking is enabled
/// (0 if it never was). `pub(crate)` for the probes that need an exact delta
/// across a region of code rather than the enabled-or-unavailable shape
/// [`heap_sample`] returns for recorded events.
pub(crate) fn heap_allocated_bytes_raw() -> u64 {
    HEAP_ALLOCATED_BYTES.load(Ordering::Relaxed)
}

/// Raw peak heap-byte reading since process start or the last
/// [`reset_heap_peak_to_current`], whichever is more recent.
pub(crate) fn heap_peak_bytes_raw() -> u64 {
    HEAP_PEAK_BYTES.load(Ordering::Relaxed)
}

/// Rebases the peak tracker to the current allocation level, so a later read
/// reflects only growth from this point forward. Used both by
/// [`record_phase_sample`] (so an "after" sample's peak covers only that
/// phase, not everything since the process started) and directly by probes
/// that want the same per-region isolation.
pub(crate) fn reset_heap_peak_to_current() {
    HEAP_PEAK_BYTES.store(heap_allocated_bytes_raw(), Ordering::Relaxed);
}

/// Heap reading shaped for a recorded event: `None` in both fields whenever
/// tracking is disabled, so a JSONL consumer can tell "0 bytes tracked" apart
/// from "not tracked" instead of a bare 0 meaning either.
#[derive(Debug, Clone, Copy, Default)]
pub struct HeapSample {
    pub current_bytes: Option<u64>,
    pub peak_bytes: Option<u64>,
}

pub fn heap_sample() -> HeapSample {
    if !heap_tracking_enabled() {
        return HeapSample::default();
    }
    HeapSample {
        current_bytes: Some(heap_allocated_bytes_raw()),
        peak_bytes: Some(heap_peak_bytes_raw()),
    }
}

/// OS-reported process memory. Fields are `None` on a platform or error path
/// where the underlying query is unavailable — never a fabricated zero.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessMemorySample {
    /// Current working set (Windows) / resident set size (Linux).
    pub rss_bytes: Option<u64>,
    /// Peak working set since process start (Windows) or peak resident set
    /// size, `VmHWM` (Linux). Unlike `rss_bytes`, this cannot miss a spike a
    /// point-in-time sample would land between.
    pub peak_rss_bytes: Option<u64>,
    /// Private (non-shared, non-mapped) commit, Windows only. A memory-mapped
    /// SQLite database counts toward `rss_bytes` but not this field, so
    /// comparing the two is a second, independent way to see an `mmap_size`
    /// effect show up.
    pub private_bytes: Option<u64>,
}

#[cfg(target_os = "windows")]
pub fn sample_process_memory() -> ProcessMemorySample {
    use windows_sys::Win32::System::ProcessStatus::{
        K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            std::ptr::addr_of_mut!(counters).cast(),
            counters.cb,
        )
    };
    if ok == 0 {
        return ProcessMemorySample::default();
    }
    ProcessMemorySample {
        rss_bytes: Some(counters.WorkingSetSize as u64),
        peak_rss_bytes: Some(counters.PeakWorkingSetSize as u64),
        private_bytes: Some(counters.PrivateUsage as u64),
    }
}

/// Linux has no crate already in this tree for this, but the kernel exposes
/// exactly what's needed as plain text at a fixed path — reading and parsing
/// two lines out of `/proc/self/status` is cheaper and smaller than adding a
/// dependency for it.
#[cfg(target_os = "linux")]
pub fn sample_process_memory() -> ProcessMemorySample {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return ProcessMemorySample::default();
    };
    let mut rss_bytes = None;
    let mut peak_rss_bytes = None;
    for line in status.lines() {
        if let Some(value) = line.strip_prefix("VmRSS:") {
            rss_bytes = parse_status_kb(value);
        } else if let Some(value) = line.strip_prefix("VmHWM:") {
            peak_rss_bytes = parse_status_kb(value);
        }
    }
    ProcessMemorySample {
        rss_bytes,
        peak_rss_bytes,
        private_bytes: None,
    }
}

#[cfg(target_os = "linux")]
fn parse_status_kb(value: &str) -> Option<u64> {
    value
        .trim()
        .strip_suffix(" kB")
        .and_then(|n| n.trim().parse::<u64>().ok())
        .map(|kb| kb * 1024)
}

/// macOS has no equivalent of `/proc` and no existing dependency in this tree
/// exposes `task_info`/`mach_task_self` (that needs a small extra crate, e.g.
/// `mach2`, not otherwise used by this app). Rather than add one for a
/// platform this instrumentation effort is not targeting, degrade to
/// unavailable — matching every other "OS or platform can't supply this"
/// path in the app (see `ProviderCapabilities`).
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub fn sample_process_memory() -> ProcessMemorySample {
    ProcessMemorySample::default()
}

/// SQLite `cache_size`/`mmap_size` for one open connection — the other half
/// of the RSS-vs-heap comparison this module exists to support. Neither
/// `history_store.rs` nor `scan_cache.rs` sets either pragma explicitly, so
/// both connections run on SQLite's compiled-in defaults; querying them
/// (rather than asserting the documented default) catches a bundled-library
/// build that overrides them.
#[derive(Debug, Clone, Copy)]
pub struct SqlitePragmaSnapshot {
    /// `PRAGMA cache_size` — pages if positive, `-1024`-scaled KiB if
    /// negative (SQLite's own encoding; see the pragma's documentation).
    pub cache_size: i64,
    /// `PRAGMA mmap_size` in bytes; 0 means memory-mapped I/O is off, so the
    /// page cache lives in ordinary heap-allocated buffers instead of a
    /// mapped region that would count toward RSS without ever touching the
    /// allocator.
    pub mmap_size: i64,
}

pub fn query_sqlite_pragmas(connection: &rusqlite::Connection) -> SqlitePragmaSnapshot {
    let cache_size = connection
        .query_row("PRAGMA cache_size", [], |row| row.get::<_, i64>(0))
        .unwrap_or(0);
    let mmap_size = connection
        .query_row("PRAGMA mmap_size", [], |row| row.get::<_, i64>(0))
        .unwrap_or(0);
    SqlitePragmaSnapshot {
        cache_size,
        mmap_size,
    }
}

/// Records one `memory.sqlite_pragmas` event per connection open — see
/// `SqlitePragmaSnapshot`'s doc comment. `connection_label` identifies which
/// of the app's two SQLite connections this is (`"history_store"` or
/// `"scan_cache"`), never a path.
pub fn record_sqlite_pragmas(
    performance: &PerformanceRecorder,
    connection_label: &str,
    pragmas: SqlitePragmaSnapshot,
) {
    if !performance.is_enabled() {
        return;
    }
    let metadata = BTreeMap::from([
        ("connection".to_string(), connection_label.to_string()),
        ("cache_size".to_string(), pragmas.cache_size.to_string()),
        ("mmap_size".to_string(), pragmas.mmap_size.to_string()),
    ]);
    performance.record_backend_duration_ms("memory.sqlite_pragmas", 0.0, true, metadata);
}

/// On-disk size of one SQLite database plus the headroom of the volume
/// holding it (issue #158). Fields are `None` where the query is unavailable
/// or fails — never a fabricated zero.
///
/// This exists because startup time tracks neither code nor data volume. The
/// v0.8.10–v0.8.13 recordings show `cache_lookup_sql_ms` moving between
/// 12,003 ms and 134,276 ms for a constant ~2.14 GB of cache hits, while
/// `cache_lookup_deserialize_ms` stays flat near 8,000 ms — CPU work per byte
/// unchanged, time to fetch the bytes an order of magnitude apart. The 8.9x
/// step landed on v0.8.12, a release that added only this module and two
/// `PRAGMA` reads, so no code change explains it. The standing hypothesis is
/// a large incrementally-grown database on a filling volume, which nothing
/// recorded today can confirm or refute. These two numbers are what turns
/// that inference into a measurement.
#[derive(Debug, Clone, Copy, Default)]
pub struct DatabaseFootprint {
    /// The database file itself.
    pub db_bytes: Option<u64>,
    /// Its `-wal` sidecar, if present. Absent (rather than zero) when the
    /// database is not in WAL mode or the file does not exist yet.
    pub wal_bytes: Option<u64>,
    /// Free and total bytes on the volume holding the database. A nearly-full
    /// volume is the condition under which large-file reads degrade, so the
    /// pair is only meaningful together.
    pub volume_free_bytes: Option<u64>,
    pub volume_total_bytes: Option<u64>,
}

/// Samples [`DatabaseFootprint`] for the database at `path`. Pure `stat` and
/// a free-space query — it never opens the database, so it is safe to call
/// while the connection is in use and costs nothing measurable.
pub fn sample_database_footprint(path: &std::path::Path) -> DatabaseFootprint {
    let file_len = |p: std::path::PathBuf| std::fs::metadata(p).ok().map(|m| m.len());
    let (volume_free_bytes, volume_total_bytes) =
        path.parent().map(volume_space).unwrap_or((None, None));
    // SQLite's sidecar is the database's full filename plus `-wal`, not a
    // replaced extension — `with_extension` would turn `history-v1.sqlite3`
    // into the right thing by luck and a dotless filename into the wrong one.
    let mut wal_name = path.as_os_str().to_os_string();
    wal_name.push("-wal");
    DatabaseFootprint {
        db_bytes: file_len(path.to_path_buf()),
        wal_bytes: file_len(wal_name.into()),
        volume_free_bytes,
        volume_total_bytes,
    }
}

#[cfg(target_os = "windows")]
fn volume_space(dir: &std::path::Path) -> (Option<u64>, Option<u64>) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free: u64 = 0;
    let mut total: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            std::ptr::addr_of_mut!(total),
            std::ptr::addr_of_mut!(free),
        )
    };
    if ok == 0 {
        return (None, None);
    }
    (Some(free), Some(total))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn volume_space(dir: &std::path::Path) -> (Option<u64>, Option<u64>) {
    use std::os::unix::ffi::OsStrExt;

    let Ok(c_path) = std::ffi::CString::new(dir.as_os_str().as_bytes()) else {
        return (None, None);
    };
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stat) } != 0 {
        return (None, None);
    }
    let block = stat.f_frsize as u64;
    (
        Some(block * stat.f_bavail as u64),
        Some(block * stat.f_blocks as u64),
    )
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
fn volume_space(_dir: &std::path::Path) -> (Option<u64>, Option<u64>) {
    (None, None)
}

/// Records one `memory.database_footprint` event per connection open. Like
/// [`record_sqlite_pragmas`], `connection_label` names which of the app's two
/// databases this is (`"history_store"` or `"scan_cache"`) and **never a
/// path** — sizes and volume headroom are not identifying, a filesystem
/// location is.
pub fn record_database_footprint(
    performance: &PerformanceRecorder,
    connection_label: &str,
    footprint: DatabaseFootprint,
) {
    if !performance.is_enabled() {
        return;
    }
    performance.record_backend_duration_ms(
        "memory.database_footprint",
        0.0,
        true,
        database_footprint_metadata(connection_label, footprint),
    );
}

fn database_footprint_metadata(
    connection_label: &str,
    footprint: DatabaseFootprint,
) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    metadata.insert("connection".to_string(), connection_label.to_string());
    insert_optional_bytes(&mut metadata, "db_bytes", footprint.db_bytes);
    insert_optional_bytes(&mut metadata, "wal_bytes", footprint.wal_bytes);
    insert_optional_bytes(
        &mut metadata,
        "volume_free_bytes",
        footprint.volume_free_bytes,
    );
    insert_optional_bytes(
        &mut metadata,
        "volume_total_bytes",
        footprint.volume_total_bytes,
    );
    metadata
}

/// Records one memory sample as a `memory.<phase>` performance event —
/// its own operation family, distinct from and additive to the existing
/// `startup.*` timing metrics (never changes their name, meaning, or
/// boundaries). Gated by the same opt-in toggle as every other performance
/// record: no OS call or allocator read happens at all unless tracking is
/// enabled, so this stays free on the default disabled path.
///
/// `boundary` is typically `"before"`/`"after"` for a bracketed phase, or
/// `"point"` for a single one-off sample (e.g. the post-startup idle sample).
/// Calling this with `boundary == "before"` also rebases the heap peak
/// tracker ([`reset_heap_peak_to_current`]) so the matching `"after"` call's
/// `heap_peak_bytes` reflects growth during just this phase, not everything
/// tracked since the process started or heap tracking was last enabled.
pub fn record_phase_sample(performance: &PerformanceRecorder, phase: &str, boundary: &str) {
    if !performance.is_enabled() {
        return;
    }
    let process = sample_process_memory();
    let heap = heap_sample();
    let mut metadata = BTreeMap::new();
    metadata.insert("boundary".to_string(), boundary.to_string());
    insert_optional_bytes(&mut metadata, "rss_bytes", process.rss_bytes);
    insert_optional_bytes(&mut metadata, "peak_rss_bytes", process.peak_rss_bytes);
    insert_optional_bytes(&mut metadata, "private_bytes", process.private_bytes);
    metadata.insert(
        "heap_tracking".to_string(),
        if heap_tracking_enabled() {
            "enabled"
        } else {
            "disabled"
        }
        .to_string(),
    );
    insert_optional_bytes(&mut metadata, "heap_bytes", heap.current_bytes);
    insert_optional_bytes(&mut metadata, "heap_peak_bytes", heap.peak_bytes);
    performance.record_backend_duration_ms(format!("memory.{phase}"), 0.0, true, metadata);

    if boundary == "before" {
        reset_heap_peak_to_current();
    }
}

fn insert_optional_bytes(metadata: &mut BTreeMap<String, String>, key: &str, value: Option<u64>) {
    metadata.insert(
        key.to_string(),
        value
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "unavailable".to_string()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    // Heap-tracking assertions in this module check the enabled/disabled
    // *shape* (Some vs None, toggle state) rather than exact byte deltas:
    // `HEAP_ALLOCATED_BYTES`/`HEAP_PEAK_BYTES` are process-wide statics, and
    // `cargo test` runs many tests concurrently on threads that all share
    // them, so an exact-delta assertion here would be flaky by construction
    // (see the `#[ignore]`d probes in `model.rs`/`history_store.rs`, which
    // accept that trade-off deliberately and instead document running them
    // filtered to a single test name).

    #[test]
    fn database_footprint_reports_sizes_and_volume_headroom() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("history-v1.sqlite3");
        std::fs::write(&db, vec![0u8; 4096]).unwrap();
        // SQLite's sidecar is the full filename plus `-wal`, not a replaced
        // extension; a `with_extension`-based sampler would miss it here.
        std::fs::write(dir.path().join("history-v1.sqlite3-wal"), vec![0u8; 1024]).unwrap();

        let footprint = sample_database_footprint(&db);

        assert_eq!(footprint.db_bytes, Some(4096));
        assert_eq!(footprint.wal_bytes, Some(1024));
        // Every platform this app ships on can answer these; a `None` here
        // means the free-space query regressed, which is the whole point of
        // the field measurement this feeds (issue #158).
        let free = footprint.volume_free_bytes.expect("volume free bytes");
        let total = footprint.volume_total_bytes.expect("volume total bytes");
        assert!(total > 0, "volume total should be positive");
        assert!(
            free <= total,
            "free ({free}) must not exceed total ({total})"
        );
    }

    #[test]
    fn database_footprint_reports_unavailable_rather_than_zero_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        // A ledger that does not exist yet must not report 0 bytes — a
        // fabricated zero would read as "empty database" in a recording
        // rather than "not there", exactly the confusion the Option-per-field
        // shape exists to prevent.
        let footprint = sample_database_footprint(&dir.path().join("missing.sqlite3"));

        assert_eq!(footprint.db_bytes, None);
        assert_eq!(footprint.wal_bytes, None);
        assert!(footprint.volume_total_bytes.is_some());
    }

    #[test]
    fn heap_tracking_is_disabled_by_default() {
        assert!(!heap_tracking_enabled());
        let sample = heap_sample();
        assert_eq!(sample.current_bytes, None);
        assert_eq!(sample.peak_bytes, None);
    }

    #[test]
    fn disabling_heap_tracking_clears_and_reports_unavailable() {
        configure_heap_tracking(true);
        assert!(heap_tracking_enabled());
        let enabled_sample = heap_sample();
        assert!(enabled_sample.current_bytes.is_some());
        assert!(enabled_sample.peak_bytes.is_some());

        configure_heap_tracking(false);
        assert!(!heap_tracking_enabled());
        let disabled_sample = heap_sample();
        assert_eq!(disabled_sample.current_bytes, None);
        assert_eq!(disabled_sample.peak_bytes, None);
        assert_eq!(heap_allocated_bytes_raw(), 0);
        assert_eq!(heap_peak_bytes_raw(), 0);
    }

    #[test]
    fn record_phase_sample_is_a_no_op_when_performance_tracking_is_disabled() {
        let recorder = PerformanceRecorder::default();
        assert!(!recorder.is_enabled());
        record_phase_sample(&recorder, "test_phase", "before");
        let status = recorder.status();
        assert_eq!(status.recorded_this_run, 0);
    }

    #[test]
    fn record_sqlite_pragmas_is_a_no_op_when_performance_tracking_is_disabled() {
        let recorder = PerformanceRecorder::default();
        record_sqlite_pragmas(
            &recorder,
            "history_store",
            SqlitePragmaSnapshot {
                cache_size: -2000,
                mmap_size: 0,
            },
        );
        assert_eq!(recorder.status().recorded_this_run, 0);
    }

    #[test]
    fn record_database_footprint_is_a_no_op_when_performance_tracking_is_disabled() {
        let recorder = PerformanceRecorder::default();
        record_database_footprint(&recorder, "history_store", DatabaseFootprint::default());
        assert_eq!(recorder.status().recorded_this_run, 0);
    }

    #[test]
    fn database_footprint_metadata_encodes_bytes_and_distinguishes_unavailable() {
        // Exercised as a pure function rather than through an enabled
        // `PerformanceRecorder`: enabling one writes to the user's real
        // performance log directory, which no test in this crate does.
        let metadata = database_footprint_metadata(
            "history_store",
            DatabaseFootprint {
                db_bytes: Some(3_435_134_976),
                wal_bytes: None,
                volume_free_bytes: Some(73_859_072_000),
                volume_total_bytes: Some(976_762_888_192),
            },
        );

        assert_eq!(metadata["connection"], "history_store");
        assert_eq!(metadata["db_bytes"], "3435134976");
        // Absent, not zero — a `0` here would read as "empty database".
        assert_eq!(metadata["wal_bytes"], "unavailable");
        assert_eq!(metadata["volume_free_bytes"], "73859072000");
        assert_eq!(metadata["volume_total_bytes"], "976762888192");
        // `sanitize_metadata` keeps only the first 16 keys and drops any whose
        // name is not a valid operation token, so both are worth pinning.
        assert!(metadata.len() <= 16);
        assert!(metadata.keys().all(|key| key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))));
    }

    /// Direct evidence for this PR's "what would the pragma values show"
    /// question: an in-memory connection configured exactly like
    /// `history_store.rs`/`scan_cache.rs` (WAL + the pragmas each sets, no
    /// `cache_size`/`mmap_size` override) reports SQLite's compiled-in
    /// defaults. `cache_size` well under the docs' `-2000` (~2 MB) and
    /// `mmap_size` at 0 (mapping disabled) both point away from the SQLite
    /// page-cache hypothesis for a multi-GB RSS: two connections at these
    /// settings cannot plausibly account for gigabytes on their own.
    #[test]
    fn sqlite_pragma_defaults_match_documented_conservative_values() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")
            .unwrap();
        let pragmas = query_sqlite_pragmas(&connection);
        assert!(
            pragmas.cache_size.unsigned_abs() <= 2_000,
            "expected the documented conservative default (-2000, ~2 MB); got {}",
            pragmas.cache_size
        );
        assert_eq!(
            pragmas.mmap_size, 0,
            "expected memory-mapped I/O to be off by default"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_process_memory_sample_is_available() {
        let sample = sample_process_memory();
        assert!(sample.rss_bytes.unwrap_or(0) > 0);
        assert!(sample.peak_rss_bytes.unwrap_or(0) > 0);
        assert!(sample.peak_rss_bytes.unwrap() >= sample.rss_bytes.unwrap());
    }

    #[test]
    #[ignore = "performance probe; run with `cargo test --release --lib \
                probe_heap_tracking_allocation_overhead -- --ignored --nocapture` \
                (filtering to this exact test name keeps other concurrently-running \
                tests' allocations out of the timed loop)"]
    fn probe_heap_tracking_allocation_overhead() {
        // Answers this PR's "measure the overhead before committing to it"
        // requirement directly: times the same alloc/dealloc-heavy workload
        // with `TrackingAllocator`'s two extra atomics active vs. inactive
        // (tracking off still pays one relaxed load per call — see the
        // module doc comment — so this also captures that residual, not just
        // the fully-disabled-vs-System-allocator cost).
        const ITERATIONS: usize = 2_000_000;
        const ALLOC_SIZE: usize = 256;

        fn run_workload() -> std::time::Duration {
            let started = std::time::Instant::now();
            for _ in 0..ITERATIONS {
                let buffer = vec![0_u8; ALLOC_SIZE];
                std::hint::black_box(&buffer);
                drop(buffer);
            }
            started.elapsed()
        }

        // Warm up (page faults, allocator internal structures) before either
        // timed run so the first measured variant isn't unfairly penalized.
        configure_heap_tracking(false);
        let _ = run_workload();

        configure_heap_tracking(false);
        let disabled = run_workload();

        configure_heap_tracking(true);
        let enabled = run_workload();
        configure_heap_tracking(false);

        let disabled_ns_per_iter = disabled.as_nanos() as f64 / ITERATIONS as f64;
        let enabled_ns_per_iter = enabled.as_nanos() as f64 / ITERATIONS as f64;
        let overhead_percent =
            100.0 * (enabled_ns_per_iter - disabled_ns_per_iter) / disabled_ns_per_iter.max(0.001);
        eprintln!(
            "heap tracking overhead over {ITERATIONS} alloc/dealloc pairs of {ALLOC_SIZE} bytes:\n\
             \x20 disabled: {disabled:?} ({disabled_ns_per_iter:.1} ns/iter)\n\
             \x20 enabled:  {enabled:?} ({enabled_ns_per_iter:.1} ns/iter)\n\
             \x20 overhead: {overhead_percent:.1}%",
        );
    }
}
