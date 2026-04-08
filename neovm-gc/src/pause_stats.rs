//! Rolling pause-time histogram for GC observability.
//!
//! Records recent STW pause durations (`pause_nanos`) captured by the
//! runtime after each completed collection cycle. Exposes percentile
//! summaries (P50/P95/P99) over a bounded window so consumers can
//! monitor pause latency without pulling every cycle's stats.
//!
//! The histogram is not a fully general tdigest or HDR histogram — it is
//! a simple ring buffer of recent samples. This is adequate for a
//! single-mutator VM where collection frequency is modest and the
//! consumer just wants "were recent pauses within budget?" signals.

use std::sync::{Arc, Mutex, MutexGuard};

const DEFAULT_WINDOW: usize = 128;

/// Public snapshot of pause-time statistics over a rolling window.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PauseHistogram {
    /// Number of samples currently held in the window.
    pub sample_count: usize,
    /// Maximum window capacity.
    pub window_capacity: usize,
    /// Total number of pause samples ever observed (including evicted samples).
    pub total_samples: u64,
    /// Sum of pauses currently in the window, in nanoseconds.
    pub sum_nanos: u64,
    /// Smallest pause in the window.
    pub min_nanos: u64,
    /// Largest pause in the window.
    pub max_nanos: u64,
    /// 50th-percentile pause, in nanoseconds.
    pub p50_nanos: u64,
    /// 95th-percentile pause, in nanoseconds.
    pub p95_nanos: u64,
    /// 99th-percentile pause, in nanoseconds.
    pub p99_nanos: u64,
    /// Mean pause over the window, in nanoseconds (integer truncated).
    pub mean_nanos: u64,
}

impl PauseHistogram {
    /// Empty histogram snapshot at the default window capacity.
    pub fn empty() -> Self {
        Self {
            window_capacity: DEFAULT_WINDOW,
            ..Self::default()
        }
    }
}

#[derive(Debug)]
pub(crate) struct PauseStats {
    samples: Vec<u64>,
    head: usize,
    len: usize,
    capacity: usize,
    total_samples: u64,
}

impl Default for PauseStats {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_WINDOW)
    }
}

impl PauseStats {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            samples: Vec::with_capacity(capacity),
            head: 0,
            len: 0,
            capacity,
            total_samples: 0,
        }
    }

    pub(crate) fn record(&mut self, pause_nanos: u64) {
        self.total_samples = self.total_samples.saturating_add(1);
        if self.samples.len() < self.capacity {
            self.samples.push(pause_nanos);
            self.len = self.samples.len();
            self.head = self.len % self.capacity;
            return;
        }
        self.samples[self.head] = pause_nanos;
        self.head = (self.head + 1) % self.capacity;
    }

    pub(crate) fn snapshot(&self) -> PauseHistogram {
        if self.len == 0 {
            return PauseHistogram {
                window_capacity: self.capacity,
                total_samples: self.total_samples,
                ..PauseHistogram::default()
            };
        }

        let mut window: Vec<u64> = self.samples[..self.len].to_vec();
        window.sort_unstable();

        let sum: u64 = window.iter().copied().fold(0u64, u64::saturating_add);
        let min = *window.first().expect("non-empty window");
        let max = *window.last().expect("non-empty window");
        let mean = sum / self.len as u64;

        let p50 = percentile(&window, 50);
        let p95 = percentile(&window, 95);
        let p99 = percentile(&window, 99);

        PauseHistogram {
            sample_count: self.len,
            window_capacity: self.capacity,
            total_samples: self.total_samples,
            sum_nanos: sum,
            min_nanos: min,
            max_nanos: max,
            p50_nanos: p50,
            p95_nanos: p95,
            p99_nanos: p99,
            mean_nanos: mean,
        }
    }

    #[cfg(test)]
    pub(crate) fn reset(&mut self) {
        self.samples.clear();
        self.head = 0;
        self.len = 0;
        self.total_samples = 0;
    }
}

fn percentile(sorted: &[u64], percentile: u8) -> u64 {
    debug_assert!(!sorted.is_empty());
    debug_assert!(percentile <= 100);
    // Nearest-rank method: index = ceil(p/100 * n) - 1, clamped.
    let n = sorted.len();
    let rank = (percentile as usize * n).div_ceil(100);
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PauseStatsHandle {
    state: Arc<Mutex<PauseStats>>,
}

impl PauseStatsHandle {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(PauseStats::default())),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PauseStats> {
        self.state
            .lock()
            .expect("pause stats should not be poisoned")
    }

    pub(crate) fn record(&self, pause_nanos: u64) {
        self.lock().record(pause_nanos);
    }

    pub(crate) fn snapshot(&self) -> PauseHistogram {
        self.lock().snapshot()
    }

}

#[cfg(test)]
#[path = "pause_stats_test.rs"]
mod tests;
