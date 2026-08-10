//! Measurement harness for the streaming-hydration memory question:
//!
//!     After `HistoryStore::stream_sessions` hydrates a realistic corpus, is
//!     the allocator's LIVE heap byte count close to the RSS growth, or far
//!     below it? And specifically: does `QuotaPointsIndex::update_session`
//!     (the other retaining call in `AppState::apply_loaded_session`,
//!     alongside the `ResidentSession` insert) account for a meaningful share
//!     of the gap once a realistic, heavy-tailed rate-limit-point corpus is
//!     used?
//!
//! This does not change or exercise any production code path other than
//! what `AppState::apply_loaded_session` already does (`src-tauri/src/store.rs`
//! ~line 977): for each streamed session, call `QuotaPointsIndex::update_session`
//! and insert `Arc::new(ResidentSession::of(&session))` into a map, then drop
//! the full `Session`. It never constructs `AppState` itself (needs a Tauri
//! handle) and never touches the user's real ledger — the store lives
//! entirely under a disposable temp directory this binary creates and
//! deletes.
//!
//! To isolate each retainer's cost, the corpus is populated once and then
//! streamed three separate times against fresh `ResidentSession`/
//! `QuotaPointsIndex` structures: resident-only, quota-only, and combined
//! (matching real `apply_loaded_session`).
//!
//! Run in release mode, reusing the already-built target dir:
//!
//!   $env:CARGO_TARGET_DIR = "D:\projects\agent-odometer\src-tauri\target"
//!   cargo run --release --example hydration_memory -- [session_count] [avg_snapshot_bytes]
//!
//! Defaults: 1200 sessions, 467000 average `session_json` bytes (turns +
//! rate-limit history combined) — matching a v0.8.12 field recording (4614
//! sessions, 467 KB average) that real test corpora (~31 KB/session) never
//! reproduced.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use dashmap::DashMap;

use odometer_lib::history_store::HistoryStore;
use odometer_lib::memory::{
    configure_heap_tracking, heap_sample, sample_process_memory, HeapSample, ProcessMemorySample,
};
use odometer_lib::model::{
    storage_id_for_session, CategoryMetric, OptimizationFinding, RateLimitSnapshotPoint,
    RateLimitWindow, ResidentSession, Session, SourceAvailability, TaskCategory, TokenHistoryPoint,
    TokenTotals, ToolKind, ToolMetrics, ToolObservation, ToolOrigin, ToolOutcome,
    TurnClassification, TurnInfo, TurnStatus,
};
use odometer_lib::provider::{claude_code_provider_id, codex_provider_id};
use odometer_lib::quota::QuotaPointsIndex;

/// Fixed per the task: C: has 69 GB free, D: (this worktree's drive) has
/// only 16 GB — a corpus this size belongs on C:.
const STORE_DIR: &str = r"C:\Users\ekalb\AppData\Local\Temp\claude\D--projects-agent-odometer\66b553b4-6ebb-4005-8f01-80a97cdd7201\scratchpad\heapprobe";

const BATCH_SIZE: usize = 50;
const MEAN_TOLERANCE: f64 = 0.20;
const MAX_GENERATION_ATTEMPTS: u32 = 3;

/// Field count from the v0.8.12 recording quoted in the original task
/// (before RSS 25.8 MB, after RSS 2864.9 MB) — the number this run's
/// extrapolation is compared against.
const FIELD_RSS_GROWTH_MB: f64 = 2864.9 - 25.8;
/// Field rate-limit point total: 2,609 real Codex transcripts, 3,634,112
/// points, mean 1,393/session — supplied by the coordinator, not derived
/// here.
const FIELD_TOTAL_POINTS: f64 = 3_634_112.0;

/// (percentile_rank, point_count) anchors from the coordinator's field
/// measurement. Interpolated in log-space between anchors so the heavy tail
/// (p50=55 -> max=22395) is reproduced rather than smoothed into a uniform
/// spread.
const RATE_LIMIT_PERCENTILE_ANCHORS: &[(f64, f64)] = &[
    (0.00, 0.0),
    (0.50, 55.0),
    (0.75, 796.0),
    (0.90, 4173.0),
    (0.95, 8259.0),
    (0.99, 16626.0),
    (1.00, 22395.0),
];

fn main() -> anyhow::Result<()> {
    // Step 1: heap accounting on before any allocation this binary controls.
    configure_heap_tracking(true);
    let sample_process_start = sample_process_memory();
    let heap_process_start = heap_sample();

    let mut args = std::env::args().skip(1);
    let session_count: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(1200);
    let avg_snapshot_bytes: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(467_000);

    println!(
        "hydration_memory: session_count={session_count} avg_snapshot_bytes={avg_snapshot_bytes}"
    );
    println!("store dir: {STORE_DIR}");

    let store_dir = PathBuf::from(STORE_DIR);
    let db_path = store_dir.join("history-v1.sqlite3");

    // Step 2/3: brand-new, disposable store; populate through the store's
    // own public write path so the on-disk shape matches what hydration
    // actually reads back. Rate-limit point counts follow the field's
    // heavy-tailed distribution; turn/message bulk is shrunk per-session to
    // compensate so the corpus mean stays near avg_snapshot_bytes rather
    // than growing on top of it.
    let CorpusResult {
        store,
        sessions: actual_sessions,
        total_bytes: actual_total_bytes,
        mean_bytes: actual_mean_bytes,
        total_points,
        point_counts,
        bytes_per_turn,
        bytes_per_point,
    } = build_corpus(&store_dir, &db_path, session_count, avg_snapshot_bytes)?;

    println!(
        "generated corpus: {actual_sessions} sessions, total session_json bytes = {} ({:.1} MB), \
         mean = {:.0} bytes/session (bytes_per_turn~{bytes_per_turn:.0}, bytes_per_point~{bytes_per_point:.0})",
        commas(actual_total_bytes),
        actual_total_bytes as f64 / 1_048_576.0,
        actual_mean_bytes
    );
    let deviation =
        (actual_mean_bytes - avg_snapshot_bytes as f64).abs() / avg_snapshot_bytes as f64;
    println!(
        "mean deviation from {avg_snapshot_bytes} byte target: {:.1}% (tolerance {:.0}%){}",
        deviation * 100.0,
        MEAN_TOLERANCE * 100.0,
        if deviation > MEAN_TOLERANCE {
            " -- OUTSIDE TOLERANCE, see warning above; proceeding anyway since the point-count \
             tail is the thing being measured"
        } else {
            ""
        }
    );

    let mut sorted_points = point_counts.clone();
    sorted_points.sort_unstable();
    let point_mean = if !sorted_points.is_empty() {
        total_points as f64 / sorted_points.len() as f64
    } else {
        0.0
    };
    println!(
        "rate-limit points: total={} mean={:.0} p50={} p75={} p90={} p95={} p99={} max={} \
         (target: mean 1393, p50 55, p75 796, p90 4173, p95 8259, p99 16626, max 22395)",
        commas(total_points),
        point_mean,
        percentile(&sorted_points, 0.50),
        percentile(&sorted_points, 0.75),
        percentile(&sorted_points, 0.90),
        percentile(&sorted_points, 0.95),
        percentile(&sorted_points, 0.99),
        sorted_points.last().copied().unwrap_or(0),
    );

    println!();
    println!(
        "process_start: rss={} private_bytes={} heap_bytes={}",
        mb(sample_process_start.rss_bytes),
        opt_commas(sample_process_start.private_bytes),
        opt_commas(heap_process_start.current_bytes),
    );

    // Steps 5-7, run three times over the SAME populated store to isolate
    // each retainer AppState::apply_loaded_session exercises:
    //   1. ResidentSession insert only
    //   2. QuotaPointsIndex::update_session only
    //   3. Both together (matches real apply_loaded_session)
    let mut pass_results = Vec::new();
    for config in [
        HydrationConfig::ResidentOnly,
        HydrationConfig::QuotaOnly,
        HydrationConfig::Combined,
    ] {
        println!();
        println!("running pass: {}", config.label());
        let result = run_hydration_pass(&store, config)?;
        pass_results.push(result);
    }

    drop(store);
    let _ = std::fs::remove_dir_all(&store_dir);

    println!();
    for pass in &pass_results {
        println!("=== {} ===", pass.config.label());
        print_table(&[
            ("baseline", pass.baseline_process, pass.baseline_heap),
            ("after_hydration", pass.after_process, pass.after_heap),
            ("after_drop", pass.drop_process, pass.drop_heap),
        ]);
        println!(
            "hydration stats: sessions={} bytes_deserialized={} ({:.1} MB)",
            pass.stats_sessions,
            commas(pass.stats_bytes),
            pass.stats_bytes as f64 / 1_048_576.0,
        );
        print_deltas(
            "after_hydration",
            pass.baseline_process,
            pass.baseline_heap,
            pass.after_process,
            pass.after_heap,
        );
        print_deltas(
            "after_drop",
            pass.baseline_process,
            pass.baseline_heap,
            pass.drop_process,
            pass.drop_heap,
        );
        println!();
    }

    println!(
        "=== per-point attribution (total_points = {}) ===",
        commas(total_points)
    );
    println!(
        "{:<14} {:>18} {:>16} {:>18} {:>16}",
        "config", "heap_delta_bytes", "bytes/point", "rss_delta_bytes", "bytes/point"
    );
    for pass in &pass_results {
        let heap_delta = signed_delta(
            pass.baseline_heap.current_bytes,
            pass.after_heap.current_bytes,
        );
        let rss_delta = signed_delta(
            pass.baseline_process.rss_bytes,
            pass.after_process.rss_bytes,
        );
        println!(
            "{:<14} {:>18} {:>16} {:>18} {:>16}",
            pass.config.label(),
            opt_signed_commas(heap_delta),
            fmt_per_point(heap_delta, total_points),
            opt_signed_commas(rss_delta),
            fmt_per_point(rss_delta, total_points),
        );
    }
    println!(
        "note: quota_only/combined also retain one QuotaAccountInfo entry per session \
         (credits_unlimited/credits_balance are set on every synthetic session), a small \
         roughly-constant per-session cost independent of point count — not attributable to \
         points specifically, and the same across quota_only and combined so it does not bias \
         the resident_only vs quota_only comparison."
    );

    println!();
    println!("=== extrapolation to field scale ===");
    if let Some(combined) = pass_results
        .iter()
        .find(|pass| pass.config == HydrationConfig::Combined)
    {
        if let Some(heap_delta) = signed_delta(
            combined.baseline_heap.current_bytes,
            combined.after_heap.current_bytes,
        ) {
            if total_points > 0 {
                let per_point_heap = heap_delta as f64 / total_points as f64;
                let extrapolated_mb = per_point_heap * FIELD_TOTAL_POINTS / 1_048_576.0;
                println!(
                    "combined per-point live heap = {per_point_heap:.2} bytes/point x {} field points \
                     = {extrapolated_mb:.1} MB extrapolated",
                    FIELD_TOTAL_POINTS as u64
                );
                println!(
                    "field measured RSS growth during hydration (v0.8.12 recording, before->after): \
                     +{FIELD_RSS_GROWTH_MB:.1} MB -- extrapolated combined-retainer live heap is \
                     {:.1}% of that",
                    if FIELD_RSS_GROWTH_MB != 0.0 {
                        extrapolated_mb / FIELD_RSS_GROWTH_MB * 100.0
                    } else {
                        0.0
                    }
                );
            }
        } else {
            println!("combined pass: heap tracking sample unavailable, cannot extrapolate");
        }
    }

    println!();
    println!(
        "note: heap_peak_bytes in each pass's table is peak since configure_heap_tracking(true) \
         at process start (memory::reset_heap_peak_to_current is pub(crate) and not reachable \
         from an example binary), not peak within a single pass."
    );

    Ok(())
}

// ---------------------------------------------------------------------
// Hydration passes
// ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum HydrationConfig {
    ResidentOnly,
    QuotaOnly,
    Combined,
}

impl HydrationConfig {
    fn label(self) -> &'static str {
        match self {
            HydrationConfig::ResidentOnly => "resident_only",
            HydrationConfig::QuotaOnly => "quota_only",
            HydrationConfig::Combined => "combined",
        }
    }
}

struct PassResult {
    config: HydrationConfig,
    baseline_process: ProcessMemorySample,
    baseline_heap: HeapSample,
    after_process: ProcessMemorySample,
    after_heap: HeapSample,
    drop_process: ProcessMemorySample,
    drop_heap: HeapSample,
    stats_sessions: usize,
    stats_bytes: u64,
}

/// Streams the whole store once under `config`, mirroring exactly the
/// subset of `AppState::apply_loaded_session` that `config` names — never
/// more. Fresh `DashMap`/`QuotaPointsIndex` each call; both are dropped
/// before the post-drop sample regardless of which one `config` actually
/// used, so drop cost is symmetric across passes.
fn run_hydration_pass(store: &HistoryStore, config: HydrationConfig) -> anyhow::Result<PassResult> {
    let baseline_process = sample_process_memory();
    let baseline_heap = heap_sample();

    let resident: DashMap<String, Arc<ResidentSession>> = DashMap::new();
    let quota_points_index = QuotaPointsIndex::new();

    let stats = store.stream_sessions(|stored| match config {
        HydrationConfig::ResidentOnly => {
            resident.insert(
                stored.key.clone(),
                Arc::new(ResidentSession::of(&stored.session)),
            );
        }
        HydrationConfig::QuotaOnly => {
            quota_points_index.update_session(&stored.key, &stored.session);
        }
        HydrationConfig::Combined => {
            // Same order as AppState::apply_loaded_session (store.rs ~977).
            quota_points_index.update_session(&stored.key, &stored.session);
            resident.insert(
                stored.key.clone(),
                Arc::new(ResidentSession::of(&stored.session)),
            );
        }
    })?;

    let after_process = sample_process_memory();
    let after_heap = heap_sample();

    drop(resident);
    drop(quota_points_index);

    let drop_process = sample_process_memory();
    let drop_heap = heap_sample();

    Ok(PassResult {
        config,
        baseline_process,
        baseline_heap,
        after_process,
        after_heap,
        drop_process,
        drop_heap,
        stats_sessions: stats.sessions,
        stats_bytes: stats.bytes,
    })
}

// ---------------------------------------------------------------------
// Corpus generation
// ---------------------------------------------------------------------

struct CorpusResult {
    store: HistoryStore,
    sessions: u64,
    total_bytes: u64,
    mean_bytes: f64,
    total_points: u64,
    point_counts: Vec<usize>,
    bytes_per_turn: f64,
    bytes_per_point: f64,
}

/// Maps a uniform-random rank in `[0, 1]` to a rate-limit point count via
/// log-space interpolation between `RATE_LIMIT_PERCENTILE_ANCHORS`. Since
/// each synthetic session draws an independent uniform rank, the resulting
/// empirical percentiles across many sessions approximate the anchors —
/// verified after generation by computing real percentiles from the
/// generated `point_counts` (see `main`).
fn rate_limit_point_count_for_rank(rank: f64) -> usize {
    let rank = rank.clamp(0.0, 1.0);
    for pair in RATE_LIMIT_PERCENTILE_ANCHORS.windows(2) {
        let (r0, v0) = pair[0];
        let (r1, v1) = pair[1];
        if rank <= r1 {
            let t = if (r1 - r0).abs() < f64::EPSILON {
                0.0
            } else {
                ((rank - r0) / (r1 - r0)).clamp(0.0, 1.0)
            };
            let log_v0 = (v0 + 1.0).ln();
            let log_v1 = (v1 + 1.0).ln();
            let log_v = log_v0 + t * (log_v1 - log_v0);
            return (log_v.exp() - 1.0).round().max(0.0) as usize;
        }
    }
    RATE_LIMIT_PERCENTILE_ANCHORS
        .last()
        .map(|(_, v)| v.round() as usize)
        .unwrap_or(0)
}

fn percentile(sorted: &[usize], p: f64) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((p * (sorted.len() - 1) as f64).round() as usize).min(sorted.len() - 1);
    sorted[idx]
}

/// Builds and populates the store, retrying with a rescaled target if the
/// realized mean session_json size drifts too far from the requested one.
/// Returns the open store (left open for the hydration passes) plus the
/// verified actual sessions/total-bytes/mean-bytes and the generated
/// rate-limit point-count distribution.
fn build_corpus(
    store_dir: &Path,
    db_path: &Path,
    session_count: usize,
    avg_snapshot_bytes: usize,
) -> anyhow::Result<CorpusResult> {
    let mut effective_avg = avg_snapshot_bytes;
    let mut rng = Rng::new(0x9E3779B97F4A7C15 ^ session_count as u64 ^ avg_snapshot_bytes as u64);

    for attempt in 1..=MAX_GENERATION_ATTEMPTS {
        let _ = std::fs::remove_dir_all(store_dir);
        std::fs::create_dir_all(store_dir)?;
        let store = HistoryStore::open(db_path)?;

        // Calibrate bytes/turn from a zero-rate-limit-point session, so the
        // point calibration below and this one measure disjoint costs.
        let calibration_turns = 20;
        let calibration_session = build_session(usize::MAX, calibration_turns, 0, &mut rng);
        let calibration_bytes = serde_json::to_vec(&calibration_session)?.len();
        let bytes_per_turn = (calibration_bytes as f64 / calibration_turns as f64).max(200.0);
        drop(calibration_session);

        // Calibrate bytes/point from the marginal delta between a zero-point
        // and a many-point session at the SAME turn count, so turn content
        // cannot contaminate the per-point figure.
        let point_calibration_turns = 5;
        let point_sample_count = 2000usize;
        let zero_points = build_session(usize::MAX - 1, point_calibration_turns, 0, &mut rng);
        let zero_points_bytes = serde_json::to_vec(&zero_points)?.len();
        drop(zero_points);
        let many_points = build_session(
            usize::MAX - 1,
            point_calibration_turns,
            point_sample_count,
            &mut rng,
        );
        let many_points_bytes = serde_json::to_vec(&many_points)?.len();
        drop(many_points);
        let bytes_per_point = ((many_points_bytes.saturating_sub(zero_points_bytes)) as f64
            / point_sample_count as f64)
            .max(20.0);

        let generation = store.begin_scan()?;
        let mut batch: Vec<(PathBuf, Session)> = Vec::with_capacity(BATCH_SIZE);
        let mut total_points: u64 = 0;
        let mut point_counts: Vec<usize> = Vec::with_capacity(session_count);
        for index in 0..session_count {
            let factor = rng.range_f64(0.3, 3.0);
            let target_bytes = (effective_avg as f64 * factor).max(1000.0);
            let rank = rng.next_f64();
            let point_count = rate_limit_point_count_for_rank(rank);
            let rate_limit_bytes_estimate = point_count as f64 * bytes_per_point;
            // Rate-limit history displaces turn/message content rather than
            // adding to it — shrink the turn budget by what points already
            // cost, down to a 3-turn floor.
            let remaining_budget =
                (target_bytes - rate_limit_bytes_estimate).max(3.0 * bytes_per_turn);
            let turn_count =
                ((remaining_budget / bytes_per_turn).round() as i64).clamp(3, 800) as usize;
            let session = build_session(index, turn_count, point_count, &mut rng);
            total_points += point_count as u64;
            point_counts.push(point_count);
            let path = PathBuf::from(format!("C:/synthetic-sessions/session-{index:06}.jsonl"));
            batch.push((path, session));
            if batch.len() == BATCH_SIZE || index + 1 == session_count {
                let items: Vec<(&Path, &Session, i64)> = batch
                    .iter()
                    .map(|(path, session)| (path.as_path(), session, generation))
                    .collect();
                store.observe_bulk_batch(&items)?;
                batch.clear();
            }
        }

        let (count, total_bytes, mean_bytes) = verify_actual_sizes(db_path)?;
        let deviation = (mean_bytes - avg_snapshot_bytes as f64).abs() / avg_snapshot_bytes as f64;
        println!(
            "generation attempt {attempt}: {count} sessions, mean {mean_bytes:.0} bytes \
             (target {avg_snapshot_bytes}, deviation {:.1}%), bytes_per_turn~{bytes_per_turn:.0} \
             bytes_per_point~{bytes_per_point:.0}",
            deviation * 100.0
        );
        if deviation <= MEAN_TOLERANCE || attempt == MAX_GENERATION_ATTEMPTS {
            if deviation > MEAN_TOLERANCE {
                println!(
                    "WARNING: mean is still {:.1}% off the {avg_snapshot_bytes} byte target after \
                     {attempt} attempts. This is expected once the heavy-tailed rate-limit point \
                     distribution is reproduced faithfully: a handful of sessions at/near the \
                     22,395-point max cost several MB of session_json from points alone, which \
                     cannot be offset by shrinking turn content below the 3-turn floor. Proceeding \
                     with this corpus and reporting the real deviation rather than aborting or \
                     flattening the tail to force convergence.",
                    deviation * 100.0
                );
            }
            return Ok(CorpusResult {
                store,
                sessions: count,
                total_bytes,
                mean_bytes,
                total_points,
                point_counts,
                bytes_per_turn,
                bytes_per_point,
            });
        }
        effective_avg =
            ((effective_avg as f64) * (avg_snapshot_bytes as f64 / mean_bytes)) as usize;
        drop(store);
    }
    unreachable!("loop always returns by the final attempt");
}

/// Sums the exact on-disk `session_json` byte lengths via SQL aggregates —
/// deliberately never deserializes a `Session` to check this, so the check
/// itself cannot pollute the heap/RSS state the baseline sample depends on.
fn verify_actual_sizes(db_path: &Path) -> anyhow::Result<(u64, u64, f64)> {
    let connection = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let (count, total): (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(s.session_json)), 0)
         FROM durable_sessions d
         JOIN session_snapshots s
           ON s.session_key = d.session_key AND s.version = d.current_snapshot_version",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let count = count.max(0) as u64;
    let total = total.max(0) as u64;
    let mean = if count > 0 {
        total as f64 / count as f64
    } else {
        0.0
    };
    Ok((count, total, mean))
}

const WORDS: &[&str] = &[
    "implement",
    "refactor",
    "review",
    "the",
    "function",
    "test",
    "database",
    "session",
    "token",
    "budget",
    "memory",
    "allocator",
    "query",
    "index",
    "schema",
    "migration",
    "transcript",
    "provider",
    "harness",
    "context",
    "window",
    "cache",
    "fragment",
    "retain",
    "hydrate",
    "stream",
    "batch",
    "commit",
    "transaction",
    "snapshot",
    "summary",
    "turn",
    "agent",
    "tool",
    "call",
    "observation",
    "outcome",
    "duration",
    "latency",
    "throughput",
    "working",
    "directory",
    "project",
    "identity",
    "branch",
    "module",
    "struct",
    "field",
    "serialize",
    "deserialize",
    "allocate",
    "release",
    "heap",
    "stack",
    "pointer",
    "buffer",
    "chunk",
    "payload",
    "endpoint",
    "handler",
    "event",
    "listener",
    "config",
    "option",
    "default",
    "value",
    "result",
    "error",
    "warning",
    "trace",
    "debug",
    "profile",
    "build",
    "compile",
    "link",
    "runtime",
    "platform",
    "process",
    "thread",
    "lock",
    "channel",
    "async",
    "await",
    "future",
    "task",
    "queue",
    "worker",
    "pool",
    "scale",
    "compare",
    "verify",
    "reconcile",
    "archive",
    "durable",
    "ledger",
    "rollup",
    "bucket",
    "history",
    "regression",
];

const TOOL_NAMES_READ: &[&str] = &["read_file", "cat", "view_file", "open_file"];
const TOOL_NAMES_SEARCH: &[&str] = &["grep", "search_code", "glob", "find_symbol"];
const TOOL_NAMES_MUTATION: &[&str] = &["edit_file", "write_file", "apply_patch", "rename_symbol"];
const TOOL_NAMES_COMMAND: &[&str] = &["run_shell", "cargo_test", "npm_run", "cargo_build"];
const TOOL_NAMES_OTHER: &[&str] = &["update_todo", "spawn_subagent", "web_search", "list_dir"];

const WORKING_DIRECTORIES: &[&str] = &[
    "C:/projects/alpha-service",
    "C:/projects/beta-cli",
    "C:/projects/gamma-web",
    "C:/projects/delta-infra",
    "C:/projects/epsilon-lib",
    "C:/projects/zeta-app",
];

fn build_session(
    index: usize,
    turn_count: usize,
    rate_limit_point_count: usize,
    rng: &mut Rng,
) -> Session {
    let harness = if index.is_multiple_of(2) {
        codex_provider_id()
    } else {
        claude_code_provider_id()
    };
    let provider_session_id = format!("synthetic-{index:06}-{:016x}", rng.next_u64());
    let storage_id = storage_id_for_session(&harness, &provider_session_id);
    let model_names = ["claude-sonnet-5", "claude-opus-4-8", "gpt-5-codex"];
    let model_name = model_names[rng.next_range(0, model_names.len())].to_string();
    let working_directory =
        WORKING_DIRECTORIES[rng.next_range(0, WORKING_DIRECTORIES.len())].to_string();

    let base_time = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
        + ChronoDuration::seconds(index as i64 * 733);

    let mut turns = Vec::with_capacity(turn_count);
    let mut tool_observations = Vec::new();
    let mut tokens_history = Vec::with_capacity(turn_count);
    let mut tokens_total = TokenTotals::default();
    let mut tool_metrics = ToolMetrics::default();
    let mut category_totals: BTreeMap<TaskCategory, CategoryMetric> = BTreeMap::new();
    let mut first_user_message: Option<String> = None;
    let mut cursor: DateTime<Utc> = base_time;

    for turn_index in 0..turn_count {
        cursor += ChronoDuration::seconds(rng.next_range(5, 240) as i64);
        let user_message_len = rng.next_range(80, 420);
        let user_message = build_paragraph(rng, user_message_len);
        let last_agent_message_len = rng.next_range(150, 900);
        let last_agent_message = build_paragraph(rng, last_agent_message_len);
        if first_user_message.is_none() {
            first_user_message = Some(user_message.clone());
        }

        let input = rng.next_range(200, 6000) as u64;
        let output = rng.next_range(50, 1800) as u64;
        let reasoning = if rng.gen_bool(0.4) {
            rng.next_range(0, 800) as u64
        } else {
            0
        };
        let cached = if rng.gen_bool(0.5) {
            rng.next_range(0, (input as usize).max(1)) as u64
        } else {
            0
        };
        let delta = TokenTotals {
            input_tokens: input,
            cached_input_tokens: cached,
            cache_creation_input_tokens: if rng.gen_bool(0.2) {
                rng.next_range(0, 500) as u64
            } else {
                0
            },
            output_tokens: output,
            reasoning_output_tokens: reasoning,
            total_tokens: input + output + reasoning,
        };
        tokens_total += &delta;

        tokens_history.push(TokenHistoryPoint {
            timestamp: cursor,
            model: Some(model_name.clone()),
            service_tier: Some("standard".to_string()),
            request_input_tokens: Some(input),
            total_tokens: tokens_total.total_tokens,
            delta: delta.clone(),
        });

        let tool_call_count = rng.next_range(0, 5);
        let mut turn_tool_metrics = ToolMetrics::default();
        for tool_index in 0..tool_call_count {
            let kind = pick_tool_kind(rng);
            let outcome = if rng.gen_bool(0.85) {
                ToolOutcome::Success
            } else {
                ToolOutcome::Failure
            };
            let name = pick_tool_name(kind, rng);
            let observation = ToolObservation {
                call_id: format!(
                    "call-{index}-{turn_index}-{tool_index}-{:08x}",
                    rng.next_u64() as u32
                ),
                turn_id: Some(format!("turn-{index}-{turn_index}")),
                harness: harness.clone(),
                model: Some(model_name.clone()),
                timestamp: cursor,
                kind,
                name: name.clone(),
                providers: Vec::new(),
                effective_tools: vec![name],
                target: Some(format!("hash-{:016x}", rng.next_u64())),
                resource_id: Some(format!("res-{:016x}", rng.next_u64())),
                origin: ToolOrigin::Core,
                shell_family: None,
                language: None,
                outcome,
                duration_ms: Some(rng.next_range(5, 4000) as u64),
                output_bytes: rng.next_range(0, 20_000) as u64,
            };
            accumulate_tool_metrics(&mut turn_tool_metrics, &observation);
            tool_observations.push(observation);
        }
        tool_metrics.add_assign(&turn_tool_metrics);

        let category = pick_category(rng);
        let entry = category_totals.entry(category).or_default();
        entry.turns += 1;
        entry.tokens += &delta;
        entry.tool_calls += tool_call_count as u64;

        turns.push(TurnInfo {
            turn_id: format!("turn-{index}-{turn_index}"),
            index: turn_index as u32 + 1,
            model: Some(model_name.clone()),
            reasoning_effort: if rng.gen_bool(0.3) {
                Some("medium".to_string())
            } else {
                None
            },
            collaboration_mode: None,
            service_tier: Some("standard".to_string()),
            status: TurnStatus::Completed,
            abort_reason: None,
            started_at: Some(cursor),
            completed_at: Some(
                cursor + ChronoDuration::milliseconds(rng.next_range(200, 60_000) as i64),
            ),
            duration_ms: Some(rng.next_range(200, 60_000) as u64),
            time_to_first_token_ms: Some(rng.next_range(50, 3000) as u64),
            user_message: Some(user_message),
            last_agent_message: Some(last_agent_message),
            tokens: delta,
            tool_metrics: turn_tool_metrics,
            classification: TurnClassification {
                version: 1,
                category,
                confidence: rng.next_f64() as f32,
                signals: vec!["synthetic".to_string()],
            },
        });
    }

    // Rate-limit history: shape matches parser.rs:791 (turn_id/limit_id are
    // Option<String>, primary is effectively always present, secondary is a
    // second window roughly 70% of the time — 5h + weekly dual-window plans
    // are the common Codex case). Count is drawn from the field-measured
    // heavy-tailed distribution, not scaled with turn_count.
    let rate_limits_history =
        build_rate_limit_points(&turns, rate_limit_point_count, base_time, rng);
    let last_event_at = rate_limits_history
        .last()
        .map(|point| point.timestamp)
        .unwrap_or(cursor)
        .max(cursor);

    let optimization_findings: Vec<OptimizationFinding> = (0..rng.next_range(0, 3))
        .map(|finding_index| OptimizationFinding {
            version: 1,
            rule_id: "repeated-read".to_string(),
            severity: if rng.gen_bool(0.5) {
                "warning".to_string()
            } else {
                "info".to_string()
            },
            confidence: "medium".to_string(),
            turn_id: Some(format!("turn-{index}-{finding_index}")),
            model: Some(model_name.clone()),
            timestamp: Some(cursor),
            evidence: build_paragraph(rng, 60),
            remediation: build_paragraph(rng, 60),
            occurrences: rng.next_range(1, 5) as u64,
            avoidable_calls: rng.next_range(0, 3) as u64,
        })
        .collect();

    let mut tokens_by_model = HashMap::new();
    tokens_by_model.insert(model_name.clone(), tokens_total.clone());
    let mut tool_metrics_by_model = BTreeMap::new();
    tool_metrics_by_model.insert(model_name.clone(), tool_metrics.clone());

    Session {
        id: provider_session_id.clone(),
        storage_id,
        harness,
        thread_name: Some(build_paragraph(rng, 40)),
        forked_from_id: None,
        parent_thread_id: None,
        agent_path: None,
        agent_nickname: None,
        file_path: format!("C:/synthetic-sessions/session-{index:06}.jsonl"),
        source_availability: SourceAvailability::Present,
        archived: true,
        started_at: base_time,
        last_event_at,
        working_directory: Some(working_directory.clone()),
        originator: Some("cli".to_string()),
        source: Some("transcript".to_string()),
        subagent_id_is_path_fallback: false,
        history_mode: None,
        memory_mode: None,
        cli_version: Some("1.2.3".to_string()),
        model_provider: Some("anthropic".to_string()),
        model: Some(model_name.clone()),
        service_tier: Some("standard".to_string()),
        plan_type: Some("pro".to_string()),
        credits_unlimited: Some(false),
        credits_balance: Some(rng.range_f64(0.0, 100.0)),
        context_window: Some(200_000),
        latest_context_tokens: Some(rng.next_range(1000, 190_000) as u64),
        total_turns: turn_count as u32,
        first_user_message,
        tokens_total,
        tokens_by_model,
        tokens_history,
        rate_limits_history,
        turns,
        tool_observations,
        tool_metrics,
        tool_metrics_by_model,
        category_totals,
        optimization_findings,
        project_key: Some(working_directory),
        project_label: None,
        project_provenance: None,
    }
}

/// Builds `point_count` rate-limit points, cycling through `turns`' turn ids
/// (matching how the real parser stamps `turn_id` from whatever turn was
/// active when the `rate_limits` payload arrived) and spacing timestamps a
/// few seconds to a couple minutes apart, matching frequent quota polling.
fn build_rate_limit_points(
    turns: &[TurnInfo],
    point_count: usize,
    base_time: DateTime<Utc>,
    rng: &mut Rng,
) -> Vec<RateLimitSnapshotPoint> {
    let mut points = Vec::with_capacity(point_count);
    let mut cursor = base_time;
    for point_index in 0..point_count {
        cursor += ChronoDuration::seconds(rng.next_range(1, 90) as i64);
        let turn_id = if turns.is_empty() {
            None
        } else {
            Some(turns[point_index % turns.len()].turn_id.clone())
        };
        let primary = Some(RateLimitWindow {
            used_percent: rng.range_f64(0.0, 100.0),
            window_minutes: Some(300),
            resets_at: Some(cursor + ChronoDuration::minutes(300)),
        });
        let secondary = if rng.gen_bool(0.7) {
            Some(RateLimitWindow {
                used_percent: rng.range_f64(0.0, 100.0),
                window_minutes: Some(10_080),
                resets_at: Some(cursor + ChronoDuration::minutes(10_080)),
            })
        } else {
            None
        };
        points.push(RateLimitSnapshotPoint {
            timestamp: cursor,
            turn_id,
            limit_id: Some("primary".to_string()),
            primary,
            secondary,
        });
    }
    points
}

fn accumulate_tool_metrics(metrics: &mut ToolMetrics, observation: &ToolObservation) {
    metrics.calls += 1;
    match observation.kind {
        ToolKind::Read => metrics.reads += 1,
        ToolKind::Search => metrics.searches += 1,
        ToolKind::Mutation => metrics.mutations += 1,
        ToolKind::Command => metrics.commands += 1,
        ToolKind::Other => metrics.other += 1,
    }
    match observation.outcome {
        ToolOutcome::Success => metrics.successes += 1,
        ToolOutcome::Failure => metrics.failures += 1,
        ToolOutcome::Pending | ToolOutcome::Unknown => metrics.unknown += 1,
    }
    match observation.origin {
        ToolOrigin::Core => metrics.core_origin_calls += 1,
        ToolOrigin::Mcp => metrics.mcp_origin_calls += 1,
        ToolOrigin::Provider => metrics.provider_origin_calls += 1,
        ToolOrigin::Unknown => metrics.unknown_origin_calls += 1,
    }
    metrics.duration_ms += observation.duration_ms.unwrap_or(0);
    metrics.output_bytes += observation.output_bytes;
}

fn pick_tool_kind(rng: &mut Rng) -> ToolKind {
    match rng.next_range(0, 5) {
        0 => ToolKind::Read,
        1 => ToolKind::Search,
        2 => ToolKind::Mutation,
        3 => ToolKind::Command,
        _ => ToolKind::Other,
    }
}

fn pick_tool_name(kind: ToolKind, rng: &mut Rng) -> String {
    let pool = match kind {
        ToolKind::Read => TOOL_NAMES_READ,
        ToolKind::Search => TOOL_NAMES_SEARCH,
        ToolKind::Mutation => TOOL_NAMES_MUTATION,
        ToolKind::Command => TOOL_NAMES_COMMAND,
        ToolKind::Other => TOOL_NAMES_OTHER,
    };
    pool[rng.next_range(0, pool.len())].to_string()
}

fn pick_category(rng: &mut Rng) -> TaskCategory {
    match rng.next_range(0, 7) {
        0 => TaskCategory::Planning,
        1 => TaskCategory::Exploration,
        2 => TaskCategory::Coding,
        3 => TaskCategory::Debugging,
        4 => TaskCategory::Testing,
        5 => TaskCategory::Review,
        _ => TaskCategory::Other,
    }
}

fn build_paragraph(rng: &mut Rng, target_chars: usize) -> String {
    let mut out = String::with_capacity(target_chars + 32);
    while out.len() < target_chars {
        let sentence_len = rng.next_range(6, 16);
        for word_index in 0..sentence_len {
            if word_index > 0 {
                out.push(' ');
            }
            out.push_str(WORDS[rng.next_range(0, WORDS.len())]);
        }
        out.push_str(". ");
    }
    out.truncate(target_chars);
    out
}

// ---------------------------------------------------------------------
// Deterministic PRNG (splitmix64) — no external `rand` dependency needed
// for a self-contained measurement harness.
// ---------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    fn next_range(&mut self, lo: usize, hi: usize) -> usize {
        if hi <= lo {
            return lo;
        }
        let span = (hi - lo) as u64;
        lo + (self.next_u64() % span) as usize
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range_f64(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.next_f64() * (hi - lo)
    }

    fn gen_bool(&mut self, probability: f64) -> bool {
        self.next_f64() < probability
    }
}

// ---------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------

fn print_table(rows: &[(&str, ProcessMemorySample, HeapSample)]) {
    println!(
        "{:<26} {:>14} {:>14} {:>18} {:>18} {:>18}",
        "phase", "rss", "peak_rss", "private_bytes", "heap_bytes", "heap_peak_bytes"
    );
    for (phase, process, heap) in rows {
        println!(
            "{:<26} {:>14} {:>14} {:>18} {:>18} {:>18}",
            phase,
            mb(process.rss_bytes),
            mb(process.peak_rss_bytes),
            opt_commas(process.private_bytes),
            opt_commas(heap.current_bytes),
            opt_commas(heap.peak_bytes),
        );
    }
}

fn print_deltas(
    label: &str,
    baseline_process: ProcessMemorySample,
    baseline_heap: HeapSample,
    process: ProcessMemorySample,
    heap: HeapSample,
) {
    println!(
        "delta baseline -> {label}: rss={} private_bytes={} heap_bytes={} heap_peak_bytes={}",
        delta_mb(baseline_process.rss_bytes, process.rss_bytes),
        delta_commas(baseline_process.private_bytes, process.private_bytes),
        delta_commas(baseline_heap.current_bytes, heap.current_bytes),
        delta_commas(baseline_heap.peak_bytes, heap.peak_bytes),
    );
}

fn mb(bytes: Option<u64>) -> String {
    match bytes {
        Some(value) => format!("{:.1} MB", value as f64 / 1_048_576.0),
        None => "n/a".to_string(),
    }
}

fn opt_commas(bytes: Option<u64>) -> String {
    match bytes {
        Some(value) => commas(value),
        None => "n/a".to_string(),
    }
}

fn commas(value: u64) -> String {
    let digits = value.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && (bytes.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*byte as char);
    }
    out
}

fn delta_mb(baseline: Option<u64>, current: Option<u64>) -> String {
    match (baseline, current) {
        (Some(base), Some(cur)) => {
            let delta = cur as i64 - base as i64;
            format!("{:+.1} MB", delta as f64 / 1_048_576.0)
        }
        _ => "n/a".to_string(),
    }
}

fn delta_commas(baseline: Option<u64>, current: Option<u64>) -> String {
    match (baseline, current) {
        (Some(base), Some(cur)) => {
            let delta = cur as i64 - base as i64;
            if delta < 0 {
                format!("-{}", commas(delta.unsigned_abs()))
            } else {
                format!("+{}", commas(delta as u64))
            }
        }
        _ => "n/a".to_string(),
    }
}

fn signed_delta(baseline: Option<u64>, current: Option<u64>) -> Option<i64> {
    match (baseline, current) {
        (Some(base), Some(cur)) => Some(cur as i64 - base as i64),
        _ => None,
    }
}

fn opt_signed_commas(delta: Option<i64>) -> String {
    match delta {
        Some(value) if value < 0 => format!("-{}", commas(value.unsigned_abs())),
        Some(value) => format!("+{}", commas(value as u64)),
        None => "n/a".to_string(),
    }
}

fn fmt_per_point(delta: Option<i64>, total_points: u64) -> String {
    match delta {
        Some(value) if total_points > 0 => format!("{:.2}", value as f64 / total_points as f64),
        _ => "n/a".to_string(),
    }
}
