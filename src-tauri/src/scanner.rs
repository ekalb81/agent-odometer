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

/// Scans all roots in parallel, invoking `on_session(path, session)` from
/// worker threads as each file finishes. Progress callbacks are serialized
/// and monotonic.
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
    on_session: F,
    on_progress: P,
) -> ScanReport
where
    F: Fn(&Path, crate::model::Session) + Send + Sync,
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
    let processing_started = Instant::now();

    work.par_iter().for_each(|(path, adapter, kind)| {
        let provider_counters = provider_atomics.get(&adapter.descriptor().id);
        let key = path.to_string_lossy().into_owned();
        // The stamp is taken BEFORE parsing so a file that grows mid-parse
        // looks changed on the next launch rather than serving stale data.
        let stamp = scan_cache::file_stamp(path);
        let cache_started = Instant::now();
        let cached = stamp
            .and_then(|(size, mtime_ms)| {
                cache
                    .as_ref()
                    .and_then(|cache| cache.lookup(&key, size, mtime_ms))
            })
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
            on_session(path.as_path(), session);
        }

        let mut done = progress_done.lock().unwrap();
        *done += 1;
        on_progress(*done, total);
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
        per_provider,
    }
}

fn elapsed_ns(started: Instant) -> u64 {
    started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
}

fn nanos_to_ms(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}
