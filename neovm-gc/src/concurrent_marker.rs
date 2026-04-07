//! Phase 5 dedicated concurrent-marker scaffold.
//!
//! `ConcurrentMarker` is a thin focused wrapper around the existing
//! [`BackgroundWorker`] infrastructure. It exposes a small, opinionated API
//! for the "spawn one mark thread, wait for it to complete the active major
//! mark, then join" use case that the rest of the runtime needs in order to
//! validate the lock-alternating concurrent mark loop.
//!
//! # Concurrency model: lock-alternating, not lock-free
//!
//! This is **not** a fully lock-free concurrent collector like ZGC or
//! Shenandoah. The marker thread acquires a brief shared (read) lock on the
//! [`SharedHeap`] for each mark slice, runs one bounded slice of work, then
//! drops the lock before sleeping. Mutators that hold the heap write lock
//! during this window see the marker briefly back off via a `WouldBlock`
//! retry; mutators that hold a read lock can proceed in parallel with the
//! marker because both sides only take the read lock.
//!
//! In wall-clock terms the mark thread runs in parallel with the mutator
//! most of the time, but there *are* short interleaved pauses when the
//! mutator decides to take the write lock to allocate, mutate, or finish
//! the cycle. The SATB write barrier (see [`crate::barrier`] and
//! `CollectorRuntime::record_post_write` on the crate-internal side)
//! keeps the pre-mutation snapshot reachable so the marker never loses
//! live edges that the mutator overwrites concurrently.
//!
//! Achieving full lock-free concurrent marking would additionally require:
//!
//! * a colored-pointer or load-barrier strategy so the mutator can safely
//!   read references that the marker might be relocating,
//! * a lock-free worklist (e.g. Chase-Lev deques shared with the mutator),
//! * card or remembered-set updates that do not need the heap lock,
//! * relocation/forwarding tables that mutators can read without taking the
//!   heap lock.
//!
//! Phase 5 deliberately stops short of those changes. The goal here is to
//! prove out a dedicated mark thread driving the existing tri-color marker
//! to completion through brief read-lock slices, with progress observable
//! via the existing [`SharedHeap`] background-status surface.

use crate::background::{
    BackgroundCollectorConfig, BackgroundWorker, BackgroundWorkerConfig, BackgroundWorkerError,
    BackgroundWorkerStats, SharedBackgroundStatus, SharedHeap, SharedHeapError,
};
use std::time::{Duration, Instant};

/// Tunable parameters for one [`ConcurrentMarker`].
///
/// Defaults match the existing [`BackgroundWorker`] tuning that exercises
/// the lock-alternating mark loop in tests: short slices, short busy
/// sleeps, and idle sleeps measured in milliseconds when no session is
/// active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConcurrentMarkerConfig {
    /// Slice budget per mark tick.
    ///
    /// Smaller values give finer interleaving with the mutator and shorter
    /// per-slice read-lock holds, at the cost of more thread wakeups and
    /// more lock acquisition overhead.
    pub mark_slice_budget: usize,
    /// Sleep between mark ticks while a major-mark session has remaining
    /// work.
    pub busy_sleep: Duration,
    /// Sleep between mark ticks when no major-mark session is active.
    pub idle_sleep: Duration,
}

impl Default for ConcurrentMarkerConfig {
    fn default() -> Self {
        Self {
            mark_slice_budget: 64,
            busy_sleep: Duration::from_micros(100),
            idle_sleep: Duration::from_millis(1),
        }
    }
}

impl ConcurrentMarkerConfig {
    fn into_worker_config(self) -> BackgroundWorkerConfig {
        BackgroundWorkerConfig {
            collector: BackgroundCollectorConfig {
                // Phase 5 only drives sessions that the mutator already
                // started. Auto-starting concurrent sessions belongs to the
                // background coordinator, not to the focused mark wrapper.
                auto_start_concurrent: false,
                // The mark thread should drive the session through to the
                // ready-to-finish phase, but the final stop-the-world finish
                // step still belongs to the mutator that called
                // `wait_for_mark_complete`.
                auto_finish_when_ready: false,
                // Each tick drives at most one mark round so the marker
                // releases its read lock quickly enough for the mutator to
                // make progress.
                max_rounds_per_tick: 1,
            },
            idle_sleep: self.idle_sleep,
            busy_sleep: self.busy_sleep,
        }
    }
}

/// Failure modes for [`ConcurrentMarker`] APIs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConcurrentMarkerError {
    /// The wrapped background worker reported an error.
    WorkerError(BackgroundWorkerError),
    /// The marker has been stopped (or its underlying worker has finished)
    /// before the requested operation could complete.
    Stopped,
    /// The requested wait timed out before the active major-mark session
    /// reported completion.
    Timeout,
    /// One shared-heap status read failed because of lock poisoning or
    /// contention with another waiter.
    Heap(SharedHeapError),
}

impl From<BackgroundWorkerError> for ConcurrentMarkerError {
    fn from(error: BackgroundWorkerError) -> Self {
        Self::WorkerError(error)
    }
}

impl From<SharedHeapError> for ConcurrentMarkerError {
    fn from(error: SharedHeapError) -> Self {
        Self::Heap(error)
    }
}

/// Snapshot of [`ConcurrentMarker`] state surfaced to mutator-side callers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConcurrentMarkerStatus {
    /// `true` while a major-mark session is currently active and not yet
    /// drained.
    pub mark_in_progress: bool,
    /// `true` once the active major-mark session has reported `completed`
    /// (i.e. the mark worklist has been fully drained and the cycle is
    /// ready to finish).
    pub mark_complete: bool,
    /// Number of objects drained by the active mark session so far.
    pub drained_objects: usize,
    /// Number of mark steps executed by the active session so far.
    pub mark_steps: u64,
    /// Number of mark rounds executed by the active session so far.
    pub mark_rounds: u64,
}

impl ConcurrentMarkerStatus {
    fn from_background(status: &SharedBackgroundStatus) -> Self {
        let progress = status.major_mark_progress;
        let mark_in_progress = status.active_major_mark_plan.is_some()
            && progress.is_some_and(|progress| !progress.completed);
        let mark_complete = status.active_major_mark_plan.is_some()
            && progress.is_some_and(|progress| progress.completed);
        let drained_objects = progress.map(|p| p.drained_objects).unwrap_or(0);
        let mark_steps = progress.map(|p| p.mark_steps).unwrap_or(0);
        let mark_rounds = progress.map(|p| p.mark_rounds).unwrap_or(0);
        Self {
            mark_in_progress,
            mark_complete,
            drained_objects,
            mark_steps,
            mark_rounds,
        }
    }
}

/// Aggregated statistics returned by [`ConcurrentMarker::join`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConcurrentMarkerStats {
    /// Number of mark-thread ticks executed.
    pub ticks: u64,
    /// Number of major-mark sessions the marker observed transitioning
    /// to the finished state.
    pub sessions_completed: u64,
}

impl ConcurrentMarkerStats {
    fn from_worker(stats: BackgroundWorkerStats) -> Self {
        Self {
            ticks: stats.collector.ticks,
            sessions_completed: stats.collector.sessions_finished,
        }
    }
}

/// Dedicated concurrent-mark thread bound to one [`SharedHeap`].
///
/// Internally `ConcurrentMarker` is a thin wrapper that translates a
/// [`ConcurrentMarkerConfig`] into a [`BackgroundWorkerConfig`], spawns a
/// [`BackgroundWorker`], and re-exposes only the operations the
/// concurrent-mark scaffold needs. The wrapped worker keeps doing what it
/// already does well -- alternating brief read-lock slices on the heap
/// with short sleeps between rounds -- and the wrapper layers a focused
/// "wait for mark complete" API on top.
#[derive(Debug)]
pub struct ConcurrentMarker {
    shared: SharedHeap,
    worker: Option<BackgroundWorker>,
    /// Snapshot of `last_completed_plan` taken when this marker was
    /// spawned. The marker is considered to have driven one cycle to
    /// completion when the shared heap exposes a different major or full
    /// plan as its last completed plan, or when an active session reports
    /// completion directly.
    baseline_completed_plan: Option<crate::plan::CollectionPlan>,
}

impl ConcurrentMarker {
    /// Spawn a dedicated concurrent-mark thread against `shared_heap`.
    ///
    /// The marker thread starts immediately and will service any active
    /// major-mark session created by the mutator. It does **not**
    /// auto-start sessions on its own; the mutator (or a separate
    /// background coordinator) is responsible for calling
    /// `begin_major_mark` first.
    pub fn start(shared_heap: SharedHeap, config: ConcurrentMarkerConfig) -> Self {
        let baseline_completed_plan = shared_heap.last_completed_plan().ok().flatten();
        let worker = shared_heap.spawn_background_worker(config.into_worker_config());
        Self {
            shared: shared_heap,
            worker: Some(worker),
            baseline_completed_plan,
        }
    }

    /// Request the marker to stop after its current mark slice.
    ///
    /// This is non-blocking and idempotent. The marker thread observes the
    /// stop flag at the next sleep wake-up; in-flight slices complete
    /// normally. Use [`ConcurrentMarker::join`] to actually reclaim the
    /// thread.
    pub fn request_stop(&self) {
        if let Some(worker) = self.worker.as_ref() {
            worker.request_stop();
        }
    }

    /// Return a snapshot of marker state observed through the shared heap.
    ///
    /// This intentionally goes through the shared heap status surface and
    /// not through the wrapped worker counters. The mark thread updates
    /// the shared status as it advances, so callers see the same view that
    /// other observers of the heap (e.g. the mutator that wants to call
    /// `finish_major_collection`) would see.
    pub fn status(&self) -> Result<ConcurrentMarkerStatus, ConcurrentMarkerError> {
        let status = self.shared.background_status()?;
        Ok(ConcurrentMarkerStatus::from_background(&status))
    }

    /// Wait up to `timeout` for the active major-mark session to drain
    /// (or for the marker thread to auto-commit a fresh cycle).
    ///
    /// The wait succeeds in either of these states:
    ///
    /// * The shared heap currently shows an active major-mark session
    ///   whose progress reports `completed == true` (the mark worklist is
    ///   fully drained and the cycle is ready to finish in the mutator).
    /// * The shared heap shows a major or full plan as its most recently
    ///   completed plan, and that plan differs from the baseline plan
    ///   captured when this `ConcurrentMarker` was created -- meaning the
    ///   marker thread has auto-committed at least one fresh cycle since
    ///   start time.
    ///
    /// Returns:
    ///
    /// * `Ok(true)` if either of the success conditions above is reached
    ///   before the deadline.
    /// * `Ok(false)` if the timeout elapses first.
    /// * `Err(ConcurrentMarkerError::Stopped)` if the marker has been
    ///   stopped before completion.
    /// * `Err(ConcurrentMarkerError::Heap(_))` if a shared-heap status
    ///   read fails.
    pub fn wait_for_mark_complete(
        &self,
        timeout: Duration,
    ) -> Result<bool, ConcurrentMarkerError> {
        let worker = self
            .worker
            .as_ref()
            .ok_or(ConcurrentMarkerError::Stopped)?;

        let deadline = Instant::now() + timeout;
        let mut observed_epoch = self.shared.background_epoch()?;
        let mut observed_status = self.shared.background_status()?;

        if self.observation_indicates_completion(&observed_status)? {
            return Ok(true);
        }

        loop {
            if worker.is_finished() {
                let final_status = self.shared.background_status()?;
                if self.observation_indicates_completion(&final_status)? {
                    return Ok(true);
                }
                return Err(ConcurrentMarkerError::Stopped);
            }

            let now = Instant::now();
            if now >= deadline {
                // Last chance: re-check directly in case
                // `wait_for_background_change` missed a transition that
                // happened just before the deadline.
                let final_status = self.shared.background_status()?;
                if self.observation_indicates_completion(&final_status)? {
                    return Ok(true);
                }
                return Ok(false);
            }
            let remaining = deadline.saturating_duration_since(now);

            let wait_result = self.shared.wait_for_background_change(
                observed_epoch,
                &observed_status,
                remaining,
            )?;
            observed_epoch = wait_result.next_epoch;
            observed_status = wait_result.status;

            if self.observation_indicates_completion(&observed_status)? {
                return Ok(true);
            }
        }
    }

    /// Determine whether the marker has reached a completed state
    /// relative to the baseline observed at start time.
    ///
    /// The completion test recognizes two distinct success states:
    ///
    /// * The shared heap currently shows an active major-mark session
    ///   whose progress reports `completed == true`.
    /// * The shared heap shows a major or full plan as its most recently
    ///   completed plan, and that plan differs from the baseline plan
    ///   captured when this `ConcurrentMarker` was created -- meaning the
    ///   marker thread (or a coordinated mutator) has committed at least
    ///   one fresh major/full cycle since marker start time.
    fn observation_indicates_completion(
        &self,
        status: &SharedBackgroundStatus,
    ) -> Result<bool, ConcurrentMarkerError> {
        if Self::is_mark_complete(status) {
            return Ok(true);
        }
        let last = self.shared.last_completed_plan()?;
        Ok(Self::reached_committed_cycle(
            &self.baseline_completed_plan,
            &last,
        ))
    }

    fn reached_committed_cycle(
        baseline: &Option<crate::plan::CollectionPlan>,
        current: &Option<crate::plan::CollectionPlan>,
    ) -> bool {
        match (baseline, current) {
            (_, None) => false,
            (None, Some(plan)) => Self::is_major_kind(plan.kind),
            (Some(prev), Some(current)) => Self::is_major_kind(current.kind) && current != prev,
        }
    }

    fn is_major_kind(kind: crate::plan::CollectionKind) -> bool {
        matches!(
            kind,
            crate::plan::CollectionKind::Major | crate::plan::CollectionKind::Full
        )
    }

    fn is_mark_complete(status: &SharedBackgroundStatus) -> bool {
        status.active_major_mark_plan.is_some()
            && status
                .major_mark_progress
                .is_some_and(|progress| progress.completed)
    }

    /// Return whether the marker thread has already terminated.
    pub fn is_finished(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(BackgroundWorker::is_finished)
    }

    /// Stop the marker, join its thread, and return aggregated stats.
    ///
    /// Returns `Err(ConcurrentMarkerError::WorkerError(_))` if the
    /// background worker thread itself returned an error or panicked.
    pub fn join(mut self) -> Result<ConcurrentMarkerStats, ConcurrentMarkerError> {
        let Some(worker) = self.worker.take() else {
            return Err(ConcurrentMarkerError::Stopped);
        };
        let stats = worker.join()?;
        Ok(ConcurrentMarkerStats::from_worker(stats))
    }
}

impl Drop for ConcurrentMarker {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            worker.request_stop();
            let _ = worker.join();
        }
    }
}
