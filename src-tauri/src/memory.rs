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
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

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
#[derive(Debug, Clone, Copy, Default, Serialize)]
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
#[derive(Debug, Clone, Copy, Default, Serialize)]
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
#[derive(Debug, Clone, Copy, Default, Serialize)]
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
    if let Ok(mut cache) = LATEST_FOOTPRINTS.lock() {
        cache.insert(connection_label.to_string(), footprint);
    }
    performance.record_backend_duration_ms(
        "memory.database_footprint",
        0.0,
        true,
        database_footprint_metadata(connection_label, footprint),
    );
}

/// Named [`DatabaseFootprint`] paired with the connection it belongs to
/// (`"history_store"` or `"scan_cache"`), for the live diagnostics view.
#[derive(Debug, Clone, Serialize)]
pub struct DatabaseFootprintEntry {
    pub connection: String,
    #[serde(flatten)]
    pub footprint: DatabaseFootprint,
}

/// Process-wide cache of the most recent [`record_database_footprint`] call
/// per connection label — the live view's data source, so it can show
/// database sizes without re-`stat`ing a file on every poll. Never grows
/// past the small, fixed number of connections this app ever opens (two:
/// `history_store`, `scan_cache`), and stays empty for the lifetime of a
/// process that never enables tracking, since [`record_database_footprint`]
/// returns before touching it while disabled.
static LATEST_FOOTPRINTS: Mutex<BTreeMap<String, DatabaseFootprint>> = Mutex::new(BTreeMap::new());

fn latest_database_footprints() -> Vec<DatabaseFootprintEntry> {
    LATEST_FOOTPRINTS
        .lock()
        .map(|cache| {
            cache
                .iter()
                .map(|(connection, footprint)| DatabaseFootprintEntry {
                    connection: connection.clone(),
                    footprint: *footprint,
                })
                .collect()
        })
        .unwrap_or_default()
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

/// Which phase is currently between a `"before"` and `"after"`
/// [`record_phase_sample`] call, for the live diagnostics view (issue #163).
/// Phases in this app are sequential, not nested (`commands::spawn_scan`
/// waits on history hydration before starting the bulk scan; every other
/// phase runs on the same startup thread after the previous one's `"after"`
/// call), so one slot is enough — see [`clear_active_phase`]'s name check
/// for the defensive case where that assumption is ever violated.
struct ActivePhase {
    phase: String,
    started_at: Instant,
}

static ACTIVE_PHASE: Mutex<Option<ActivePhase>> = Mutex::new(None);

fn set_active_phase(phase: &str) {
    if let Ok(mut guard) = ACTIVE_PHASE.lock() {
        *guard = Some(ActivePhase {
            phase: phase.to_string(),
            started_at: Instant::now(),
        });
    }
}

/// Clears the active-phase slot only if it still names `phase`, so an
/// out-of-order or overlapping call (which should never happen given the
/// sequential contract above) cannot clobber a different phase's state.
fn clear_active_phase(phase: &str) {
    if let Ok(mut guard) = ACTIVE_PHASE.lock() {
        if guard.as_ref().map(|active| active.phase.as_str()) == Some(phase) {
            *guard = None;
        }
    }
}

/// Current active phase and its elapsed time, if any — `None` once the
/// process is idle between phases (e.g. after startup finishes) or before
/// the first one starts.
fn active_phase_status() -> Option<(String, u64)> {
    ACTIVE_PHASE.lock().ok().and_then(|guard| {
        guard.as_ref().map(|active| {
            (
                active.phase.clone(),
                active.started_at.elapsed().as_millis() as u64,
            )
        })
    })
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
/// tracked since the process started or heap tracking was last enabled, and
/// records `phase` as the current [`active_phase_status`] until the matching
/// `"after"` call. `"point"` samples never touch the active-phase slot: they
/// are not brackets, so there is no `"after"` to clear them.
pub fn record_phase_sample(performance: &PerformanceRecorder, phase: &str, boundary: &str) {
    if !performance.is_enabled() {
        return;
    }
    if boundary == "before" {
        set_active_phase(phase);
    } else if boundary == "after" {
        clear_active_phase(phase);
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

// ---------------------------------------------------------------------
// Continuous phase sampling (issue #163)
//
// `record_phase_sample` above gives exactly two points per bracketed phase:
// a "before" and an "after". That is enough to say a phase got slower and
// nothing else — v0.8.14's 118.4s parallel scan phase (v0.8.13: 51.7s) could
// have been uniformly slower or stalled 60s in one place, and the two-point
// recording cannot tell those apart. `PhaseSampler` fills the gap: while a
// phase this app expects to run long (the bulk scan, history hydration) is
// underway, it samples at a fixed interval for the phase's whole duration
// and emits each sample as its own `memory.<phase>.sample` event carrying
// the active phase, elapsed time, memory, and caller-supplied progress
// context (files scanned, sessions hydrated) — enough to plot a curve and
// tell a stall apart from steady slow progress.
// ---------------------------------------------------------------------

/// Progress context for one sampler tick — e.g. files scanned, sessions
/// hydrated. Both fields are `None` when the phase has no meaningful count
/// to report (never a fabricated 0), matching every other "unavailable"
/// convention in this module.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct PhaseProgress {
    pub done: Option<u64>,
    pub total: Option<u64>,
}

/// Supplies [`PhaseProgress`] for each tick. A boxed closure rather than a
/// trait so each `commands.rs` call site can close over exactly the
/// atomic/counter it already has (`state.scan_done`/`scan_total` for the
/// bulk scan, `state.sessions.len()` for hydration) without this module
/// needing to know what a scan or a hydration is.
pub type ProgressFn = Box<dyn Fn() -> PhaseProgress + Send>;

/// One continuous-sampler tick, shaped for both the performance log (via
/// [`PerformanceRecorder::record_backend_duration_ms`]) and the live
/// diagnostics view (via [`recent_phase_samples`]).
#[derive(Debug, Clone, Serialize)]
pub struct PhaseSampleEvent {
    pub phase: String,
    pub sample_index: u32,
    pub elapsed_ms: u64,
    pub rss_bytes: Option<u64>,
    pub peak_rss_bytes: Option<u64>,
    pub private_bytes: Option<u64>,
    pub heap_bytes: Option<u64>,
    pub heap_peak_bytes: Option<u64>,
    pub progress_done: Option<u64>,
    pub progress_total: Option<u64>,
    /// True on the sample that hit the phase's sample cap (see
    /// [`MAX_PHASE_SAMPLES`]) — never silent: a consumer of this stream can
    /// always tell a capped phase apart from one that simply finished within
    /// budget, rather than the sampler quietly going quiet partway through.
    pub capped: bool,
}

/// Bounds how many samples one phase can ever emit, so a pathological
/// multi-hour phase cannot flood the performance log. At the default 500ms
/// interval this is a little under 17 minutes of sampling — generous for
/// every phase this instrumentation currently targets (a 118s scan is ~236
/// samples). The sample that reaches this cap is still recorded, with
/// `capped: true`, and then sampling stops; it does not silently degrade to
/// a slower cadence, so a consumer always knows exactly how much of the
/// phase's tail has no coverage.
const MAX_PHASE_SAMPLES: u32 = 2_000;

/// How many recent samples the live view keeps for its sparkline,
/// independent of (and much smaller than) [`MAX_PHASE_SAMPLES`]. Chosen to
/// match `Sparkline.svelte`'s own `MAX_POINTS` stride-sampling threshold, so
/// every sample this module keeps is one the frontend actually plots.
const RECENT_SAMPLES_CAP: usize = 240;

static RECENT_PHASE_SAMPLES: Mutex<VecDeque<PhaseSampleEvent>> = Mutex::new(VecDeque::new());

fn push_recent_sample(event: PhaseSampleEvent) {
    if let Ok(mut recent) = RECENT_PHASE_SAMPLES.lock() {
        recent.push_back(event);
        while recent.len() > RECENT_SAMPLES_CAP {
            recent.pop_front();
        }
    }
}

/// Drops every sample from a previous phase so a newly started sampler's
/// first tick does not share a chart with stale data from whatever ran
/// before it. Samples are retained (not cleared) once a sampler stops, so
/// the live view can still show a just-finished phase's curve until the next
/// one starts.
fn clear_recent_samples() {
    if let Ok(mut recent) = RECENT_PHASE_SAMPLES.lock() {
        recent.clear();
    }
}

/// Recent samples, oldest first, for the live diagnostics view. Empty
/// whenever no sampler has run yet in this process.
pub fn recent_phase_samples() -> Vec<PhaseSampleEvent> {
    RECENT_PHASE_SAMPLES
        .lock()
        .map(|recent| recent.iter().cloned().collect())
        .unwrap_or_default()
}

/// Signals a [`PhaseSampler`]'s background thread to stop, waking it
/// immediately via a condvar rather than making it poll — the thread spends
/// almost all of its life blocked in [`StopSignal::wait`], not spinning.
struct StopSignal {
    stopped: Mutex<bool>,
    condvar: Condvar,
}

impl StopSignal {
    fn new() -> Self {
        Self {
            stopped: Mutex::new(false),
            condvar: Condvar::new(),
        }
    }

    fn signal(&self) {
        if let Ok(mut stopped) = self.stopped.lock() {
            *stopped = true;
        }
        self.condvar.notify_all();
    }

    fn is_stopped(&self) -> bool {
        self.stopped.lock().map(|stopped| *stopped).unwrap_or(true)
    }

    /// Blocks for up to `interval`, waking early if [`Self::signal`] is
    /// called from another thread.
    fn wait(&self, interval: Duration) {
        let Ok(stopped) = self.stopped.lock() else {
            return;
        };
        let _ = self
            .condvar
            .wait_timeout_while(stopped, interval, |stopped| !*stopped);
    }
}

/// Continuous memory sampler for one long-running phase (issue #163):
/// samples RSS/heap plus caller-supplied progress at a fixed interval for as
/// long as the phase runs, so a 100+ second phase produces a curve instead
/// of the two boundary samples [`record_phase_sample`] alone gives.
///
/// [`Self::start`] returns `None` immediately when tracking is disabled — no
/// thread, timer, or allocation is created on the default disabled path,
/// the same zero-cost contract as every other entry point in this module.
pub struct PhaseSampler {
    stop: Arc<StopSignal>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl PhaseSampler {
    /// Starts sampling `phase` every `interval` while `performance` is
    /// enabled; `progress` is polled once per tick for caller-supplied
    /// context (files scanned, sessions hydrated). Call sites bracket this
    /// with their own `record_phase_sample(performance, phase, "before"/
    /// "after")` calls, unchanged — this only adds what happens between them.
    pub fn start(
        performance: &PerformanceRecorder,
        phase: impl Into<String>,
        interval: Duration,
        progress: ProgressFn,
    ) -> Option<Self> {
        Self::start_with_cap(performance, phase, interval, progress, MAX_PHASE_SAMPLES)
    }

    fn start_with_cap(
        performance: &PerformanceRecorder,
        phase: impl Into<String>,
        interval: Duration,
        progress: ProgressFn,
        cap: u32,
    ) -> Option<Self> {
        if !performance.is_enabled() {
            return None;
        }
        // Cloning only happens once we know a thread will actually be
        // spawned: `PerformanceRecorder`'s fields are all `Arc`-backed (see
        // its doc comment), so this is an atomic refcount bump, not a second
        // recorder.
        let performance = performance.clone();
        let phase = phase.into();
        clear_recent_samples();
        let stop = Arc::new(StopSignal::new());
        let thread_stop = Arc::clone(&stop);
        let thread_phase = phase.clone();
        let handle = std::thread::Builder::new()
            .name(format!("phase-sampler-{phase}"))
            .spawn(move || {
                sampler_loop(
                    performance,
                    thread_phase,
                    interval,
                    progress,
                    cap,
                    thread_stop,
                )
            })
            .map_err(|error| {
                tracing::warn!("phase sampler unavailable for {}: {}", phase, error);
            })
            .ok();
        Some(Self { stop, handle })
    }

    /// Test-only variant of [`Self::start`] with an injectable sample cap, so
    /// the "bounded output" contract is verifiable in milliseconds instead of
    /// needing [`MAX_PHASE_SAMPLES`] real ticks.
    #[cfg(test)]
    fn start_for_test(
        performance: &PerformanceRecorder,
        phase: impl Into<String>,
        interval: Duration,
        progress: ProgressFn,
        cap: u32,
    ) -> Option<Self> {
        Self::start_with_cap(performance, phase, interval, progress, cap)
    }

    /// Signals the sampler thread to stop after its current tick and waits
    /// for it to exit, so a caller's matching `record_phase_sample(..,
    /// "after")` call is always ordered after the sampler's last tick in the
    /// log.
    pub fn stop(mut self) {
        self.stop.signal();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for PhaseSampler {
    fn drop(&mut self) {
        // Safety net for a call site that returns early and skips `stop()`:
        // still signal the thread to exit rather than leaking it for the
        // rest of the process's life. Does not join — `Drop` must never
        // block, and every real call site calls `stop()` explicitly for the
        // ordering guarantee documented there.
        self.stop.signal();
    }
}

fn sampler_loop(
    performance: PerformanceRecorder,
    phase: String,
    interval: Duration,
    progress: ProgressFn,
    cap: u32,
    stop: Arc<StopSignal>,
) {
    let phase_started = Instant::now();
    let mut index: u32 = 0;
    loop {
        if stop.is_stopped() {
            return;
        }
        // `cap` is the maximum *count* of samples this phase may emit, so
        // the tick at `index == cap - 1` (the `cap`th sample, 0-indexed) is
        // the one that must carry `capped: true` — not one past it.
        let capped = index + 1 >= cap;
        sampler_tick(
            &performance,
            &phase,
            phase_started,
            index,
            &progress,
            capped,
        );
        if capped {
            return;
        }
        index += 1;
        stop.wait(interval);
    }
}

fn sampler_tick(
    performance: &PerformanceRecorder,
    phase: &str,
    phase_started: Instant,
    index: u32,
    progress: &ProgressFn,
    capped: bool,
) {
    let process = sample_process_memory();
    let heap = heap_sample();
    let progress_value = progress();
    let elapsed_ms = phase_started.elapsed().as_millis() as u64;

    push_recent_sample(PhaseSampleEvent {
        phase: phase.to_string(),
        sample_index: index,
        elapsed_ms,
        rss_bytes: process.rss_bytes,
        peak_rss_bytes: process.peak_rss_bytes,
        private_bytes: process.private_bytes,
        heap_bytes: heap.current_bytes,
        heap_peak_bytes: heap.peak_bytes,
        progress_done: progress_value.done,
        progress_total: progress_value.total,
        capped,
    });

    let mut metadata = BTreeMap::new();
    metadata.insert("sample_index".to_string(), index.to_string());
    metadata.insert("elapsed_ms".to_string(), elapsed_ms.to_string());
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
    if let Some(done) = progress_value.done {
        metadata.insert("progress_done".to_string(), done.to_string());
    }
    if let Some(total) = progress_value.total {
        metadata.insert("progress_total".to_string(), total.to_string());
    }
    metadata.insert("capped".to_string(), capped.to_string());
    performance.record_backend_duration_ms(format!("memory.{phase}.sample"), 0.0, true, metadata);
}

/// Everything the live diagnostics view needs in one call (issue #163):
/// current RSS/peak/heap, the active phase and its elapsed time (if any),
/// the most recent phase's progress, recent samples for the sparkline, and
/// known database footprints. `enabled: false` with every other field at its
/// default is the whole disabled-path answer — the frontend must render that
/// as "tracking is off", never as zeros.
#[derive(Debug, Clone, Default, Serialize)]
pub struct MemoryLiveStatus {
    pub enabled: bool,
    pub process: Option<ProcessMemorySample>,
    pub heap: Option<HeapSample>,
    pub active_phase: Option<String>,
    pub active_phase_elapsed_ms: Option<u64>,
    pub progress_done: Option<u64>,
    pub progress_total: Option<u64>,
    pub recent_samples: Vec<PhaseSampleEvent>,
    pub database_footprints: Vec<DatabaseFootprintEntry>,
}

pub fn live_status(performance: &PerformanceRecorder) -> MemoryLiveStatus {
    if !performance.is_enabled() {
        return MemoryLiveStatus::default();
    }
    let recent = recent_phase_samples();
    let (progress_done, progress_total) = recent
        .last()
        .map(|sample| (sample.progress_done, sample.progress_total))
        .unwrap_or((None, None));
    let active = active_phase_status();
    MemoryLiveStatus {
        enabled: true,
        process: Some(sample_process_memory()),
        heap: Some(heap_sample()),
        active_phase: active.as_ref().map(|(phase, _)| phase.clone()),
        active_phase_elapsed_ms: active.map(|(_, elapsed_ms)| elapsed_ms),
        progress_done,
        progress_total,
        recent_samples: recent,
        database_footprints: latest_database_footprints(),
    }
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

    // `PhaseSampler` tests below share process-wide statics
    // (`ACTIVE_PHASE`, `RECENT_PHASE_SAMPLES`, `LATEST_FOOTPRINTS`) with each
    // other the same way the heap-tracking tests above share the heap
    // counters — `cargo test` runs this file's tests concurrently on
    // multiple threads by default. Unlike the heap tests, these assertions
    // need exact counts (sample counts, `capped` on a specific index), so
    // rather than weakening them to shape-only, every test that touches this
    // state takes `TEST_SERIAL` first to run relative to its siblings one at
    // a time. This does not protect against a *different* test module
    // touching these statics, but nothing else in this crate does.
    static TEST_SERIAL: Mutex<()> = Mutex::new(());

    fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_SERIAL
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn no_progress() -> PhaseProgress {
        PhaseProgress::default()
    }

    #[test]
    fn phase_sampler_does_not_start_when_tracking_is_disabled() {
        let _guard = serial_guard();
        let recorder = PerformanceRecorder::default();
        assert!(!recorder.is_enabled());
        let sampler = PhaseSampler::start(
            &recorder,
            "test_phase",
            Duration::from_millis(1),
            Box::new(no_progress),
        );
        assert!(
            sampler.is_none(),
            "no sampler, and so no thread, when disabled"
        );
        // Nothing should have been queued either — this is the same
        // zero-cost contract as every other function in this module.
        assert_eq!(recorder.status().recorded_this_run, 0);
    }

    #[test]
    fn phase_sampler_ticks_progress_and_stops_cleanly() {
        let _guard = serial_guard();
        let recorder = PerformanceRecorder::enabled_for_test();
        let sampler = PhaseSampler::start_for_test(
            &recorder,
            "lifecycle_phase",
            Duration::from_millis(2),
            Box::new(|| PhaseProgress {
                done: Some(5),
                total: Some(10),
            }),
            1_000, // cap far above what a short-lived test can reach
        )
        .expect("sampler starts when tracking is enabled");

        std::thread::sleep(Duration::from_millis(30));
        sampler.stop();

        let samples = recent_phase_samples();
        assert!(
            samples.len() >= 2,
            "expected multiple ticks over 30ms at a 2ms interval, got {}",
            samples.len()
        );
        assert!(samples.iter().all(|s| s.phase == "lifecycle_phase"));
        assert!(samples.iter().all(|s| !s.capped));
        assert!(samples
            .iter()
            .all(|s| s.progress_done == Some(5) && s.progress_total == Some(10)));
        // sample_index strictly increases and starts at 0.
        assert_eq!(samples[0].sample_index, 0);
        for pair in samples.windows(2) {
            assert!(pair[1].sample_index > pair[0].sample_index);
        }

        // `stop()` joins the thread before returning, so no further ticks
        // land after this point — the count observed immediately after
        // `stop()` must still hold a moment later.
        let count_after_stop = recent_phase_samples().len();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(
            recent_phase_samples().len(),
            count_after_stop,
            "sampler kept ticking after stop()"
        );
    }

    #[test]
    fn phase_sampler_bounds_output_at_its_cap() {
        let _guard = serial_guard();
        let recorder = PerformanceRecorder::enabled_for_test();
        const CAP: u32 = 3;
        let sampler = PhaseSampler::start_for_test(
            &recorder,
            "capped_phase",
            Duration::from_millis(2),
            Box::new(no_progress),
            CAP,
        )
        .expect("sampler starts when tracking is enabled");

        // The sampler stops itself once it reaches the cap; give it ample
        // time to do so rather than calling `stop()` (which would race the
        // sampler's own natural exit).
        std::thread::sleep(Duration::from_millis(100));

        let samples = recent_phase_samples();
        assert_eq!(
            samples.len(),
            CAP as usize,
            "a pathological phase must not emit more than its cap"
        );
        assert!(
            samples.last().unwrap().capped,
            "the sample that hits the cap must say so, not go quiet silently"
        );
        assert!(samples[..samples.len() - 1].iter().all(|s| !s.capped));

        // Confirm the thread really exited (no further growth) rather than
        // merely pausing.
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(recent_phase_samples().len(), CAP as usize);

        drop(sampler); // exercises the Drop safety net; already stopped.
    }

    /// Starts a sampler and gives its background thread enough wall-clock
    /// time to emit every tick up to `cap` before stopping it, so a test
    /// doesn't race the thread's first OS scheduling quantum — a real call
    /// site never calls `stop()` this soon after `start()` (a real phase
    /// runs for seconds to minutes), so nothing here needs to tolerate zero
    /// ticks the way production code does.
    fn run_sampler_to_completion(
        recorder: &PerformanceRecorder,
        phase: &str,
        interval: Duration,
        progress: ProgressFn,
        cap: u32,
    ) {
        let sampler = PhaseSampler::start_for_test(recorder, phase, interval, progress, cap)
            .expect("sampler starts when tracking is enabled");
        std::thread::sleep(interval * cap.max(1) + Duration::from_millis(20));
        sampler.stop();
    }

    #[test]
    fn phase_sampler_clears_previous_phase_samples_on_start() {
        let _guard = serial_guard();
        let recorder = PerformanceRecorder::enabled_for_test();

        run_sampler_to_completion(
            &recorder,
            "phase_a",
            Duration::from_millis(2),
            Box::new(no_progress),
            5,
        );
        assert!(!recent_phase_samples().is_empty());

        run_sampler_to_completion(
            &recorder,
            "phase_b",
            Duration::from_millis(2),
            Box::new(no_progress),
            3,
        );

        let samples = recent_phase_samples();
        assert!(!samples.is_empty());
        assert!(
            samples.iter().all(|s| s.phase == "phase_b"),
            "starting a new sampler must clear the previous phase's samples"
        );
    }

    /// Forces `ACTIVE_PHASE` back to a known-clean slate rather than
    /// asserting it is already clean: these tests share that static with
    /// every other test in this module under `TEST_SERIAL`, and a sibling
    /// test that panics mid-test (before reaching its own cleanup call) can
    /// otherwise leave it dirty for whichever test the OS scheduler happens
    /// to run next — a false failure in an unrelated test, not evidence of a
    /// real bug here.
    fn reset_active_phase_for_test() {
        if let Ok(mut guard) = ACTIVE_PHASE.lock() {
            *guard = None;
        }
    }

    #[test]
    fn record_phase_sample_tracks_active_phase_between_before_and_after() {
        let _guard = serial_guard();
        reset_active_phase_for_test();
        let recorder = PerformanceRecorder::enabled_for_test();

        record_phase_sample(&recorder, "boundary_phase", "before");
        let (phase, _elapsed_ms) = active_phase_status().expect("active after \"before\"");
        assert_eq!(phase, "boundary_phase");

        record_phase_sample(&recorder, "boundary_phase", "after");
        assert!(active_phase_status().is_none(), "cleared after \"after\"");
    }

    #[test]
    fn record_phase_sample_point_boundary_never_touches_active_phase() {
        let _guard = serial_guard();
        reset_active_phase_for_test();
        let recorder = PerformanceRecorder::enabled_for_test();
        assert!(active_phase_status().is_none());
        record_phase_sample(&recorder, "idle", "point");
        assert!(active_phase_status().is_none());
    }

    #[test]
    fn live_status_reports_disabled_shape_when_tracking_is_disabled() {
        let recorder = PerformanceRecorder::default();
        let status = live_status(&recorder);
        assert!(!status.enabled);
        assert!(status.process.is_none());
        assert!(status.heap.is_none());
        assert!(status.active_phase.is_none());
        assert!(status.recent_samples.is_empty());
        assert!(status.database_footprints.is_empty());
    }

    #[test]
    fn live_status_reports_active_phase_progress_and_footprints_when_enabled() {
        let _guard = serial_guard();
        reset_active_phase_for_test();
        let recorder = PerformanceRecorder::enabled_for_test();

        record_phase_sample(&recorder, "live_status_phase", "before");
        record_database_footprint(
            &recorder,
            "history_store",
            DatabaseFootprint {
                db_bytes: Some(1_024),
                wal_bytes: None,
                volume_free_bytes: Some(2_048),
                volume_total_bytes: Some(4_096),
            },
        );
        run_sampler_to_completion(
            &recorder,
            "live_status_phase",
            Duration::from_millis(2),
            Box::new(|| PhaseProgress {
                done: Some(3),
                total: Some(9),
            }),
            5,
        );

        let status = live_status(&recorder);
        assert!(status.enabled);
        assert!(status.process.is_some());
        assert!(status.heap.is_some());
        assert_eq!(status.active_phase.as_deref(), Some("live_status_phase"));
        assert_eq!(status.progress_done, Some(3));
        assert_eq!(status.progress_total, Some(9));
        assert!(!status.recent_samples.is_empty());
        assert_eq!(
            status
                .database_footprints
                .iter()
                .find(|entry| entry.connection == "history_store")
                .map(|entry| entry.footprint.db_bytes),
            Some(Some(1_024))
        );

        record_phase_sample(&recorder, "live_status_phase", "after");
        assert!(live_status(&recorder).active_phase.is_none());
    }

    #[test]
    #[ignore = "performance probe; run with `cargo test --release --lib \
                probe_phase_sampler_does_not_perturb_measured_work -- --ignored --nocapture` \
                (filtering to this exact test name keeps other concurrently-running \
                tests' scheduling noise out of the timed loop)"]
    fn probe_phase_sampler_does_not_perturb_measured_work() {
        // Answers issue #163's "sampling must not perturb what it measures"
        // requirement directly: times the same synthetic CPU/allocation
        // workload (standing in for a real scan/hydrate phase's work) with a
        // 500ms-interval `PhaseSampler` running concurrently vs. not.
        fn synthetic_phase_work() -> std::time::Duration {
            let started = std::time::Instant::now();
            let mut total: u64 = 0;
            for _ in 0..20 {
                let mut buffer = vec![0_u8; 64 * 1024];
                for (i, byte) in buffer.iter_mut().enumerate() {
                    *byte = (i % 256) as u8;
                }
                total = total.wrapping_add(buffer.iter().map(|b| *b as u64).sum());
                std::thread::sleep(Duration::from_millis(100));
            }
            std::hint::black_box(total);
            started.elapsed()
        }

        let without_sampler = synthetic_phase_work();

        let recorder = PerformanceRecorder::enabled_for_test();
        let counter = Arc::new(AtomicU64::new(0));
        let progress_counter = Arc::clone(&counter);
        let sampler = PhaseSampler::start(
            &recorder,
            "probe_phase",
            Duration::from_millis(500),
            Box::new(move || PhaseProgress {
                done: Some(progress_counter.load(Ordering::Relaxed)),
                total: None,
            }),
        )
        .expect("sampler starts when tracking is enabled");
        let with_sampler = synthetic_phase_work();
        sampler.stop();

        let without_ms = without_sampler.as_secs_f64() * 1_000.0;
        let with_ms = with_sampler.as_secs_f64() * 1_000.0;
        let delta_percent = 100.0 * (with_ms - without_ms) / without_ms.max(0.001);
        eprintln!(
            "phase sampler perturbation over a ~2s synthetic phase:\n\
             \x20 without sampler: {without_ms:.1}ms\n\
             \x20 with sampler:    {with_ms:.1}ms\n\
             \x20 delta:           {delta_percent:+.1}%\n\
             \x20 samples emitted: {}",
            recent_phase_samples().len(),
        );
    }
}
