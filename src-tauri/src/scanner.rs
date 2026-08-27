use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use rayon::prelude::*;
use walkdir::WalkDir;

use crate::provider::{
    ProviderAdapter, ProviderId, ProviderRegistry, ProviderSourceKind, ProviderSourceSet,
};
use crate::scan_cache::{self, ScanCache};

/// How many sessions `scan_all` hands to `on_session_batch` at a time
/// (issue #132). Read+parse stay per-file and fully parallel; only the
/// durable-write callback is batched, so this bounds how many sessions one
/// `AppState::reconcile_scanned_batch_if_current` call (and, downstream, one
/// `HistoryStore::observe_bulk_batch` transaction) covers. A measured
/// isolated probe of the store's write path
/// (`history_store::tests::diagnostic_probe_commit_count_vs_row_count`)
/// found WAL commit overhead — not row/statement volume — dominates a
/// one-transaction-per-session write pattern by roughly two orders of
/// magnitude; batching amortizes that fixed per-commit cost across this many
/// sessions instead of paying it once per session.
pub(crate) const SCAN_WRITE_BATCH_SIZE: usize = 64;

/// Environment override for [`SCAN_WRITE_BATCH_SIZE`], for the batch-size
/// sweep issue #182 calls for (16 / 64 / 256).
///
/// The two candidate explanations for the ~1,491 MB scan peak make opposite
/// predictions about this knob — the queue model says the peak scales with
/// batch size, the in-flight-window model says it flattens once the window
/// saturates — so varying it is the experiment that separates them. An
/// environment variable rather than a setting: this exists to run an
/// experiment against a real corpus, not as a knob to present to users, and
/// it must not become a supported configuration surface by accident.
const SCAN_WRITE_BATCH_SIZE_ENV: &str = "ODOMETER_SCAN_WRITE_BATCH_SIZE";

/// [`SCAN_WRITE_BATCH_SIZE`], or a valid override from
/// [`SCAN_WRITE_BATCH_SIZE_ENV`].
///
/// Invalid or zero values fall back to the default rather than failing:
/// a typo in an experiment's environment must not make the app scan
/// one-session-at-a-time (the exact pathology #132 fixed) or not at all.
/// The effective value is recorded on the scan's write-lock metric, so a
/// recording always says which value produced it rather than leaving it to
/// be remembered.
pub(crate) fn scan_write_batch_size() -> usize {
    parse_write_batch_size_override(std::env::var(SCAN_WRITE_BATCH_SIZE_ENV).ok().as_deref())
}

/// The parsing half of [`scan_write_batch_size`], separated so it can be
/// tested without mutating process-wide environment state that every other
/// concurrently-running test would also see.
fn parse_write_batch_size_override(raw: Option<&str>) -> usize {
    raw.and_then(|raw| raw.trim().parse::<usize>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(SCAN_WRITE_BATCH_SIZE)
}

pub fn scan_jsonl_files(root: &Path) -> Vec<PathBuf> {
    scan_files(root, |path| {
        path.extension().and_then(|extension| extension.to_str()) == Some("jsonl")
    })
}

fn scan_provider_files(root: &Path, adapter: &dyn ProviderAdapter) -> Vec<PathBuf> {
    scan_files(root, |path| adapter.accepts_path(path))
}

fn scan_files(root: &Path, accepts_path: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    if !root.exists() {
        return Vec::new();
    }

    WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file() && accepts_path(entry.path()))
        .map(|e| e.path().to_path_buf())
        .collect()
}

/// Per-provider breakdown of one scan, for the provider diagnostics report
/// (issue #39). These are the same counters the aggregate `ScanReport` fields
/// derive from, just attributed to the provider that owns each file — no
/// second discovery or parsing pass.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProviderScanCounts {
    pub discovered: u64,
    pub parsed: u64,
    /// A file was parsed without error but produced no session (e.g. an
    /// adapter-specific filter declined it).
    pub skipped: u64,
    pub parse_failures: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub files: usize,
    pub discovery_ms: f64,
    pub processing_ms: f64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub parsed_files: u64,
    pub parse_failures: u64,
    pub parse_total_ms: f64,
    pub parse_max_ms: f64,
    pub cache_lookup_total_ms: f64,
    /// Sum, across every cache hit, of the SQL fetch alone (issue #140):
    /// [`crate::scan_cache::ScanCache::connection`]'s lock plus the `UPDATE
    /// ... RETURNING`. Compared against `cache_lookup_total_ms`, the residual
    /// is deserialize time — this splits "hits queued behind each other on
    /// one connection" from "hits doing more CPU-bound work each".
    pub cache_lookup_sql_ms: f64,
    /// Sum, across every cache hit, of the unlocked JSON deserialize alone.
    pub cache_lookup_deserialize_ms: f64,
    /// Sum, across every cache hit, of the fetched `session_json` blob's
    /// byte length — lets a recording distinguish "more hits" from "the same
    /// hits fetching/deserializing bigger cached `Session` snapshots" as an
    /// explanation for `cache_lookup_total_ms` growth.
    pub cache_hit_bytes_total: u64,
    /// Batch size this scan actually used, so a recording says which value
    /// produced it rather than leaving it to be remembered (issue #182).
    pub write_batch_size: usize,
    /// Worker parallelism available to the scan. The repo sets no
    /// `num_threads`, so this is rayon's default and was previously
    /// unrecorded — leaving the waiter counts alongside it uninterpretable.
    pub available_parallelism: usize,
    pub per_provider: HashMap<ProviderId, ProviderScanCounts>,
}

#[derive(Default)]
struct ProviderAtomics {
    parsed: AtomicU64,
    skipped: AtomicU64,
    parse_failures: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
}

/// Scans all roots in parallel, invoking `on_session_batch(vec![(path,
/// session), ..])` from worker threads once per completed batch of up to
/// [`SCAN_WRITE_BATCH_SIZE`] files (issue #132; before that change this
/// called back once per file). The batch is handed over by value — it is a
/// freshly built, worker-local `Vec` that nothing else references, so the
/// callback taking ownership (rather than a borrowed slice) avoids a full
/// deep clone at every call site that needs to hold onto it (issue #182).
/// Reading and parsing stay per-file and fully parallel — this only changes
/// how many sessions accumulate before the durable-write callback fires, so
/// a bulk scan's writer can batch them into one transaction instead of one
/// per session. Progress callbacks still fire once per file, serialized and
/// monotonic, independent of batch boundaries.
/// When `cache` is `Some`, files whose (size, mtime) match the cache are
/// served from it without being read or parsed, and cache rows are updated
/// individually. The cache must already be open: opening it is the caller's
/// job, both because it must happen only once (a second `ScanCache::load` on
/// the same file would see this open's just-written version metadata and
/// report a false warm cache) and because the caller needs the freshly
/// opened cache's `cold_reason` before this function's first progress
/// callback fires. When duplicate session IDs exist under multiple roots,
/// callback order (and thus which one wins in the caller's map) is
/// nondeterministic.
pub fn scan_all<F, P>(
    sources: &ProviderSourceSet,
    cache: Option<ScanCache>,
    on_session_batch: F,
    on_progress: P,
) -> ScanReport
where
    F: Fn(Vec<(PathBuf, crate::model::Session)>) + Send + Sync,
    P: Fn(usize, usize) + Send + Sync,
{
    let discovery_started = Instant::now();
    let registry = ProviderRegistry::builtin();
    let mut work: Vec<(PathBuf, &'static dyn ProviderAdapter, ProviderSourceKind)> = Vec::new();
    let mut discovered = HashSet::new();
    let mut discovered_counts: HashMap<ProviderId, u64> = HashMap::new();

    for source in sources.iter() {
        // ProviderSourceSet construction validates this lookup. Keeping the
        // invariant here avoids a registry lookup for every discovered file.
        let adapter = registry
            .adapter(source.provider_id())
            .expect("validated provider source has a registered adapter");
        // Every configured provider gets a zero-filled entry even with no
        // discovered files, so the diagnostics report can distinguish "found
        // zero files" from "provider never scanned".
        discovered_counts
            .entry(adapter.descriptor().id.clone())
            .or_insert(0);
        for path in scan_provider_files(source.root(), adapter) {
            if discovered.insert(path.clone()) {
                *discovered_counts
                    .entry(adapter.descriptor().id.clone())
                    .or_insert(0) += 1;
                work.push((path, adapter, source.kind()));
            }
        }
    }

    let total = work.len();
    let discovery_ms = discovery_started.elapsed().as_secs_f64() * 1_000.0;
    on_progress(0, total);

    // Read-only after this point: workers only look up an entry's atomics and
    // mutate those, never the map itself.
    let provider_atomics: HashMap<ProviderId, ProviderAtomics> = discovered_counts
        .keys()
        .map(|id| (id.clone(), ProviderAtomics::default()))
        .collect();

    // The callback mutates shared UI progress. Keep both sequence allocation
    // and delivery under one lock so parallel workers cannot publish 25, then
    // regress to a delayed 24.
    let progress_done = Mutex::new(0usize);
    let cache_hits = AtomicU64::new(0);
    let cache_misses = AtomicU64::new(0);
    let parsed_files = AtomicU64::new(0);
    let parse_failures = AtomicU64::new(0);
    let parse_total_ns = AtomicU64::new(0);
    let parse_max_ns = AtomicU64::new(0);
    let cache_lookup_total_ns = AtomicU64::new(0);
    let cache_lookup_sql_ns = AtomicU64::new(0);
    let cache_lookup_deserialize_ns = AtomicU64::new(0);
    let cache_hit_bytes_total = AtomicU64::new(0);
    let processing_started = Instant::now();

    // Read once, not per chunk: an environment change mid-scan must not
    // produce a run whose batches were different sizes, which would make the
    // sweep's own numbers uninterpretable.
    let write_batch_size = scan_write_batch_size();
    work.par_chunks(write_batch_size).for_each(|chunk| {
        let mut batch: Vec<(PathBuf, crate::model::Session)> = Vec::with_capacity(chunk.len());
        for (path, adapter, kind) in chunk {
            let provider_counters = provider_atomics.get(&adapter.descriptor().id);
            let key = path.to_string_lossy().into_owned();
            // The stamp is taken BEFORE parsing so a file that grows mid-parse
            // looks changed on the next launch rather than serving stale data.
            let stamp = scan_cache::file_stamp(path);
            let cache_started = Instant::now();
            // Captured from every database hit, before `accepts_cached_session`
            // can still turn it into a counted miss below (issue #140): these
            // three totals describe the cache layer's own cost (lock + SQL
            // fetch, unlocked deserialize, bytes fetched), not whether the
            // scanner ultimately reused the row.
            let cache_hit = stamp.and_then(|(size, mtime_ms)| {
                cache
                    .as_ref()
                    .and_then(|cache| cache.lookup_with_stats(&key, size, mtime_ms))
            });
            if let Some(hit) = &cache_hit {
                cache_lookup_sql_ns.fetch_add(hit.sql_ns, Ordering::Relaxed);
                cache_lookup_deserialize_ns.fetch_add(hit.deserialize_ns, Ordering::Relaxed);
                cache_hit_bytes_total.fetch_add(hit.raw_bytes as u64, Ordering::Relaxed);
            }
            let cached = cache_hit
                .map(|hit| hit.session)
                .filter(|session| adapter.accepts_cached_session(session, *kind));
            if cache.as_ref().is_some_and(ScanCache::is_enabled) {
                cache_lookup_total_ns.fetch_add(elapsed_ns(cache_started), Ordering::Relaxed);
                if cached.is_some() {
                    cache_hits.fetch_add(1, Ordering::Relaxed);
                    if let Some(counters) = provider_counters {
                        counters.cache_hits.fetch_add(1, Ordering::Relaxed);
                    }
                } else {
                    cache_misses.fetch_add(1, Ordering::Relaxed);
                    if let Some(counters) = provider_counters {
                        counters.cache_misses.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }

            let session = match cached {
                Some(session) => Some(session),
                None => {
                    let parse_started = Instant::now();
                    let result = adapter.parse_file(path, *kind);
                    let parse_ns = elapsed_ns(parse_started);
                    parsed_files.fetch_add(1, Ordering::Relaxed);
                    parse_total_ns.fetch_add(parse_ns, Ordering::Relaxed);
                    parse_max_ns.fetch_max(parse_ns, Ordering::Relaxed);
                    if let Some(counters) = provider_counters {
                        counters.parsed.fetch_add(1, Ordering::Relaxed);
                    }
                    match result {
                        Ok(Some(session)) => {
                            if let (Some(cache), Some((size, mtime_ms))) = (&cache, stamp) {
                                cache.store(&key, size, mtime_ms, &session);
                            }
                            Some(session)
                        }
                        Ok(None) => {
                            if let Some(counters) = provider_counters {
                                counters.skipped.fetch_add(1, Ordering::Relaxed);
                            }
                            None
                        }
                        Err(e) => {
                            parse_failures.fetch_add(1, Ordering::Relaxed);
                            if let Some(counters) = provider_counters {
                                counters.parse_failures.fetch_add(1, Ordering::Relaxed);
                            }
                            tracing::warn!("failed to parse {:?}: {}", path, e);
                            None
                        }
                    }
                }
            };

            if let Some(session) = session {
                batch.push((path.clone(), session));
            }

            let mut done = progress_done.lock().unwrap();
            *done += 1;
            on_progress(*done, total);
        }
        if !batch.is_empty() {
            on_session_batch(batch);
        }
    });

    if let Some(cache) = cache {
        cache.finish_scan();
    }
    let per_provider = provider_atomics
        .into_iter()
        .map(|(id, counters)| {
            let discovered = discovered_counts.get(&id).copied().unwrap_or(0);
            let counts = ProviderScanCounts {
                discovered,
                parsed: counters.parsed.load(Ordering::Relaxed),
                skipped: counters.skipped.load(Ordering::Relaxed),
                parse_failures: counters.parse_failures.load(Ordering::Relaxed),
                cache_hits: counters.cache_hits.load(Ordering::Relaxed),
                cache_misses: counters.cache_misses.load(Ordering::Relaxed),
            };
            (id, counts)
        })
        .collect();
    ScanReport {
        files: total,
        discovery_ms,
        processing_ms: processing_started.elapsed().as_secs_f64() * 1_000.0,
        cache_hits: cache_hits.load(Ordering::Relaxed),
        cache_misses: cache_misses.load(Ordering::Relaxed),
        parsed_files: parsed_files.load(Ordering::Relaxed),
        parse_failures: parse_failures.load(Ordering::Relaxed),
        parse_total_ms: nanos_to_ms(parse_total_ns.load(Ordering::Relaxed)),
        parse_max_ms: nanos_to_ms(parse_max_ns.load(Ordering::Relaxed)),
        cache_lookup_total_ms: nanos_to_ms(cache_lookup_total_ns.load(Ordering::Relaxed)),
        cache_lookup_sql_ms: nanos_to_ms(cache_lookup_sql_ns.load(Ordering::Relaxed)),
        cache_lookup_deserialize_ms: nanos_to_ms(
            cache_lookup_deserialize_ns.load(Ordering::Relaxed),
        ),
        cache_hit_bytes_total: cache_hit_bytes_total.load(Ordering::Relaxed),
        write_batch_size,
        available_parallelism: std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(0),
        per_provider,
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn nanos_to_ms(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::{parse_write_batch_size_override, SCAN_WRITE_BATCH_SIZE};

    /// Issue #182: the batch-size override exists to run a sweep against a
    /// real corpus, so a typo in an experiment's environment must degrade to
    /// the shipped default rather than to a pathological value. Zero in
    /// particular would mean `par_chunks(0)`, and one-session-at-a-time is
    /// the exact pattern #132 fixed.
    #[test]
    fn an_invalid_batch_size_override_falls_back_to_the_default() {
        for invalid in ["", "0", "-1", "sixty-four", "64.0"] {
            assert_eq!(
                parse_write_batch_size_override(Some(invalid)),
                SCAN_WRITE_BATCH_SIZE,
                "{invalid:?} must not change the batch size"
            );
        }
        assert_eq!(parse_write_batch_size_override(None), SCAN_WRITE_BATCH_SIZE);
    }

    #[test]
    fn a_valid_batch_size_override_is_used() {
        for (raw, expected) in [("16", 16), ("256", 256), (" 128 ", 128)] {
            assert_eq!(parse_write_batch_size_override(Some(raw)), expected);
        }
    }
}
