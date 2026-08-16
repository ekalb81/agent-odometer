//! Measurement harness for issue #182's two bulk-scan write-path fixes.
//!
//! `examples/hydration_memory.rs` measures a different phase entirely
//! (`HistoryStore::stream_sessions`, the post-scan startup hydration read)
//! and never calls `scan_all`, `AppState::reconcile_scanned_batch_if_current`,
//! or `HistoryStore::observe_bulk_batch` — it does not exercise the scan
//! write path these fixes touch. This harness does, as directly as an
//! example binary (no `AppHandle`, so `commands.rs`'s actual closure can't
//! run) can manage:
//!
//! - **Fix 1** (`commands.rs:1344`'s removed `batch.to_vec()`): isolates the
//!   exact clone call the fix deleted and times/heap-samples it directly,
//!   repeated on a realistic `SCAN_WRITE_BATCH_SIZE`-sized batch.
//! - **Fix 2** (`history_store.rs`'s `observe_bulk_batch` read-back): calls
//!   the real, current `HistoryStore::observe_bulk_batch` — the exact
//!   function the fix changed — across a batch count and corpus size
//!   matching issue #182's field recording (~4,687 sessions, ~73 batches of
//!   64, ~467 KB mean `session_json`), and reports wall time plus process/
//!   allocator memory deltas for the whole write+read-back pass. Comparing
//!   this run's numbers against the same run on the pre-fix code (revert the
//!   `history_store.rs` half of the fix, rebuild `--release`, rerun) is what
//!   gives the actual before/after for fix 2; a single run only gives
//!   "after".
//!
//! Corpus generation (`build_session` and its helpers) is a trimmed copy of
//! `hydration_memory.rs`'s generator, dropping the mean-convergence retry
//! loop and the rate-limit-point distribution (this harness only needs
//! turn-driven size control, not point-count fidelity) — so reported mean
//! bytes/session are an honest, if simplified, approximation of the field's
//! 462.7-470.7 KB figure, not a reproduction of it.
//!
//! Neither pass touches the real ledger: both open a brand-new
//! `HistoryStore` under a disposable temp directory on `C:` and delete it
//! before exiting.
//!
//! Run in release mode, reusing the already-built target dir:
//!
//!   $env:CARGO_TARGET_DIR = "D:\projects\agent-odometer\src-tauri\target"
//!   cargo run --release --example scan_write_batch_memory -- [batch_count] [avg_snapshot_bytes]
//!
//! Defaults: 73 batches of 64 (4,672 sessions, matching field cardinality
//! ~4,687) and 467,000 average bytes.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};

use odometer_lib::history_store::HistoryStore;
use odometer_lib::memory::{
    configure_heap_tracking, heap_sample, sample_process_memory, HeapSample, ProcessMemorySample,
};
use odometer_lib::model::{
    storage_id_for_session, CategoryMetric, OptimizationFinding, Session, SourceAvailability,
    TaskCategory, TokenHistoryPoint, TokenTotals, ToolKind, ToolMetrics, ToolObservation,
    ToolOrigin, ToolOutcome, TurnClassification, TurnInfo, TurnStatus,
};
use odometer_lib::provider::{claude_code_provider_id, codex_provider_id};

/// Fixed per the task: C: has ample free space, D: (this worktree's drive)
/// does not — a corpus this size belongs on C:.
const STORE_DIR: &str = r"C:\Users\ekalb\AppData\Local\Temp\claude\D--projects-agent-odometer\66b553b4-6ebb-4005-8f01-80a97cdd7201\scratchpad\scanwriteprobe";

/// Mirrors `scanner::SCAN_WRITE_BATCH_SIZE` (`pub(crate)`, not importable
/// from an example binary in a different crate).
const REAL_SCAN_BATCH_SIZE: usize = 64;
/// Field recording: ~4,687 files/scan at batch size 64 -> ~73 batches.
const DEFAULT_BATCH_COUNT: usize = 73;
const CLONE_REPS: usize = 200;
/// Field scan cardinality (issue #182), for the fix-1 extrapolation only.
const FIELD_SESSION_COUNT: f64 = 4_687.0;

fn main() -> anyhow::Result<()> {
    configure_heap_tracking(true);

    let mut args = std::env::args().skip(1);
    let batch_count: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_BATCH_COUNT);
    let avg_snapshot_bytes: usize = args
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(467_000);

    println!(
        "scan_write_batch_memory: batch_count={batch_count} (x{REAL_SCAN_BATCH_SIZE}/batch = \
         {} sessions) avg_snapshot_bytes={avg_snapshot_bytes}",
        batch_count * REAL_SCAN_BATCH_SIZE
    );

    measure_clone_cost(avg_snapshot_bytes)?;
    measure_observe_bulk_batch(batch_count, avg_snapshot_bytes)?;

    Ok(())
}

// ---------------------------------------------------------------------
// Measurement A: fix 1 -- commands.rs's removed `batch.to_vec()`
// ---------------------------------------------------------------------

fn measure_clone_cost(avg_snapshot_bytes: usize) -> anyhow::Result<()> {
    println!();
    println!("=== measurement A: commands.rs's removed batch.to_vec() (issue #182 fix 1) ===");

    let mut rng = Rng::new(0xC10E_0000_5EED_0001);
    let turn_count = calibrate_turn_count(avg_snapshot_bytes, &mut rng)?;

    let batch: Vec<(PathBuf, Session)> = (0..REAL_SCAN_BATCH_SIZE)
        .map(|index| {
            let session = build_session(index, turn_count, 0, &mut rng);
            (
                PathBuf::from(format!(
                    "C:/synthetic-sessions/clone-probe-{index:06}.jsonl"
                )),
                session,
            )
        })
        .collect();
    let batch_bytes: usize = batch
        .iter()
        .map(|(_, session)| serde_json::to_vec(session).map(|v| v.len()).unwrap_or(0))
        .sum();
    let mean_bytes = batch_bytes as f64 / batch.len() as f64;
    println!(
        "clone-probe batch: {} sessions, {} total bytes ({:.1} MB), mean {:.0} bytes/session \
         (turn content only, no rate-limit history -- see module doc)",
        batch.len(),
        commas(batch_bytes as u64),
        batch_bytes as f64 / 1_048_576.0,
        mean_bytes
    );

    let heap_before = heap_sample();
    let process_before = sample_process_memory();
    let started = Instant::now();
    let mut sink = 0usize;
    for _ in 0..CLONE_REPS {
        // Exactly commands.rs:1344's removed call, before this fix: a full
        // deep clone of the batch `on_session_batch` received, taken before
        // the durable-write lock is even acquired. `scanner.rs` drops the
        // original `batch` immediately after this call returns, so nothing
        // downstream ever needed this clone to exist.
        let cloned = batch.to_vec();
        sink = sink.wrapping_add(cloned.len());
        drop(cloned);
    }
    std::hint::black_box(sink);
    let wall = started.elapsed();
    let heap_after = heap_sample();
    let process_after = sample_process_memory();
    drop(batch);

    let per_clone_ms = wall.as_secs_f64() * 1000.0 / CLONE_REPS as f64;
    println!(
        "{CLONE_REPS} repetitions of Vec<(PathBuf, Session)>::to_vec() on a \
         {REAL_SCAN_BATCH_SIZE}-session batch:"
    );
    println!(
        "  wall time: total={:.3}s  mean_per_clone={per_clone_ms:.3}ms",
        wall.as_secs_f64()
    );
    println!(
        "  heap current delta start->end (expected ~0, every clone is dropped): {}",
        delta_commas(heap_before.current_bytes, heap_after.current_bytes)
    );
    println!(
        "  heap PEAK delta (one extra clone briefly resident alongside the original batch): {}",
        delta_commas(heap_before.peak_bytes, heap_after.peak_bytes)
    );
    println!(
        "  process RSS peak delta: {}",
        delta_mb(process_before.peak_rss_bytes, process_after.peak_rss_bytes)
    );

    let per_scan_batches = FIELD_SESSION_COUNT / REAL_SCAN_BATCH_SIZE as f64;
    println!(
        "  extrapolated to one full scan (~{per_scan_batches:.0} batches at field cardinality \
         {FIELD_SESSION_COUNT:.0} sessions / {REAL_SCAN_BATCH_SIZE} per batch): ~{:.0} ms of pure \
         clone time removed in total; each in-flight batch previously carried one avoided \
         ~{:.1} MB peak allocation",
        per_clone_ms * per_scan_batches,
        mean_bytes * REAL_SCAN_BATCH_SIZE as f64 / 1_048_576.0,
    );

    Ok(())
}

fn calibrate_turn_count(avg_snapshot_bytes: usize, rng: &mut Rng) -> anyhow::Result<usize> {
    let calibration_turns = 20;
    let calibration_session = build_session(usize::MAX, calibration_turns, 0, rng);
    let calibration_bytes = serde_json::to_vec(&calibration_session)?.len();
    let bytes_per_turn = (calibration_bytes as f64 / calibration_turns as f64).max(200.0);
    drop(calibration_session);
    Ok(((avg_snapshot_bytes as f64 / bytes_per_turn).round() as i64).clamp(3, 800) as usize)
}

// ---------------------------------------------------------------------
// Measurement B: fix 2 -- observe_bulk_batch's read-back connection reuse
// ---------------------------------------------------------------------

fn measure_observe_bulk_batch(batch_count: usize, avg_snapshot_bytes: usize) -> anyhow::Result<()> {
    println!();
    println!(
        "=== measurement B: observe_bulk_batch's read-back connection reuse (issue #182 fix 2) ==="
    );

    let store_dir = PathBuf::from(STORE_DIR);
    let _ = std::fs::remove_dir_all(&store_dir);
    std::fs::create_dir_all(&store_dir)?;
    let db_path = store_dir.join("history-v1.sqlite3");
    let store = HistoryStore::open(&db_path)?;
    let generation = store.begin_scan()?.max(1);

    let mut rng = Rng::new(0x5CA4_0002_BA7C_0003);
    let turn_count = calibrate_turn_count(avg_snapshot_bytes, &mut rng)?;

    let total_sessions = batch_count * REAL_SCAN_BATCH_SIZE;
    println!(
        "writing {total_sessions} freshly-identified sessions across {batch_count} batches of \
         {REAL_SCAN_BATCH_SIZE} via HistoryStore::observe_bulk_batch (the actual, current, \
         post-fix function) -- every batch is a fresh insert, matching a cold first scan; no \
         batch here hits the collision/displaced-session branch."
    );

    let process_before = sample_process_memory();
    let heap_before = heap_sample();
    let started = Instant::now();
    let mut written = 0usize;
    for batch_index in 0..batch_count {
        let items_owned: Vec<(PathBuf, Session)> = (0..REAL_SCAN_BATCH_SIZE)
            .map(|slot| {
                let index = batch_index * REAL_SCAN_BATCH_SIZE + slot;
                let session = build_session(index, turn_count, 0, &mut rng);
                (
                    PathBuf::from(format!("C:/synthetic-sessions/scan-probe-{index:06}.jsonl")),
                    session,
                )
            })
            .collect();
        let items: Vec<(&Path, &Session, i64)> = items_owned
            .iter()
            .map(|(path, session)| (path.as_path(), session, generation))
            .collect();
        store.observe_bulk_batch(&items)?;
        written += items.len();
    }
    let wall = started.elapsed();
    let process_after = sample_process_memory();
    let heap_after = heap_sample();

    let (verified_count, total_bytes, mean_bytes) = verify_actual_sizes(&db_path)?;
    println!(
        "wrote {written} sessions ({verified_count} verified in store), {} total session_json \
         bytes ({:.1} MB), mean {mean_bytes:.0} bytes/session (target {avg_snapshot_bytes})",
        commas(total_bytes),
        total_bytes as f64 / 1_048_576.0,
    );
    println!(
        "wall time inside the write+read-back loop: {:.3}s ({:.1} sessions/s, {:.2} ms/batch, \
         {:.3} ms/session)",
        wall.as_secs_f64(),
        written as f64 / wall.as_secs_f64(),
        wall.as_secs_f64() * 1000.0 / batch_count as f64,
        wall.as_secs_f64() * 1000.0 / written as f64,
    );
    println!(
        "process: rss {} -> {} (peak {} -> {}), private_bytes {} -> {}",
        mb(process_before.rss_bytes),
        mb(process_after.rss_bytes),
        mb(process_before.peak_rss_bytes),
        mb(process_after.peak_rss_bytes),
        opt_commas(process_before.private_bytes),
        opt_commas(process_after.private_bytes),
    );
    println!(
        "allocator heap: current {} -> {} (peak {} -> {})",
        opt_commas(heap_before.current_bytes),
        opt_commas(heap_after.current_bytes),
        opt_commas(heap_before.peak_bytes),
        opt_commas(heap_after.peak_bytes),
    );
    print_deltas(
        "write+read-back pass",
        process_before,
        heap_before,
        process_after,
        heap_after,
    );
    println!(
        "connection-open count for the read-back half alone: this run's {batch_count} \
         observe_bulk_batch calls open {batch_count} reader connections post-fix (one per call, \
         via load_batch_outcomes's shared open_reader()) versus {written} pre-fix (one per \
         load_one call, one per session -- displaced-key reads would add more, but none occur in \
         this fresh-insert-only corpus) -- a {:.0}x reduction in connection-open count for this \
         run, independent of whatever the measured wall-time/memory deltas above show.",
        written as f64 / batch_count as f64
    );

    drop(store);
    let _ = std::fs::remove_dir_all(&store_dir);
    Ok(())
}

/// Sums the exact on-disk `session_json` byte lengths via SQL aggregates —
/// deliberately never deserializes a `Session` to check this, so the check
/// itself cannot pollute the memory state already sampled above.
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

// ---------------------------------------------------------------------
// Corpus generation -- trimmed copy of hydration_memory.rs's build_session
// (see module doc: no mean-convergence retry loop, no rate-limit-point
// history; turn count alone controls size).
// ---------------------------------------------------------------------

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
    // Rate-limit-point history is intentionally left empty by every caller
    // in this file (see module doc); the parameter stays so the signature
    // matches hydration_memory.rs's for easy comparison, and the field is
    // still exercised at zero length.
    let _ = rate_limit_point_count;

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

    let last_event_at = cursor;

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
        rate_limits_history: Vec::new(),
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

fn print_deltas(
    label: &str,
    baseline_process: ProcessMemorySample,
    baseline_heap: HeapSample,
    process: ProcessMemorySample,
    heap: HeapSample,
) {
    println!(
        "delta baseline -> {label}: rss={} peak_rss={} private_bytes={} heap_bytes={} \
         heap_peak_bytes={}",
        delta_mb(baseline_process.rss_bytes, process.rss_bytes),
        delta_mb(baseline_process.peak_rss_bytes, process.peak_rss_bytes),
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
