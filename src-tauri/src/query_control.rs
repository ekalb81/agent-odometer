//! Per-request limits for local, read-only query surfaces.
use anyhow::{bail, Result};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct QueryControl {
    pub max_sessions: usize,
    pub max_windows: usize,
    pub max_output_bytes: usize,
    pub max_rows: usize,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
    rows: Arc<AtomicUsize>,
    snapshot_bytes: Arc<AtomicUsize>,
}

impl Default for QueryControl {
    fn default() -> Self {
        Self::with_timeout(Duration::from_secs(10))
    }
}

impl QueryControl {
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            max_sessions: 50_000,
            max_windows: 32,
            max_output_bytes: 8 * 1024 * 1024,
            max_rows: 1_000_000,
            deadline: Instant::now()
                .checked_add(timeout)
                .unwrap_or_else(Instant::now),
            cancelled: Arc::new(AtomicBool::new(false)),
            rows: Arc::new(AtomicUsize::new(0)),
            snapshot_bytes: Arc::new(AtomicUsize::new(0)),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            bail!("query cancelled");
        }
        if Instant::now() >= self.deadline {
            bail!("query deadline exceeded");
        }
        Ok(())
    }
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }
    pub(crate) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }
    pub(crate) fn consume_row(&self) -> Result<()> {
        self.check()?;
        if self.rows.fetch_add(1, Ordering::Relaxed) >= self.max_rows {
            bail!("query row limit exceeded ({})", self.max_rows);
        }
        Ok(())
    }
    pub(crate) fn consume_snapshot_bytes(&self, bytes: usize) -> Result<()> {
        self.check()?;
        const LIMIT: usize = 64 * 1024 * 1024;
        let prior = self.snapshot_bytes.fetch_add(bytes, Ordering::Relaxed);
        if bytes > LIMIT || prior > LIMIT.saturating_sub(bytes) {
            bail!("query snapshot materialization limit exceeded (64 MiB)");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cancellation_is_shared_and_expiry_is_explicit() {
        let control = QueryControl::default();
        control.clone().cancel();
        assert!(control
            .check()
            .unwrap_err()
            .to_string()
            .contains("cancelled"));
        assert!(QueryControl::with_timeout(Duration::ZERO)
            .check()
            .unwrap_err()
            .to_string()
            .contains("deadline"));
    }
    #[test]
    fn row_budget_is_shared() {
        let control = QueryControl {
            max_rows: 1,
            ..Default::default()
        };
        control.consume_row().unwrap();
        assert!(control.clone().consume_row().is_err());
    }
}
