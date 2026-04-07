//! Adaptive GC pacer (Phase 7 completion).
//!
//! Layered on top of the static allocation-pressure thresholds. The pacer
//! observes allocation and mark rates over recently completed cycles, then
//! uses an EWMA model to decide when the next major collection should
//! fire. The goal is to keep observed pause time within
//! [`PacerConfig::target_pause`] while not running GC more often than
//! [`PacerConfig::heap_growth_target_ratio`] requires.
//!
//! Inspired by Go's pacer redesign:
//! <https://go.googlesource.com/proposal/+/master/design/44167-gc-pacer-redesign.md>
//!
//! The pacer never overrides existing static thresholds — if the static
//! `allocation_pressure_plan` already wants to collect, the static plan
//! still runs. The pacer only triggers an *additional* major collection
//! when its allocation/mark rate model believes one is due.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::stats::CollectionStats;

/// Tunable parameters for the adaptive pacer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PacerConfig {
    /// Target pause budget per major collection. The pacer tries to
    /// keep observed mark+reclaim time below this. Default: 10ms.
    pub target_pause: Duration,
    /// Target CPU fraction the pacer is willing to spend on GC over
    /// a sliding allocation window. 0.25 means "GC may use up to 25%
    /// of mutator-equivalent CPU time." Default: 0.25.
    pub target_gc_cpu_fraction: f64,
    /// Minimum heap growth before the next major GC may fire,
    /// expressed as a multiplier of the live set after the last
    /// completed major. Default: 1.5 (heap can grow 50% before
    /// triggering a major collection).
    pub heap_growth_target_ratio: f64,
    /// Smoothing factor for exponentially weighted moving averages
    /// over allocation and mark rates. Range (0, 1]. 0.2 means
    /// "the latest sample contributes 20% to the running estimate."
    /// Default: 0.2.
    pub ewma_alpha: f64,
    /// Hard floor on the live-bytes threshold so the pacer never
    /// scales below a sane minimum (otherwise tiny early heaps
    /// would trigger GC every allocation). Default: 1 MiB.
    pub min_trigger_bytes: usize,
}

impl Default for PacerConfig {
    fn default() -> Self {
        Self {
            target_pause: Duration::from_millis(10),
            target_gc_cpu_fraction: 0.25,
            heap_growth_target_ratio: 1.5,
            ewma_alpha: 0.2,
            min_trigger_bytes: 1024 * 1024,
        }
    }
}

/// Public snapshot of the pacer's current model.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PacerStats {
    /// Last computed allocation rate, bytes per second.
    pub allocation_rate_bps: u64,
    /// Last computed mark rate, bytes per second.
    pub mark_rate_bps: u64,
    /// Live bytes after the last completed major collection.
    pub last_live_bytes: usize,
    /// Threshold the pacer will fire the next major at,
    /// in live bytes.
    pub next_major_trigger_bytes: usize,
    /// Number of cycles the pacer has observed.
    pub observed_cycles: u64,
    /// Number of times the pacer overshoots its budget
    /// (observed pause exceeded target_pause).
    pub overshoot_count: u64,
}

/// Decision returned from [`Pacer::record_allocation`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacerDecision {
    /// Continue allocating; no collection needed.
    Continue,
    /// Trigger a minor collection.
    TriggerMinor,
    /// Trigger a major collection.
    TriggerMajor,
}

/// Adaptive GC pacer state.
///
/// Cheap to clone: the inner state is `Arc<Mutex<...>>`.
#[derive(Clone, Debug)]
pub struct Pacer {
    config: PacerConfig,
    state: Arc<Mutex<PacerState>>,
}

#[derive(Debug)]
struct PacerState {
    last_allocation_rate_bps: f64,
    last_mark_rate_bps: f64,
    last_live_bytes: usize,
    next_major_trigger_bytes: usize,
    observed_cycles: u64,
    overshoot_count: u64,
    last_cycle_start: Option<Instant>,
    bytes_allocated_since_last_cycle: usize,
}

impl Pacer {
    /// Build a new pacer with `config`. The initial trigger threshold
    /// is set to `config.min_trigger_bytes` so the pacer cannot fire
    /// before the heap grows past the floor.
    pub fn new(config: PacerConfig) -> Self {
        let state = PacerState {
            last_allocation_rate_bps: 0.0,
            last_mark_rate_bps: 0.0,
            last_live_bytes: 0,
            next_major_trigger_bytes: config.min_trigger_bytes,
            observed_cycles: 0,
            overshoot_count: 0,
            last_cycle_start: None,
            bytes_allocated_since_last_cycle: 0,
        };
        Self {
            config,
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Returns the configuration the pacer was constructed with.
    pub fn config(&self) -> PacerConfig {
        self.config
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PacerState> {
        self.state
            .lock()
            .expect("pacer state should not be poisoned")
    }

    /// Tell the pacer about a fresh allocation. Cheap (just adds to a
    /// counter). Returns whether GC should be triggered now.
    pub fn record_allocation(&self, bytes: usize) -> PacerDecision {
        let mut state = self.lock();
        state.bytes_allocated_since_last_cycle = state
            .bytes_allocated_since_last_cycle
            .saturating_add(bytes);
        let projected_live = state
            .last_live_bytes
            .saturating_add(state.bytes_allocated_since_last_cycle);
        if projected_live >= state.next_major_trigger_bytes {
            PacerDecision::TriggerMajor
        } else {
            PacerDecision::Continue
        }
    }

    /// Tell the pacer a major collection just completed. Updates the
    /// EWMA estimates and computes the next trigger threshold.
    pub fn record_completed_cycle(
        &self,
        cycle: &CollectionStats,
        live_bytes_after: usize,
    ) {
        self.record_completed_cycle_at(cycle, live_bytes_after, Instant::now());
    }

    /// Same as [`Self::record_completed_cycle`] but lets callers (and
    /// tests) supply the wall-clock instant for deterministic behavior.
    pub fn record_completed_cycle_at(
        &self,
        cycle: &CollectionStats,
        live_bytes_after: usize,
        now: Instant,
    ) {
        let alpha = self.config.ewma_alpha.clamp(f64::MIN_POSITIVE, 1.0);
        let target_pause_nanos = duration_as_nanos_u64(self.config.target_pause);
        let target_pause_secs = nanos_as_secs_f64(target_pause_nanos);
        let mut state = self.lock();

        // 1. Mark rate (bytes per second processed by the marker).
        if cycle.pause_nanos > 0 {
            let pause_secs = nanos_as_secs_f64(cycle.pause_nanos);
            let observed = (live_bytes_after as f64) / pause_secs;
            state.last_mark_rate_bps =
                ewma_update(state.last_mark_rate_bps, observed, alpha);
        }

        // 2. Allocation rate (bytes per second observed by the mutator
        //    between the previous cycle's completion and this one).
        if let Some(start) = state.last_cycle_start {
            let elapsed = now.saturating_duration_since(start);
            let elapsed_secs = duration_as_secs_f64(elapsed);
            if elapsed_secs > 0.0 {
                let observed = (state.bytes_allocated_since_last_cycle as f64) / elapsed_secs;
                state.last_allocation_rate_bps =
                    ewma_update(state.last_allocation_rate_bps, observed, alpha);
            }
        }

        // 3. Overshoot accounting.
        if cycle.pause_nanos > target_pause_nanos {
            state.overshoot_count = state.overshoot_count.saturating_add(1);
        }

        state.observed_cycles = state.observed_cycles.saturating_add(1);
        state.last_live_bytes = live_bytes_after;

        // 4. Compute the next trigger threshold.
        let target_growth = compute_target_growth(
            live_bytes_after,
            self.config.heap_growth_target_ratio,
            self.config.min_trigger_bytes,
        );
        let max_safe_growth =
            compute_max_safe_growth(state.last_mark_rate_bps, target_pause_secs);
        let chosen_growth = if max_safe_growth == 0 {
            target_growth
        } else {
            target_growth.min(max_safe_growth)
        };
        let chosen_growth = chosen_growth.max(self.config.min_trigger_bytes);
        state.next_major_trigger_bytes = live_bytes_after.saturating_add(chosen_growth);

        // 5. Reset allocation accounting for the new window.
        state.bytes_allocated_since_last_cycle = 0;
        state.last_cycle_start = Some(now);
    }

    /// Snapshot current pacer stats.
    pub fn stats(&self) -> PacerStats {
        let state = self.lock();
        PacerStats {
            allocation_rate_bps: state.last_allocation_rate_bps as u64,
            mark_rate_bps: state.last_mark_rate_bps as u64,
            last_live_bytes: state.last_live_bytes,
            next_major_trigger_bytes: state.next_major_trigger_bytes,
            observed_cycles: state.observed_cycles,
            overshoot_count: state.overshoot_count,
        }
    }
}

fn ewma_update(previous: f64, observed: f64, alpha: f64) -> f64 {
    if previous == 0.0 {
        observed
    } else {
        alpha * observed + (1.0 - alpha) * previous
    }
}

fn compute_target_growth(
    live_bytes_after: usize,
    growth_ratio: f64,
    min_trigger_bytes: usize,
) -> usize {
    let scaled = (live_bytes_after as f64) * growth_ratio;
    let scaled = if scaled.is_finite() && scaled >= 0.0 {
        scaled as usize
    } else {
        0
    };
    scaled.max(min_trigger_bytes)
}

fn compute_max_safe_growth(mark_rate_bps: f64, target_pause_secs: f64) -> usize {
    if !mark_rate_bps.is_finite() || mark_rate_bps <= 0.0 || target_pause_secs <= 0.0 {
        return 0;
    }
    let bytes = mark_rate_bps * target_pause_secs;
    if !bytes.is_finite() || bytes <= 0.0 {
        return 0;
    }
    bytes as usize
}

fn duration_as_nanos_u64(d: Duration) -> u64 {
    d.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn nanos_as_secs_f64(nanos: u64) -> f64 {
    (nanos as f64) / 1_000_000_000.0
}

fn duration_as_secs_f64(d: Duration) -> f64 {
    d.as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cycle_with_pause(pause_nanos: u64) -> CollectionStats {
        CollectionStats {
            collections: 1,
            major_collections: 1,
            pause_nanos,
            ..CollectionStats::default()
        }
    }

    #[test]
    fn ewma_update_seeds_with_first_observation() {
        assert_eq!(ewma_update(0.0, 100.0, 0.2), 100.0);
    }

    #[test]
    fn ewma_update_blends_observations_with_alpha() {
        // previous=100, observed=200, alpha=0.5 → 0.5*200 + 0.5*100 = 150
        assert_eq!(ewma_update(100.0, 200.0, 0.5), 150.0);
    }

    #[test]
    fn ewma_update_alpha_one_takes_latest() {
        assert_eq!(ewma_update(100.0, 200.0, 1.0), 200.0);
    }

    #[test]
    fn ewma_update_small_alpha_gives_inertia() {
        // alpha=0.1: latest contributes 10%.
        let blended = ewma_update(100.0, 200.0, 0.1);
        assert!((blended - 110.0).abs() < 0.001);
    }

    #[test]
    fn compute_target_growth_floor() {
        assert_eq!(compute_target_growth(0, 1.5, 1024), 1024);
    }

    #[test]
    fn compute_target_growth_scales_live_bytes() {
        // 1000 * 1.5 = 1500, above floor of 100, returns 1500.
        assert_eq!(compute_target_growth(1000, 1.5, 100), 1500);
    }

    #[test]
    fn compute_max_safe_growth_zero_when_no_mark_rate() {
        assert_eq!(compute_max_safe_growth(0.0, 0.01), 0);
    }

    #[test]
    fn compute_max_safe_growth_returns_byte_budget() {
        // 1_000_000 bytes/sec * 0.01 sec = 10_000 bytes.
        assert_eq!(compute_max_safe_growth(1_000_000.0, 0.01), 10_000);
    }

    #[test]
    fn pacer_default_threshold_starts_at_min_trigger() {
        let pacer = Pacer::new(PacerConfig::default());
        let stats = pacer.stats();
        assert_eq!(stats.observed_cycles, 0);
        assert_eq!(stats.last_live_bytes, 0);
        assert_eq!(stats.next_major_trigger_bytes, 1024 * 1024);
    }

    #[test]
    fn pacer_record_completed_cycle_updates_threshold() {
        let pacer = Pacer::new(PacerConfig {
            min_trigger_bytes: 256,
            ..PacerConfig::default()
        });
        let cycle = cycle_with_pause(1_000_000);
        pacer.record_completed_cycle(&cycle, 4096);
        let stats = pacer.stats();
        assert_eq!(stats.observed_cycles, 1);
        assert_eq!(stats.last_live_bytes, 4096);
        // 4096 + max(256, min(target_growth, max_safe_growth))
        assert!(stats.next_major_trigger_bytes >= 4096 + 256);
    }

    #[test]
    fn pacer_overshoot_increments_count_when_pause_exceeds_target() {
        let pacer = Pacer::new(PacerConfig {
            target_pause: Duration::from_millis(1),
            ..PacerConfig::default()
        });
        let cycle = cycle_with_pause(2_000_000);
        pacer.record_completed_cycle(&cycle, 1024);
        let stats = pacer.stats();
        assert_eq!(stats.overshoot_count, 1);
    }

    #[test]
    fn pacer_no_overshoot_when_pause_within_target() {
        let pacer = Pacer::new(PacerConfig {
            target_pause: Duration::from_millis(10),
            ..PacerConfig::default()
        });
        let cycle = cycle_with_pause(1_000_000);
        pacer.record_completed_cycle(&cycle, 1024);
        let stats = pacer.stats();
        assert_eq!(stats.overshoot_count, 0);
    }

    #[test]
    fn pacer_record_allocation_returns_continue_below_threshold() {
        let pacer = Pacer::new(PacerConfig {
            min_trigger_bytes: 4096,
            ..PacerConfig::default()
        });
        let decision = pacer.record_allocation(64);
        assert_eq!(decision, PacerDecision::Continue);
    }

    #[test]
    fn pacer_record_allocation_returns_trigger_major_when_threshold_exceeded() {
        let pacer = Pacer::new(PacerConfig {
            min_trigger_bytes: 256,
            ..PacerConfig::default()
        });
        let decision = pacer.record_allocation(512);
        assert_eq!(decision, PacerDecision::TriggerMajor);
    }
}
