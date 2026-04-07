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

use crate::object::SpaceKind;
use crate::stats::CollectionStats;

/// Allocation-space hint passed to [`Pacer::record_allocation`]. Only
/// `Nursery` advances the soft minor counter; everything else updates
/// the major-side counter alone. This mirrors a subset of the crate-
/// internal `SpaceKind` enum without leaking it through the public
/// API surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacerAllocationSpace {
    /// Nursery (young) generation.
    Nursery,
    /// Any other space (old, pinned, large, immortal).
    Other,
}

impl From<SpaceKind> for PacerAllocationSpace {
    fn from(space: SpaceKind) -> Self {
        match space {
            SpaceKind::Nursery => PacerAllocationSpace::Nursery,
            SpaceKind::Old
            | SpaceKind::Pinned
            | SpaceKind::Large
            | SpaceKind::Immortal => PacerAllocationSpace::Other,
        }
    }
}

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
    /// Soft byte threshold for pacer-driven minor collections.
    /// When the bytes the mutator has allocated to the nursery
    /// since the last completed minor cycle reach this value, the
    /// pacer asks for a minor GC.
    ///
    /// This sits *underneath* the static `nursery.semispace_bytes`
    /// hard threshold: the static path is the backstop, the pacer
    /// path is an optional early trigger so the next allocation
    /// burst does not slam into a full nursery. 0 disables it.
    /// Default: 0 (disabled — opt-in).
    pub nursery_soft_trigger_bytes: usize,
}

impl Default for PacerConfig {
    fn default() -> Self {
        Self {
            target_pause: Duration::from_millis(10),
            target_gc_cpu_fraction: 0.25,
            heap_growth_target_ratio: 1.5,
            ewma_alpha: 0.2,
            min_trigger_bytes: 1024 * 1024,
            nursery_soft_trigger_bytes: 0,
        }
    }
}

/// Public snapshot of the pacer's current model.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
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
    /// Bytes the mutator has allocated to the nursery since the
    /// last completed minor cycle. The pacer compares this against
    /// `PacerConfig::nursery_soft_trigger_bytes` to decide whether
    /// to ask for an early minor.
    pub nursery_bytes_since_last_minor: usize,
    /// Number of cycles the pacer has observed.
    pub observed_cycles: u64,
    /// Number of completed minor cycles the pacer has observed.
    pub observed_minor_cycles: u64,
    /// Number of times a pacer decision drove a major collection
    /// (i.e. the static pressure plan would not have fired one but
    /// the pacer's `TriggerMajor` decision did). Useful for telling
    /// pacer-driven work apart from static threshold work.
    pub pacer_triggered_majors: u64,
    /// Number of times a pacer decision drove a minor collection
    /// (the pacer's nursery soft trigger fired before the static
    /// nursery threshold did).
    pub pacer_triggered_minors: u64,
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
/// Cheap to clone: the inner state is `Arc<Mutex<...>>`. Config and
/// runtime state both live behind the same lock so [`Pacer::clone`]s
/// observe each other's [`Pacer::update_config`] calls.
#[derive(Clone, Debug)]
pub struct Pacer {
    state: Arc<Mutex<PacerState>>,
}

#[derive(Debug)]
struct PacerState {
    config: PacerConfig,
    last_allocation_rate_bps: f64,
    last_mark_rate_bps: f64,
    last_live_bytes: usize,
    next_major_trigger_bytes: usize,
    observed_cycles: u64,
    observed_minor_cycles: u64,
    pacer_triggered_majors: u64,
    pacer_triggered_minors: u64,
    overshoot_count: u64,
    last_cycle_start: Option<Instant>,
    bytes_allocated_since_last_cycle: usize,
    bytes_allocated_to_nursery_since_last_minor: usize,
}

impl Pacer {
    /// Build a new pacer with `config`. The initial trigger threshold
    /// is set to `config.min_trigger_bytes` so the pacer cannot fire
    /// before the heap grows past the floor.
    pub fn new(config: PacerConfig) -> Self {
        let state = PacerState {
            config,
            last_allocation_rate_bps: 0.0,
            last_mark_rate_bps: 0.0,
            last_live_bytes: 0,
            next_major_trigger_bytes: config.min_trigger_bytes,
            observed_cycles: 0,
            observed_minor_cycles: 0,
            pacer_triggered_majors: 0,
            pacer_triggered_minors: 0,
            overshoot_count: 0,
            last_cycle_start: None,
            bytes_allocated_since_last_cycle: 0,
            bytes_allocated_to_nursery_since_last_minor: 0,
        };
        Self {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Returns a snapshot of the pacer's current configuration.
    pub fn config(&self) -> PacerConfig {
        self.lock().config
    }

    /// Replace the pacer's configuration in place. Preserves all
    /// accumulated runtime state (EWMA estimates, observed cycles,
    /// pacer counters, next-major-trigger threshold). Use this for
    /// runtime tuning when the new config should take effect on the
    /// next decision without resetting the pacer's history.
    ///
    /// All cloned [`Pacer`] handles see the new config because they
    /// share the same `Arc<Mutex<PacerState>>`.
    pub fn update_config(&self, config: PacerConfig) {
        let mut state = self.lock();
        state.config = config;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PacerState> {
        self.state
            .lock()
            .expect("pacer state should not be poisoned")
    }

    /// Tell the pacer about a fresh allocation. Cheap (just adds to
    /// a couple of counters). Returns whether GC should be triggered
    /// now. The `space` argument lets the pacer keep separate
    /// nursery accounting for the soft minor trigger.
    ///
    /// `space` accepts anything convertible into
    /// [`PacerAllocationSpace`], so internal callers can pass the
    /// crate-private `SpaceKind` directly via the blanket `From`
    /// impl.
    pub fn record_allocation(
        &self,
        bytes: usize,
        space: impl Into<PacerAllocationSpace>,
    ) -> PacerDecision {
        let space = space.into();
        let mut state = self.lock();
        state.bytes_allocated_since_last_cycle = state
            .bytes_allocated_since_last_cycle
            .saturating_add(bytes);
        if matches!(space, PacerAllocationSpace::Nursery) {
            state.bytes_allocated_to_nursery_since_last_minor = state
                .bytes_allocated_to_nursery_since_last_minor
                .saturating_add(bytes);
        }
        Self::compute_decision(&state)
    }

    /// Re-evaluate the current pacer state without advancing it.
    /// Useful for callers that want to check the decision after a
    /// nested action that may have completed cycles and reset
    /// counters in between (e.g. the runtime's
    /// `prepare_typed_allocation` runs the static pressure plan
    /// between recording the allocation and acting on the pacer).
    pub fn decision(&self) -> PacerDecision {
        let state = self.lock();
        Self::compute_decision(&state)
    }

    fn compute_decision(state: &PacerState) -> PacerDecision {
        // Major check has priority — when both thresholds are
        // exceeded the bigger collection wins. The minor would only
        // be wasted work in that case.
        let projected_live = state
            .last_live_bytes
            .saturating_add(state.bytes_allocated_since_last_cycle);
        if projected_live >= state.next_major_trigger_bytes {
            return PacerDecision::TriggerMajor;
        }
        if state.config.nursery_soft_trigger_bytes > 0
            && state.bytes_allocated_to_nursery_since_last_minor
                >= state.config.nursery_soft_trigger_bytes
        {
            return PacerDecision::TriggerMinor;
        }
        PacerDecision::Continue
    }

    /// Tell the pacer that a minor cycle just completed. Resets the
    /// nursery soft-trigger counter and bumps the observed-minor
    /// counter. Cheap; takes the lock briefly.
    pub fn record_completed_minor_cycle(&self) {
        let mut state = self.lock();
        state.bytes_allocated_to_nursery_since_last_minor = 0;
        state.observed_minor_cycles = state.observed_minor_cycles.saturating_add(1);
    }

    /// Bump the pacer-triggered major counter. Called by the runtime
    /// when prepare_typed_allocation acts on a `TriggerMajor` decision
    /// (i.e. the static pressure plan would not have fired).
    pub fn record_pacer_triggered_major(&self) {
        let mut state = self.lock();
        state.pacer_triggered_majors = state.pacer_triggered_majors.saturating_add(1);
    }

    /// Bump the pacer-triggered minor counter. Called by the runtime
    /// when prepare_typed_allocation acts on a `TriggerMinor` decision
    /// (i.e. the nursery soft trigger fired before the static
    /// nursery threshold).
    pub fn record_pacer_triggered_minor(&self) {
        let mut state = self.lock();
        state.pacer_triggered_minors = state.pacer_triggered_minors.saturating_add(1);
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
        let mut state = self.lock();
        // Read config out of state under the same lock so any
        // concurrent update_config call observes a consistent view.
        let config = state.config;
        let alpha = config.ewma_alpha.clamp(f64::MIN_POSITIVE, 1.0);
        let target_pause_nanos = duration_as_nanos_u64(config.target_pause);
        let target_pause_secs = nanos_as_secs_f64(target_pause_nanos);

        // 1. Mark rate (bytes per second processed by the marker).
        //
        // Approximate the mark-only time by subtracting the
        // reclaim-prepare phase from the total stop-the-world pause.
        // CollectionStats already separates reclaim_prepare_nanos so
        // this gives a tighter mark rate estimate than treating the
        // whole pause as marking. Falls back to pause_nanos when the
        // subtraction would underflow (defensive — should not happen
        // in practice because reclaim_prepare_nanos is a strict
        // sub-interval of pause_nanos).
        let mark_nanos = cycle
            .pause_nanos
            .checked_sub(cycle.reclaim_prepare_nanos)
            .unwrap_or(cycle.pause_nanos);
        if mark_nanos > 0 {
            let mark_secs = nanos_as_secs_f64(mark_nanos);
            let observed = (live_bytes_after as f64) / mark_secs;
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
        //
        // Three constraints stack here, and the smallest one wins
        // (subject to the `min_trigger_bytes` floor at the end):
        //
        //   a. `target_growth` — heap is allowed to grow by
        //      `heap_growth_target_ratio * live_bytes`. This is the
        //      coarse "don't spend GC time on a barely-growing heap"
        //      knob.
        //   b. `max_safe_growth` — the most bytes the marker can chew
        //      through inside one `target_pause` budget. Prevents the
        //      next major from blowing the pause SLO.
        //   c. `cpu_aware_growth` — the Go-style CPU-aware trigger:
        //      pick `G` so that the GC's expected wall-clock time
        //      consumes at most `target_gc_cpu_fraction` of the
        //      mutator+GC duty cycle. Skipped on the first cycle
        //      because we don't yet have an allocation rate sample.
        //
        // Each `compute_*_growth` helper returns 0 to mean
        // "no signal yet, skip this constraint." Constraints that
        // return 0 are not applied to the running minimum.
        let target_growth = compute_target_growth(
            live_bytes_after,
            config.heap_growth_target_ratio,
            config.min_trigger_bytes,
        );
        let max_safe_growth =
            compute_max_safe_growth(state.last_mark_rate_bps, target_pause_secs);
        let cpu_aware_growth = compute_cpu_aware_growth(
            live_bytes_after,
            state.last_allocation_rate_bps,
            state.last_mark_rate_bps,
            config.target_gc_cpu_fraction,
        );
        let mut chosen_growth = target_growth;
        if max_safe_growth > 0 {
            chosen_growth = chosen_growth.min(max_safe_growth);
        }
        if cpu_aware_growth > 0 {
            chosen_growth = chosen_growth.min(cpu_aware_growth);
        }
        let chosen_growth = chosen_growth.max(config.min_trigger_bytes);
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
            nursery_bytes_since_last_minor: state.bytes_allocated_to_nursery_since_last_minor,
            observed_cycles: state.observed_cycles,
            observed_minor_cycles: state.observed_minor_cycles,
            pacer_triggered_majors: state.pacer_triggered_majors,
            pacer_triggered_minors: state.pacer_triggered_minors,
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

/// Go-style CPU-aware growth budget.
///
/// Derivation: let `L` be live bytes, `r_alloc` be the observed
/// allocation rate (bytes/sec), `r_mark` the observed mark rate
/// (bytes/sec), and `c` the target GC CPU fraction. The mutator
/// allocates `G` bytes in time `G / r_alloc`; the next major then
/// marks `L` bytes in time `L / r_mark`. For the GC's share of
/// duty-cycle wall-clock to settle at `c`,
///
///   `t_mark / (t_alloc + t_mark) = c`
///   ⇒ `G = L · r_alloc · (1 − c) / (c · r_mark)`.
///
/// Returns 0 (the "skip this constraint" sentinel) when any input is
/// missing or out of range. In particular, the first completed cycle
/// has no allocation-rate sample yet so this helper returns 0 and the
/// other two trigger heuristics decide alone.
fn compute_cpu_aware_growth(
    live_bytes_after: usize,
    alloc_rate_bps: f64,
    mark_rate_bps: f64,
    target_gc_cpu_fraction: f64,
) -> usize {
    if !alloc_rate_bps.is_finite() || alloc_rate_bps <= 0.0 {
        return 0;
    }
    if !mark_rate_bps.is_finite() || mark_rate_bps <= 0.0 {
        return 0;
    }
    if !target_gc_cpu_fraction.is_finite()
        || target_gc_cpu_fraction <= 0.0
        || target_gc_cpu_fraction >= 1.0
    {
        return 0;
    }
    let one_minus_c = 1.0 - target_gc_cpu_fraction;
    let numer = (live_bytes_after as f64) * alloc_rate_bps * one_minus_c;
    let denom = target_gc_cpu_fraction * mark_rate_bps;
    if denom <= 0.0 || !denom.is_finite() {
        return 0;
    }
    let g = numer / denom;
    if !g.is_finite() || g <= 0.0 {
        return 0;
    }
    if g >= (usize::MAX as f64) {
        usize::MAX
    } else {
        g as usize
    }
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
#[path = "pacer_test.rs"]
mod tests;
