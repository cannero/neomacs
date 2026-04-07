use crate::collector_state::{CollectorSharedSnapshot, CollectorState, CollectorStateHandle};
use crate::heap::{AllocError, Heap};
use crate::mutator::Mutator;
use crate::pacer::PacerStats;
use crate::pause_stats::PauseHistogram;
use crate::plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
use crate::runtime::{CollectorRuntime, SharedCollectorRuntime};
use crate::runtime_state::{RuntimeState, RuntimeStateHandle};
use crate::stats::{CollectionStats, HeapStats};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{
    Arc, Condvar, LockResult, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard, TryLockError,
    TryLockResult,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Collector-side runtime surface required by the background coordinator.
pub trait BackgroundCollectionRuntime {
    /// Return the active major-mark plan, if one is in progress.
    fn active_major_mark_plan(&self) -> Option<crate::plan::CollectionPlan>;

    /// Recommend the next background concurrent collection plan, if any.
    fn recommended_background_plan(&self) -> Option<crate::plan::CollectionPlan>;

    /// Begin a persistent major-mark session for one scheduler-provided plan.
    fn begin_major_mark(&mut self, plan: crate::plan::CollectionPlan) -> Result<(), AllocError>;

    /// Advance one background mark round for the active major-mark session.
    fn poll_background_mark_round(
        &mut self,
    ) -> Result<Option<crate::plan::MajorMarkProgress>, AllocError>;

    /// Prepare reclaim for the active major collection once mark work is fully drained.
    fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError>;

    /// Commit the active major collection once reclaim has already been prepared.
    fn commit_active_reclaim_if_ready(&mut self) -> Result<Option<CollectionStats>, AllocError>;

    /// Finish the active major collection if its mark work is fully drained.
    fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError>;
}

/// Background collector coordinator configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundCollectorConfig {
    /// Whether concurrent major/full plans may be auto-started from heap pressure.
    pub auto_start_concurrent: bool,
    /// Whether a tick should immediately enter the final stop-the-world finish phase once
    /// concurrent marking is fully drained.
    pub auto_finish_when_ready: bool,
    /// Maximum background service rounds executed in one coordinator tick.
    pub max_rounds_per_tick: usize,
}

impl Default for BackgroundCollectorConfig {
    fn default() -> Self {
        Self {
            auto_start_concurrent: true,
            auto_finish_when_ready: true,
            max_rounds_per_tick: 1,
        }
    }
}

/// Runtime statistics for one background collector coordinator.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BackgroundCollectorStats {
    /// Number of coordinator ticks executed.
    pub ticks: u64,
    /// Number of background service rounds executed.
    pub rounds: u64,
    /// Number of concurrent sessions auto-started by the coordinator.
    pub sessions_started: u64,
    /// Number of concurrent sessions finished by the coordinator.
    pub sessions_finished: u64,
}

/// Shared synchronized heap wrapper for worker-owned collector services.
#[derive(Clone, Debug)]
pub struct SharedHeap {
    inner: Arc<RwLock<Heap>>,
    runtime: SharedRuntimeHandle,
    collector: SharedCollectorHandle,
}

/// Public snapshot of shared heap state that can be read without taking the main heap mutex.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedHeapStatus {
    /// Current heap statistics.
    pub stats: HeapStats,
    /// Adaptive pacer statistics (allocation rate, mark rate, next
    /// trigger threshold, observed cycles, and overshoot count) as
    /// captured in the latest shared snapshot.
    pub pacer: PacerStats,
    /// Rolling pause-time histogram (P50/P95/P99 nanoseconds over a
    /// bounded window) as captured in the latest shared snapshot.
    pub pauses: PauseHistogram,
    /// Runtime-side follow-up work that remains outside GC commit.
    pub runtime_work: RuntimeWorkStatus,
    /// Scheduler-visible recommended collection plan from the latest shared snapshot.
    pub recommended_plan: CollectionPlan,
    /// Background collector recommendation from the latest shared snapshot.
    pub recommended_background_plan: Option<CollectionPlan>,
    /// Most recently completed collection plan.
    pub last_completed_plan: Option<CollectionPlan>,
    /// Active major-mark plan, if any.
    pub active_major_mark_plan: Option<CollectionPlan>,
    /// Active major-mark progress, if any.
    pub major_mark_progress: Option<MajorMarkProgress>,
}

/// Public snapshot of one shared background service and its backing heap state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedBackgroundServiceStatus {
    /// Current background collector coordinator statistics.
    pub collector: BackgroundCollectorStats,
    /// Current shared heap snapshot.
    pub heap: SharedHeapStatus,
}

/// Public snapshot of background-collector-visible shared heap state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedBackgroundStatus {
    /// Background collector recommendation from the latest shared snapshot.
    pub recommended_background_plan: Option<CollectionPlan>,
    /// Active major-mark plan, if any.
    pub active_major_mark_plan: Option<CollectionPlan>,
    /// Active major-mark progress, if any.
    pub major_mark_progress: Option<MajorMarkProgress>,
    /// Runtime-side follow-up work that remains outside GC commit.
    pub runtime_work: RuntimeWorkStatus,
    /// Number of queued finalizers waiting to run.
    pub pending_finalizers: usize,
}

/// One consistent observation of background epoch and background-visible shared heap state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedBackgroundObservation {
    /// Background-state change epoch associated with this observation.
    pub epoch: u64,
    /// Background-collector-visible state observed at that epoch.
    pub status: SharedBackgroundStatus,
}

/// Result of waiting for one background-collector-visible shared heap state change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedBackgroundWaitResult {
    /// Background-state change epoch observed at the end of the wait.
    pub next_epoch: u64,
    /// Whether at least one background-state signal advanced the epoch during the wait.
    pub signal_changed: bool,
    /// Whether background-collector-visible state changed during the wait.
    pub background_changed: bool,
    /// Background-collector-visible state observed at the end of the wait.
    pub status: SharedBackgroundStatus,
}

/// Shared-heap failure modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedHeapError {
    /// Shared heap state was poisoned by another panic.
    LockPoisoned,
    /// Shared heap state is currently locked by another thread.
    WouldBlock,
}

/// Shared heap access failure modes with a snapshot-backed contention state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SharedHeapAccessError {
    /// Shared heap state was poisoned by another panic.
    LockPoisoned,
    /// Shared heap state is currently locked by another thread.
    WouldBlock(SharedHeapStatus),
}

/// Shared background service failure modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedBackgroundError {
    /// Shared heap state was poisoned by another panic.
    LockPoisoned,
    /// Shared heap state is currently locked by another thread.
    WouldBlock,
    /// The collector reported one collection/runtime error.
    Collection(AllocError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SharedHeapSnapshot {
    stats: HeapStats,
    pacer: PacerStats,
    pauses: PauseHistogram,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SharedRuntimeSnapshot {
    finalizers_run: u64,
    pending_finalizers: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct SharedCollectorHandle {
    state: CollectorStateHandle,
    snapshot: Arc<RwLock<CollectorSharedSnapshot>>,
    signal: Arc<SharedHeapSignal>,
}

#[derive(Clone, Debug)]
pub(crate) struct SharedRuntimeHandle {
    state: RuntimeStateHandle,
    snapshot: Arc<RwLock<SharedRuntimeSnapshot>>,
    heap_snapshot: Arc<RwLock<SharedHeapSnapshot>>,
    heap_signal: Arc<SharedHeapSignal>,
    collector: SharedCollectorHandle,
}

#[derive(Debug, Default)]
struct SharedHeapSignal {
    epoch: AtomicU64,
    wait_lock: Mutex<()>,
    cv: Condvar,
}

impl SharedHeapSignal {
    fn current_epoch(&self) -> Result<u64, SharedHeapError> {
        Ok(self.epoch.load(Ordering::Acquire))
    }

    fn notify(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
        self.cv.notify_all();
    }

    fn wait_for_change(
        &self,
        observed_epoch: u64,
        timeout: Duration,
    ) -> Result<(u64, bool), SharedHeapError> {
        if timeout.is_zero() {
            return Ok((observed_epoch, false));
        }

        let wait_guard = self
            .wait_lock
            .lock()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        if self.epoch.load(Ordering::Acquire) != observed_epoch {
            let next_epoch = self.epoch.load(Ordering::Acquire);
            return Ok((next_epoch, true));
        }
        let (_wait_guard, _) = self
            .cv
            .wait_timeout_while(wait_guard, timeout, |_| {
                self.epoch.load(Ordering::Acquire) == observed_epoch
            })
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let next_epoch = self.epoch.load(Ordering::Acquire);
        Ok((next_epoch, next_epoch != observed_epoch))
    }
}

impl SharedHeapSnapshot {
    fn capture(heap: &Heap) -> Self {
        Self {
            stats: heap.storage_stats(),
            pacer: heap.pacer_stats(),
            pauses: heap.pause_histogram(),
        }
    }
}

impl SharedRuntimeSnapshot {
    fn capture(heap: &Heap) -> Self {
        let (finalizers_run, pending_finalizers) = heap.runtime_finalizer_stats();
        Self {
            finalizers_run,
            pending_finalizers,
        }
    }
}

impl SharedCollectorHandle {
    fn new(
        state: CollectorStateHandle,
        snapshot: CollectorSharedSnapshot,
        signal: Arc<SharedHeapSignal>,
    ) -> Self {
        Self {
            state,
            snapshot: Arc::new(RwLock::new(snapshot)),
            signal,
        }
    }

    pub(crate) fn with_state<R>(
        &self,
        f: impl FnOnce(&mut CollectorState) -> R,
    ) -> Result<R, SharedHeapError> {
        Ok(self.state.with_state(f))
    }

    pub(crate) fn try_with_state<R>(
        &self,
        f: impl FnOnce(&mut CollectorState) -> R,
    ) -> Result<R, SharedHeapError> {
        self.state.try_with_state(f).map_err(|error| match error {
            TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })
    }

    pub(crate) fn read_snapshot<R>(
        &self,
        f: impl FnOnce(&CollectorSharedSnapshot) -> R,
    ) -> Result<R, SharedHeapError> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&snapshot))
    }

    pub(crate) fn snapshot(&self) -> Result<CollectorSharedSnapshot, SharedHeapError> {
        self.read_snapshot(Clone::clone)
    }

    pub(crate) fn state_snapshot(&self) -> CollectorSharedSnapshot {
        self.state.shared_snapshot()
    }

    pub(crate) fn active_reclaim_prep_request(
        &self,
    ) -> Option<crate::collector_session::ActiveReclaimPrepRequest> {
        self.state.active_reclaim_prep_request()
    }

    pub(crate) fn prepare_active_collection_reclaim_with_request_and_refresh(
        &self,
        request: crate::collector_session::ActiveReclaimPrepRequest,
        objects: &[crate::object::ObjectRecord],
        index: &crate::index_state::ObjectIndex,
        trace_ephemerons: impl FnOnce(
            &mut crate::collector_exec::MarkTracer<'_>,
            &crate::plan::CollectionPlan,
        ) -> (u64, u64),
        prepare_reclaim: impl FnOnce(
            &crate::plan::CollectionPlan,
        )
            -> Result<crate::reclaim::PreparedReclaim, crate::heap::AllocError>,
        stats: &crate::stats::HeapStats,
        old_gen: &crate::spaces::OldGenState,
        old_config: &crate::spaces::OldGenConfig,
        plan_for: impl FnMut(crate::plan::CollectionKind) -> crate::plan::CollectionPlan,
    ) -> Result<bool, crate::heap::AllocError> {
        self.state
            .prepare_active_collection_reclaim_with_request_and_refresh(
                request,
                objects,
                index,
                trace_ephemerons,
                prepare_reclaim,
                stats,
                old_gen,
                old_config,
                plan_for,
            )
    }

    fn publish_snapshot(
        &self,
        next_collector: CollectorSharedSnapshot,
        heap_snapshot: &SharedHeapSnapshot,
        runtime_snapshot: &SharedRuntimeSnapshot,
    ) -> Result<(), SharedHeapError> {
        let background_changed = {
            let mut collector_snapshot = self
                .snapshot
                .write()
                .map_err(|_| SharedHeapError::LockPoisoned)?;
            let changed = shared_background_status_from_parts(
                heap_snapshot,
                runtime_snapshot,
                &*collector_snapshot,
            ) != shared_background_status_from_parts(
                heap_snapshot,
                runtime_snapshot,
                &next_collector,
            );
            *collector_snapshot = next_collector;
            changed
        };
        if background_changed {
            self.signal.notify();
        }
        Ok(())
    }

    fn store_snapshot(
        &self,
        next_collector: CollectorSharedSnapshot,
    ) -> Result<(), SharedHeapError> {
        let mut collector_snapshot = self
            .snapshot
            .write()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        *collector_snapshot = next_collector;
        Ok(())
    }

    pub(crate) fn epoch(&self) -> Result<u64, SharedHeapError> {
        self.signal.current_epoch()
    }

    pub(crate) fn wait_for_change(
        &self,
        observed_epoch: u64,
        timeout: Duration,
    ) -> Result<(u64, bool), SharedHeapError> {
        self.signal.wait_for_change(observed_epoch, timeout)
    }

    pub(crate) fn notify(&self) {
        self.signal.notify();
    }
}

impl SharedRuntimeHandle {
    fn new(
        state: RuntimeStateHandle,
        snapshot: SharedRuntimeSnapshot,
        heap_snapshot: Arc<RwLock<SharedHeapSnapshot>>,
        heap_signal: Arc<SharedHeapSignal>,
        collector: SharedCollectorHandle,
    ) -> Self {
        Self {
            state,
            snapshot: Arc::new(RwLock::new(snapshot)),
            heap_snapshot,
            heap_signal,
            collector,
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut RuntimeState) -> R) -> Result<R, SharedHeapError> {
        Ok(self.state.with_state(f))
    }

    fn try_with_state<R>(
        &self,
        f: impl FnOnce(&mut RuntimeState) -> R,
    ) -> Result<R, SharedHeapError> {
        self.state.try_with_state(f).map_err(|error| match error {
            TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })
    }

    fn read_snapshot<R>(
        &self,
        f: impl FnOnce(&SharedRuntimeSnapshot) -> R,
    ) -> Result<R, SharedHeapError> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&snapshot))
    }

    fn read_heap_snapshot<R>(
        &self,
        f: impl FnOnce(&SharedHeapSnapshot) -> R,
    ) -> Result<R, SharedHeapError> {
        let snapshot = self
            .heap_snapshot
            .read()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&snapshot))
    }

    fn replace_heap_snapshot(
        &self,
        next_heap_snapshot: SharedHeapSnapshot,
    ) -> Result<SharedHeapSnapshot, SharedHeapError> {
        let mut snapshot = self
            .heap_snapshot
            .write()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let previous = snapshot.clone();
        *snapshot = next_heap_snapshot;
        Ok(previous)
    }

    fn replace_snapshot(
        &self,
        next_runtime_snapshot: SharedRuntimeSnapshot,
    ) -> Result<SharedRuntimeSnapshot, SharedHeapError> {
        let mut snapshot = self
            .snapshot
            .write()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let previous = *snapshot;
        *snapshot = next_runtime_snapshot;
        Ok(previous)
    }

    fn publish_snapshot(
        &self,
        next_runtime_snapshot: SharedRuntimeSnapshot,
    ) -> Result<(), SharedHeapError> {
        let previous_runtime_snapshot = self.replace_snapshot(next_runtime_snapshot)?;
        let runtime_changed = previous_runtime_snapshot != next_runtime_snapshot;
        let background_changed = {
            let heap_snapshot = self
                .heap_snapshot
                .read()
                .map_err(|_| SharedHeapError::LockPoisoned)?;
            let collector_snapshot = self.collector.snapshot()?;
            shared_background_status_from_parts(
                &heap_snapshot,
                &previous_runtime_snapshot,
                &collector_snapshot,
            ) != shared_background_status_from_parts(
                &heap_snapshot,
                &next_runtime_snapshot,
                &collector_snapshot,
            )
        };
        if runtime_changed {
            self.heap_signal.notify();
        }
        if background_changed {
            self.collector.notify();
        }
        Ok(())
    }

    pub(crate) fn publish_collector_snapshot(
        &self,
        next_collector: CollectorSharedSnapshot,
    ) -> Result<(), SharedHeapError> {
        let heap_snapshot = self.read_heap_snapshot(Clone::clone)?;
        let runtime_snapshot = self.read_snapshot(|snapshot| *snapshot)?;
        self.collector
            .publish_snapshot(next_collector, &heap_snapshot, &runtime_snapshot)
    }

    pub(crate) fn heap_epoch(&self) -> Result<u64, SharedHeapError> {
        self.heap_signal.current_epoch()
    }

    pub(crate) fn wait_for_heap_change(
        &self,
        observed_epoch: u64,
        timeout: Duration,
    ) -> Result<(u64, bool), SharedHeapError> {
        self.heap_signal.wait_for_change(observed_epoch, timeout)
    }

    pub(crate) fn notify_heap(&self) {
        self.heap_signal.notify();
    }

    pub(crate) fn pending_finalizer_count(&self) -> Result<usize, SharedHeapError> {
        self.read_snapshot(|snapshot| snapshot.pending_finalizers)
    }

    pub(crate) fn runtime_work_status(&self) -> Result<RuntimeWorkStatus, SharedHeapError> {
        self.read_snapshot(|snapshot| {
            RuntimeWorkStatus::from_pending_finalizers(snapshot.pending_finalizers)
        })
    }

    pub(crate) fn observe_heap_status(&self) -> Result<SharedHeapStatus, SharedHeapError> {
        loop {
            let before_epoch = self.heap_epoch()?;
            let heap_snapshot = self.read_heap_snapshot(Clone::clone)?;
            let runtime_snapshot = self.read_snapshot(|snapshot| *snapshot)?;
            let collector = self.collector.snapshot()?;
            let after_epoch = self.heap_epoch()?;
            if before_epoch == after_epoch {
                return Ok(shared_heap_status_from_parts(
                    &heap_snapshot,
                    &runtime_snapshot,
                    &collector,
                ));
            }
        }
    }

    pub(crate) fn observe_background_status(
        &self,
    ) -> Result<SharedBackgroundStatus, SharedHeapError> {
        self.observe_background_status_with_epoch()
            .map(|(_, status)| status)
    }

    pub(crate) fn observe_background_status_with_epoch(
        &self,
    ) -> Result<(u64, SharedBackgroundStatus), SharedHeapError> {
        loop {
            let before_epoch = self.collector.epoch()?;
            let heap_snapshot = self.read_heap_snapshot(Clone::clone)?;
            let runtime_snapshot = self.read_snapshot(|snapshot| *snapshot)?;
            let collector_snapshot = self.collector.snapshot()?;
            let status = shared_background_status_from_parts(
                &heap_snapshot,
                &runtime_snapshot,
                &collector_snapshot,
            );
            let after_epoch = self.collector.epoch()?;
            if before_epoch == after_epoch {
                return Ok((after_epoch, status));
            }
        }
    }

    pub(crate) fn wait_for_background_change(
        &self,
        observed_epoch: &mut u64,
        observed_status: &mut SharedBackgroundStatus,
        timeout: Duration,
        stop: Option<&AtomicBool>,
    ) -> Result<SharedBackgroundWaitResult, SharedHeapError> {
        if timeout.is_zero() {
            return Ok(SharedBackgroundWaitResult {
                next_epoch: *observed_epoch,
                signal_changed: false,
                background_changed: false,
                status: observed_status.clone(),
            });
        }

        let started_at = std::time::Instant::now();
        let mut remaining = timeout;
        let mut signal_changed = false;
        loop {
            let (next_epoch, changed) =
                self.collector.wait_for_change(*observed_epoch, remaining)?;
            *observed_epoch = next_epoch;
            signal_changed |= changed;

            if stop.is_some_and(|stop| stop.load(Ordering::Acquire)) {
                return Ok(SharedBackgroundWaitResult {
                    next_epoch,
                    signal_changed,
                    background_changed: false,
                    status: observed_status.clone(),
                });
            }

            let next_status = self.observe_background_status()?;
            if next_status != *observed_status {
                *observed_status = next_status.clone();
                return Ok(SharedBackgroundWaitResult {
                    next_epoch,
                    signal_changed,
                    background_changed: true,
                    status: next_status,
                });
            }

            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                return Ok(SharedBackgroundWaitResult {
                    next_epoch,
                    signal_changed,
                    background_changed: false,
                    status: next_status,
                });
            }
            remaining = timeout.saturating_sub(elapsed);
        }
    }

    pub(crate) fn drain_pending_finalizers(&self) -> Result<u64, SharedHeapError> {
        let (ran, next_runtime_snapshot) = self.with_state(|runtime_state| {
            let ran = runtime_state.drain_pending_finalizers();
            let (finalizers_run, pending_finalizers) = runtime_state.snapshot();
            (
                ran,
                SharedRuntimeSnapshot {
                    finalizers_run,
                    pending_finalizers,
                },
            )
        })?;
        self.publish_snapshot(next_runtime_snapshot)?;
        Ok(ran)
    }

    pub(crate) fn try_drain_pending_finalizers(&self) -> Result<u64, SharedHeapError> {
        let (ran, next_runtime_snapshot) = self.try_with_state(|runtime_state| {
            let ran = runtime_state.drain_pending_finalizers();
            let (finalizers_run, pending_finalizers) = runtime_state.snapshot();
            (
                ran,
                SharedRuntimeSnapshot {
                    finalizers_run,
                    pending_finalizers,
                },
            )
        })?;
        self.publish_snapshot(next_runtime_snapshot)?;
        Ok(ran)
    }

    fn publish_heap_change(
        &self,
        next_heap_snapshot: SharedHeapSnapshot,
        next_runtime_snapshot: SharedRuntimeSnapshot,
    ) -> Result<(), SharedHeapError> {
        let next_collector = self
            .collector
            .with_state(|collector| collector.shared_snapshot())?;
        let previous_collector = self
            .collector
            .snapshot()
            .unwrap_or_else(|_| next_collector.clone());
        let previous_heap_snapshot = self.replace_heap_snapshot(next_heap_snapshot.clone())?;
        let previous_runtime_snapshot = self.replace_snapshot(next_runtime_snapshot)?;
        self.collector.store_snapshot(next_collector.clone())?;

        let heap_changed = previous_heap_snapshot != next_heap_snapshot;
        let runtime_changed = previous_runtime_snapshot != next_runtime_snapshot;
        let background_changed = shared_background_status_from_parts(
            &previous_heap_snapshot,
            &previous_runtime_snapshot,
            &previous_collector,
        ) != shared_background_status_from_parts(
            &next_heap_snapshot,
            &next_runtime_snapshot,
            &next_collector,
        );

        if heap_changed {
            self.notify_heap();
        }
        if runtime_changed {
            self.notify_heap();
        }
        if background_changed {
            self.collector.notify();
        }
        Ok(())
    }
}

fn shared_heap_stats_from_parts(
    heap_snapshot: &SharedHeapSnapshot,
    runtime_snapshot: &SharedRuntimeSnapshot,
) -> HeapStats {
    let mut stats = heap_snapshot.stats;
    stats.finalizers_run = runtime_snapshot.finalizers_run;
    stats.pending_finalizers = runtime_snapshot.pending_finalizers;
    stats
}

fn shared_heap_status_from_parts(
    heap_snapshot: &SharedHeapSnapshot,
    runtime_snapshot: &SharedRuntimeSnapshot,
    collector_snapshot: &CollectorSharedSnapshot,
) -> SharedHeapStatus {
    let stats = shared_heap_stats_from_parts(heap_snapshot, runtime_snapshot);
    SharedHeapStatus {
        stats,
        pacer: heap_snapshot.pacer,
        pauses: heap_snapshot.pauses,
        runtime_work: RuntimeWorkStatus::from_pending_finalizers(
            runtime_snapshot.pending_finalizers,
        ),
        recommended_plan: collector_snapshot.recommended_plan.clone(),
        recommended_background_plan: collector_snapshot.recommended_background_plan.clone(),
        last_completed_plan: collector_snapshot.last_completed_plan.clone(),
        active_major_mark_plan: collector_snapshot.active_major_mark_plan.clone(),
        major_mark_progress: collector_snapshot.major_mark_progress,
    }
}

fn shared_background_status_from_parts(
    _heap_snapshot: &SharedHeapSnapshot,
    runtime_snapshot: &SharedRuntimeSnapshot,
    collector_snapshot: &CollectorSharedSnapshot,
) -> SharedBackgroundStatus {
    SharedBackgroundStatus {
        recommended_background_plan: collector_snapshot.recommended_background_plan.clone(),
        active_major_mark_plan: collector_snapshot.active_major_mark_plan.clone(),
        major_mark_progress: collector_snapshot.major_mark_progress,
        runtime_work: RuntimeWorkStatus::from_pending_finalizers(
            runtime_snapshot.pending_finalizers,
        ),
        pending_finalizers: runtime_snapshot.pending_finalizers,
    }
}

/// Guard returned by `SharedHeap::lock()` and `SharedHeap::try_lock()`.
#[derive(Debug)]
pub struct SharedHeapGuard<'a> {
    guard: Option<RwLockWriteGuard<'a, Heap>>,
    runtime: &'a SharedRuntimeHandle,
    dirty: bool,
}

/// Guard returned by `SharedHeap::read()` and `SharedHeap::try_read()`.
#[derive(Debug)]
pub struct SharedHeapReadGuard<'a> {
    guard: RwLockReadGuard<'a, Heap>,
}

impl<'a> SharedHeapGuard<'a> {
    fn new(guard: RwLockWriteGuard<'a, Heap>, runtime: &'a SharedRuntimeHandle) -> Self {
        Self {
            guard: Some(guard),
            runtime,
            dirty: false,
        }
    }
}

impl Deref for SharedHeapReadGuard<'_> {
    type Target = Heap;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl Deref for SharedHeapGuard<'_> {
    type Target = Heap;

    fn deref(&self) -> &Self::Target {
        self.guard
            .as_deref()
            .expect("shared heap guard should hold heap lock")
    }
}

impl DerefMut for SharedHeapGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.dirty = true;
        self.guard
            .as_deref_mut()
            .expect("shared heap guard should hold heap lock")
    }
}

impl Drop for SharedHeapGuard<'_> {
    fn drop(&mut self) {
        if !self.dirty {
            return;
        }
        let (next_snapshot, next_runtime_snapshot) = {
            let heap = self
                .guard
                .as_deref()
                .expect("shared heap guard should hold heap lock");
            (
                SharedHeapSnapshot::capture(heap),
                SharedRuntimeSnapshot::capture(heap),
            )
        };
        // Release the heap mutex before touching shared snapshot locks so readers do not extend
        // the main heap lock window.
        self.guard.take();
        let _ = self
            .runtime
            .publish_heap_change(next_snapshot, next_runtime_snapshot);
    }
}

impl SharedHeap {
    /// Create a new shared heap with `config`.
    pub fn new(config: crate::heap::HeapConfig) -> Self {
        Self::from_heap(Heap::new(config))
    }

    /// Wrap one heap for shared synchronized access.
    pub fn from_heap(heap: Heap) -> Self {
        let snapshot = SharedHeapSnapshot::capture(&heap);
        let runtime_snapshot = SharedRuntimeSnapshot::capture(&heap);
        let collector_snapshot = heap.collector_shared_snapshot();
        let runtime_state = heap.runtime_state_handle();
        let collector_state = heap.collector_handle();
        let background_signal = Arc::new(SharedHeapSignal::default());
        let heap_signal = Arc::new(SharedHeapSignal::default());
        let snapshot = Arc::new(RwLock::new(snapshot));
        let collector =
            SharedCollectorHandle::new(collector_state, collector_snapshot, background_signal);
        let runtime = SharedRuntimeHandle::new(
            runtime_state,
            runtime_snapshot,
            snapshot.clone(),
            heap_signal.clone(),
            collector.clone(),
        );
        Self {
            inner: Arc::new(RwLock::new(heap)),
            runtime,
            collector,
        }
    }

    /// Lock the underlying heap.
    pub fn lock(&self) -> LockResult<SharedHeapGuard<'_>> {
        match self.inner.write() {
            Ok(guard) => Ok(SharedHeapGuard::new(guard, &self.runtime)),
            Err(error) => Err(std::sync::PoisonError::new(SharedHeapGuard::new(
                error.into_inner(),
                &self.runtime,
            ))),
        }
    }

    /// Try to lock the underlying heap without blocking.
    pub fn try_lock(&self) -> TryLockResult<SharedHeapGuard<'_>> {
        match self.inner.try_write() {
            Ok(guard) => Ok(SharedHeapGuard::new(guard, &self.runtime)),
            Err(TryLockError::Poisoned(error)) => {
                Err(TryLockError::Poisoned(std::sync::PoisonError::new(
                    SharedHeapGuard::new(error.into_inner(), &self.runtime),
                )))
            }
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }

    /// Acquire a shared read guard for the underlying heap.
    pub fn read(&self) -> LockResult<SharedHeapReadGuard<'_>> {
        self.inner
            .read()
            .map(|guard| SharedHeapReadGuard { guard })
            .map_err(|error| {
                std::sync::PoisonError::new(SharedHeapReadGuard {
                    guard: error.into_inner(),
                })
            })
    }

    /// Try to acquire a shared read guard for the underlying heap without blocking.
    pub fn try_read(&self) -> TryLockResult<SharedHeapReadGuard<'_>> {
        self.inner
            .try_read()
            .map(|guard| SharedHeapReadGuard { guard })
            .map_err(|error| match error {
                TryLockError::Poisoned(error) => {
                    TryLockError::Poisoned(std::sync::PoisonError::new(SharedHeapReadGuard {
                        guard: error.into_inner(),
                    }))
                }
                TryLockError::WouldBlock => TryLockError::WouldBlock,
            })
    }

    /// Execute one closure with exclusive access to the underlying heap.
    pub fn with_heap<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> Result<R, SharedHeapError> {
        let mut heap = self.lock().map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&mut heap))
    }

    /// Execute one closure with shared read access to the underlying heap.
    pub fn with_heap_read<R>(&self, f: impl FnOnce(&Heap) -> R) -> Result<R, SharedHeapError> {
        let heap = self.read().map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&heap))
    }

    /// Execute one closure with exclusive access to the underlying heap without blocking.
    pub fn try_with_heap<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> Result<R, SharedHeapError> {
        let mut heap = self.try_lock().map_err(|error| match error {
            TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        Ok(f(&mut heap))
    }

    /// Execute one closure with shared read access to the underlying heap without blocking.
    pub fn try_with_heap_read<R>(&self, f: impl FnOnce(&Heap) -> R) -> Result<R, SharedHeapError> {
        let heap = self.try_read().map_err(|error| match error {
            TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        Ok(f(&heap))
    }

    /// Execute one closure with exclusive access to the underlying heap without blocking.
    ///
    /// If the heap is contended, returns the latest shared snapshot instead of a bare
    /// `WouldBlock`, so callers can react based on current heap/background state.
    pub fn try_with_heap_status<R>(
        &self,
        f: impl FnOnce(&mut Heap) -> R,
    ) -> Result<R, SharedHeapAccessError> {
        match self.try_lock() {
            Ok(mut heap) => Ok(f(&mut heap)),
            Err(TryLockError::Poisoned(_)) => Err(SharedHeapAccessError::LockPoisoned),
            Err(TryLockError::WouldBlock) => Err(SharedHeapAccessError::WouldBlock(
                self.status()
                    .map_err(|_| SharedHeapAccessError::LockPoisoned)?,
            )),
        }
    }

    pub(crate) fn collector_handle(&self) -> SharedCollectorHandle {
        self.collector.clone()
    }

    pub(crate) fn runtime_handle(&self) -> SharedRuntimeHandle {
        self.runtime.clone()
    }

    /// Return the current shared-heap change epoch used by signal-backed waiters.
    pub fn epoch(&self) -> Result<u64, SharedHeapError> {
        self.runtime.heap_epoch()
    }

    /// Return the current background-state change epoch used by background waiters.
    pub fn background_epoch(&self) -> Result<u64, SharedHeapError> {
        self.collector_runtime()
            .background_epoch()
            .map_err(|error| match error {
                SharedBackgroundError::LockPoisoned => SharedHeapError::LockPoisoned,
                SharedBackgroundError::WouldBlock => SharedHeapError::WouldBlock,
                SharedBackgroundError::Collection(_) => SharedHeapError::LockPoisoned,
            })
    }

    /// Wait for the shared-heap change epoch to advance or for `timeout` to elapse.
    ///
    /// The returned tuple is `(next_epoch, changed)`, where `changed` reports whether one real
    /// heap mutation or worker-facing signal advanced the epoch before the timeout elapsed.
    pub fn wait_for_change(
        &self,
        observed_epoch: u64,
        timeout: Duration,
    ) -> Result<(u64, bool), SharedHeapError> {
        self.runtime.wait_for_heap_change(observed_epoch, timeout)
    }

    /// Wait for one background-collector-visible shared heap state change or for `timeout` to
    /// elapse.
    pub fn wait_for_background_change(
        &self,
        observed_epoch: u64,
        observed_status: &SharedBackgroundStatus,
        timeout: Duration,
    ) -> Result<SharedBackgroundWaitResult, SharedHeapError> {
        let mut observed_epoch = observed_epoch;
        let mut observed_status = observed_status.clone();
        self.runtime.wait_for_background_change(
            &mut observed_epoch,
            &mut observed_status,
            timeout,
            None,
        )
    }

    /// Execute one closure with exclusive access to a mutator bound to this heap.
    pub fn with_mutator<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut Mutator<'heap>) -> R,
    ) -> Result<R, SharedHeapError> {
        self.with_heap(|heap| {
            let mut mutator = heap.mutator();
            f(&mut mutator)
        })
    }

    /// Execute one closure with exclusive access to a mutator bound to this heap without
    /// blocking.
    pub fn try_with_mutator<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut Mutator<'heap>) -> R,
    ) -> Result<R, SharedHeapError> {
        self.try_with_heap(|heap| {
            let mut mutator = heap.mutator();
            f(&mut mutator)
        })
    }

    /// Execute one closure with exclusive access to a mutator bound to this heap without
    /// blocking, returning a snapshot-backed contention state on lock miss.
    pub fn try_with_mutator_status<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut Mutator<'heap>) -> R,
    ) -> Result<R, SharedHeapAccessError> {
        self.try_with_heap_status(|heap| {
            let mut mutator = heap.mutator();
            f(&mut mutator)
        })
    }

    /// Execute one closure with exclusive access to a collector runtime bound to this heap.
    pub fn with_runtime<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut CollectorRuntime<'heap>) -> R,
    ) -> Result<R, SharedHeapError> {
        self.with_heap(|heap| {
            let mut runtime = heap.collector_runtime();
            f(&mut runtime)
        })
    }

    /// Execute one closure with exclusive access to a collector runtime bound to this heap
    /// without blocking.
    pub fn try_with_runtime<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut CollectorRuntime<'heap>) -> R,
    ) -> Result<R, SharedHeapError> {
        self.try_with_heap(|heap| {
            let mut runtime = heap.collector_runtime();
            f(&mut runtime)
        })
    }

    /// Execute one closure with exclusive access to a collector runtime bound to this heap
    /// without blocking, returning a snapshot-backed contention state on lock miss.
    pub fn try_with_runtime_status<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut CollectorRuntime<'heap>) -> R,
    ) -> Result<R, SharedHeapAccessError> {
        self.try_with_heap_status(|heap| {
            let mut runtime = heap.collector_runtime();
            f(&mut runtime)
        })
    }

    /// Create a shared collector-side runtime bound to this heap.
    pub fn collector_runtime(&self) -> SharedCollectorRuntime {
        SharedCollectorRuntime::new(self.clone())
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> Result<HeapStats, SharedHeapError> {
        self.runtime
            .observe_heap_status()
            .map(|status| status.stats)
    }

    /// Return the adaptive pacer's current model snapshot.
    ///
    /// Reads from the same lock-free shared snapshot as
    /// [`SharedHeap::stats`], so this never blocks on the heap
    /// mutex even while a mutator or background worker holds it.
    pub fn pacer_stats(&self) -> Result<PacerStats, SharedHeapError> {
        self.runtime
            .observe_heap_status()
            .map(|status| status.pacer)
    }

    /// Return the rolling pause-time histogram (P50/P95/P99 over a
    /// bounded window) from the latest shared snapshot.
    ///
    /// Reads from the same lock-free shared snapshot as
    /// [`SharedHeap::stats`], so this never blocks on the heap
    /// mutex.
    pub fn pause_histogram(&self) -> Result<PauseHistogram, SharedHeapError> {
        self.runtime
            .observe_heap_status()
            .map(|status| status.pauses)
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> Result<usize, SharedHeapError> {
        self.runtime.pending_finalizer_count()
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> Result<RuntimeWorkStatus, SharedHeapError> {
        self.runtime.runtime_work_status()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&self) -> Result<u64, SharedHeapError> {
        self.runtime.drain_pending_finalizers()
    }

    /// Run and drain queued finalizers without blocking on heap contention.
    pub fn try_drain_pending_finalizers(&self) -> Result<u64, SharedHeapError> {
        self.runtime.try_drain_pending_finalizers()
    }

    /// Return one consistent shared snapshot of heap and background-collector state.
    pub fn status(&self) -> Result<SharedHeapStatus, SharedHeapError> {
        self.runtime.observe_heap_status()
    }

    /// Recommend the next collection plan from current heap pressure.
    pub fn recommended_plan(&self) -> Result<crate::plan::CollectionPlan, SharedHeapError> {
        self.collector_runtime()
            .recommended_plan()
            .map_err(|error| match error {
                SharedBackgroundError::LockPoisoned => SharedHeapError::LockPoisoned,
                SharedBackgroundError::WouldBlock => SharedHeapError::WouldBlock,
                SharedBackgroundError::Collection(_) => SharedHeapError::LockPoisoned,
            })
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.collector_runtime()
            .recommended_background_plan()
            .map_err(|error| match error {
                SharedBackgroundError::LockPoisoned => SharedHeapError::LockPoisoned,
                SharedBackgroundError::WouldBlock => SharedHeapError::WouldBlock,
                SharedBackgroundError::Collection(_) => SharedHeapError::LockPoisoned,
            })
    }

    /// Return background-collector-visible shared heap state from the latest shared snapshot.
    pub fn background_status(&self) -> Result<SharedBackgroundStatus, SharedHeapError> {
        self.runtime.observe_background_status()
    }

    /// Return one consistent observation of background epoch and background-visible shared heap
    /// state.
    pub fn background_observation(&self) -> Result<SharedBackgroundObservation, SharedHeapError> {
        let (epoch, status) = self.runtime.observe_background_status_with_epoch()?;
        Ok(SharedBackgroundObservation { epoch, status })
    }

    /// Return the last completed plan, if any.
    pub fn last_completed_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.collector_runtime()
            .last_completed_plan()
            .map_err(|error| match error {
                SharedBackgroundError::LockPoisoned => SharedHeapError::LockPoisoned,
                SharedBackgroundError::WouldBlock => SharedHeapError::WouldBlock,
                SharedBackgroundError::Collection(_) => SharedHeapError::LockPoisoned,
            })
    }

    /// Return the active major-mark plan, if any.
    pub fn active_major_mark_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.collector_runtime()
            .active_major_mark_plan()
            .map_err(|error| match error {
                SharedBackgroundError::LockPoisoned => SharedHeapError::LockPoisoned,
                SharedBackgroundError::WouldBlock => SharedHeapError::WouldBlock,
                SharedBackgroundError::Collection(_) => SharedHeapError::LockPoisoned,
            })
    }

    /// Return progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Result<Option<MajorMarkProgress>, SharedHeapError> {
        self.collector_runtime()
            .major_mark_progress()
            .map_err(|error| match error {
                SharedBackgroundError::LockPoisoned => SharedHeapError::LockPoisoned,
                SharedBackgroundError::WouldBlock => SharedHeapError::WouldBlock,
                SharedBackgroundError::Collection(_) => SharedHeapError::LockPoisoned,
            })
    }

    /// Spawn a worker-owned background collector thread for this heap.
    pub fn spawn_background_worker(&self, config: BackgroundWorkerConfig) -> BackgroundWorker {
        self.collector_runtime().spawn_background_worker(config)
    }

    /// Create a shared background service loop for this heap.
    pub fn background_service(&self, config: BackgroundCollectorConfig) -> SharedBackgroundService {
        self.collector_runtime().background_service(config)
    }

    /// Wake waiters blocked on `wait_for_change`.
    pub fn notify_waiters(&self) {
        self.runtime.notify_heap();
    }

    /// Wake waiters blocked on background-state changes.
    pub fn notify_background_waiters(&self) {
        self.collector.notify();
    }
}

/// Background worker configuration for an autonomous collector thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundWorkerConfig {
    /// Background collector coordinator configuration used by the worker.
    pub collector: BackgroundCollectorConfig,
    /// Sleep duration after an idle worker round.
    pub idle_sleep: Duration,
    /// Sleep duration after one ready-to-finish or finished round.
    pub busy_sleep: Duration,
}

impl Default for BackgroundWorkerConfig {
    fn default() -> Self {
        Self {
            collector: BackgroundCollectorConfig::default(),
            idle_sleep: Duration::from_millis(1),
            busy_sleep: Duration::ZERO,
        }
    }
}

/// Runtime statistics for one autonomous background worker.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BackgroundWorkerStats {
    /// Number of worker loop iterations executed.
    pub loops: u64,
    /// Number of worker iterations that observed an idle collector.
    pub idle_loops: u64,
    /// Number of worker iterations that entered signal-backed waiting.
    pub wait_loops: u64,
    /// Number of idle worker iterations satisfied entirely from shared snapshot state.
    pub snapshot_idle_loops: u64,
    /// Number of worker waits woken early by one shared-heap signal.
    pub signal_wakeups: u64,
    /// Number of signal-backed wakes that observed one real background-scheduler state change.
    pub background_change_wakeups: u64,
    /// Number of signal-backed wakes ignored because background-scheduler state stayed the same.
    pub ignored_signal_wakeups: u64,
    /// Number of worker iterations that skipped due to heap lock contention.
    pub contention_loops: u64,
    /// Background collector coordinator statistics accumulated by the worker.
    pub collector: BackgroundCollectorStats,
}

/// Public snapshot of one background worker and its backing shared heap state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackgroundWorkerStatus {
    /// Current autonomous worker statistics.
    pub worker: BackgroundWorkerStats,
    /// Current shared heap snapshot backing the worker.
    pub heap: SharedHeapStatus,
}

/// Background worker failure modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BackgroundWorkerError {
    /// The worker encountered one heap/collector allocation error.
    Collection(AllocError),
    /// Shared heap or worker stats were poisoned by another panic.
    LockPoisoned,
    /// The worker thread panicked before returning.
    WorkerPanicked,
}

/// Background collection coordinator for incremental major-mark sessions.
#[derive(Debug, Default)]
pub struct BackgroundCollector {
    config: BackgroundCollectorConfig,
    stats: BackgroundCollectorStats,
}

/// Collector-owned background service loop bound to one heap.
#[derive(Debug)]
pub struct BackgroundService<'heap> {
    collector: BackgroundCollector,
    runtime: CollectorRuntime<'heap>,
}

/// Shared background service loop backed by `SharedHeap`.
#[derive(Debug)]
pub struct SharedBackgroundService {
    collector: BackgroundCollector,
    runtime: SharedCollectorRuntime,
}

/// Join handle and control surface for one autonomous background collector thread.
#[derive(Debug)]
pub struct BackgroundWorker {
    stop: Arc<AtomicBool>,
    stats: Arc<BackgroundWorkerCounters>,
    runtime: SharedCollectorRuntime,
    handle: Option<JoinHandle<Result<(), BackgroundWorkerError>>>,
}

#[derive(Debug, Default)]
struct BackgroundWorkerCounters {
    loops: AtomicU64,
    idle_loops: AtomicU64,
    wait_loops: AtomicU64,
    snapshot_idle_loops: AtomicU64,
    signal_wakeups: AtomicU64,
    background_change_wakeups: AtomicU64,
    ignored_signal_wakeups: AtomicU64,
    contention_loops: AtomicU64,
    collector_ticks: AtomicU64,
    collector_rounds: AtomicU64,
    collector_sessions_started: AtomicU64,
    collector_sessions_finished: AtomicU64,
}

impl BackgroundWorkerCounters {
    fn snapshot(&self) -> BackgroundWorkerStats {
        BackgroundWorkerStats {
            loops: self.loops.load(Ordering::Relaxed),
            idle_loops: self.idle_loops.load(Ordering::Relaxed),
            wait_loops: self.wait_loops.load(Ordering::Relaxed),
            snapshot_idle_loops: self.snapshot_idle_loops.load(Ordering::Relaxed),
            signal_wakeups: self.signal_wakeups.load(Ordering::Relaxed),
            background_change_wakeups: self.background_change_wakeups.load(Ordering::Relaxed),
            ignored_signal_wakeups: self.ignored_signal_wakeups.load(Ordering::Relaxed),
            contention_loops: self.contention_loops.load(Ordering::Relaxed),
            collector: BackgroundCollectorStats {
                ticks: self.collector_ticks.load(Ordering::Relaxed),
                rounds: self.collector_rounds.load(Ordering::Relaxed),
                sessions_started: self.collector_sessions_started.load(Ordering::Relaxed),
                sessions_finished: self.collector_sessions_finished.load(Ordering::Relaxed),
            },
        }
    }

    fn add_loops(&self, delta: u64) {
        self.loops.fetch_add(delta, Ordering::Relaxed);
    }

    fn add_idle_loops(&self, delta: u64) {
        self.idle_loops.fetch_add(delta, Ordering::Relaxed);
    }

    fn add_wait_loops(&self, delta: u64) {
        self.wait_loops.fetch_add(delta, Ordering::Relaxed);
    }

    fn add_snapshot_idle_loops(&self, delta: u64) {
        self.snapshot_idle_loops.fetch_add(delta, Ordering::Relaxed);
    }

    fn add_signal_wakeups(&self, delta: u64) {
        self.signal_wakeups.fetch_add(delta, Ordering::Relaxed);
    }

    fn add_background_change_wakeups(&self, delta: u64) {
        self.background_change_wakeups
            .fetch_add(delta, Ordering::Relaxed);
    }

    fn add_ignored_signal_wakeups(&self, delta: u64) {
        self.ignored_signal_wakeups
            .fetch_add(delta, Ordering::Relaxed);
    }

    fn add_contention_loops(&self, delta: u64) {
        self.contention_loops.fetch_add(delta, Ordering::Relaxed);
    }

    fn store_collector(&self, collector: BackgroundCollectorStats) {
        self.collector_ticks
            .store(collector.ticks, Ordering::Relaxed);
        self.collector_rounds
            .store(collector.rounds, Ordering::Relaxed);
        self.collector_sessions_started
            .store(collector.sessions_started, Ordering::Relaxed);
        self.collector_sessions_finished
            .store(collector.sessions_finished, Ordering::Relaxed);
    }
}

impl BackgroundCollector {
    /// Create a new background collector coordinator.
    pub fn new(config: BackgroundCollectorConfig) -> Self {
        Self {
            config,
            stats: BackgroundCollectorStats::default(),
        }
    }

    /// Return the coordinator configuration.
    pub fn config(&self) -> BackgroundCollectorConfig {
        self.config
    }

    /// Return current coordinator statistics.
    pub fn stats(&self) -> BackgroundCollectorStats {
        self.stats
    }

    fn idle_tick(&mut self) -> BackgroundCollectionStatus {
        self.stats.ticks = self.stats.ticks.saturating_add(1);
        BackgroundCollectionStatus::Idle
    }

    fn ready_to_finish_tick(&mut self, progress: MajorMarkProgress) -> BackgroundCollectionStatus {
        self.stats.ticks = self.stats.ticks.saturating_add(1);
        BackgroundCollectionStatus::ReadyToFinish(progress)
    }

    fn begin_tick(&mut self) {
        self.stats.ticks = self.stats.ticks.saturating_add(1);
    }

    fn ensure_active_session<R: BackgroundCollectionRuntime>(
        &mut self,
        runtime: &mut R,
    ) -> Result<bool, AllocError> {
        if runtime.active_major_mark_plan().is_none() && self.config.auto_start_concurrent {
            if let Some(plan) = runtime.recommended_background_plan()
                && matches!(plan.kind, CollectionKind::Major | CollectionKind::Full)
            {
                runtime.begin_major_mark(plan)?;
                self.stats.sessions_started = self.stats.sessions_started.saturating_add(1);
            }
        }

        Ok(runtime.active_major_mark_plan().is_some())
    }

    fn tick_round<R: BackgroundCollectionRuntime>(
        &mut self,
        runtime: &mut R,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        if !self.ensure_active_session(runtime)? {
            return Ok(BackgroundCollectionStatus::Idle);
        }

        self.stats.rounds = self.stats.rounds.saturating_add(1);
        let Some(progress) = runtime.poll_background_mark_round()? else {
            return Ok(BackgroundCollectionStatus::Idle);
        };

        if progress.completed {
            if self.config.auto_finish_when_ready {
                if runtime.prepare_active_reclaim_if_needed()? {
                    return Ok(BackgroundCollectionStatus::ReadyToFinish(progress));
                }
                if let Some(cycle) = runtime.commit_active_reclaim_if_ready()? {
                    self.stats.sessions_finished = self.stats.sessions_finished.saturating_add(1);
                    return Ok(BackgroundCollectionStatus::Finished(cycle));
                }
            }
            return Ok(BackgroundCollectionStatus::ReadyToFinish(progress));
        }

        Ok(BackgroundCollectionStatus::Progress(progress))
    }

    fn aggregate_progress(
        total_drained_objects: &mut usize,
        progress: MajorMarkProgress,
    ) -> MajorMarkProgress {
        *total_drained_objects = total_drained_objects.saturating_add(progress.drained_objects);
        crate::plan::MajorMarkProgress {
            completed: progress.completed,
            drained_objects: *total_drained_objects,
            elapsed_nanos: progress.elapsed_nanos,
            mark_steps: progress.mark_steps,
            mark_rounds: progress.mark_rounds,
            remaining_work: progress.remaining_work,
        }
    }

    fn tick_with_rounds<E>(
        &mut self,
        mut tick_round: impl FnMut(&mut Self) -> Result<BackgroundCollectionStatus, E>,
    ) -> Result<BackgroundCollectionStatus, E> {
        self.begin_tick();

        let rounds = self.config.max_rounds_per_tick.max(1);
        let mut total_drained_objects = 0usize;
        let mut last_progress = None;
        for _ in 0..rounds {
            match tick_round(self)? {
                BackgroundCollectionStatus::Idle => break,
                BackgroundCollectionStatus::Finished(cycle) => {
                    return Ok(BackgroundCollectionStatus::Finished(cycle));
                }
                BackgroundCollectionStatus::Progress(progress) => {
                    last_progress = Some(Self::aggregate_progress(
                        &mut total_drained_objects,
                        progress,
                    ));
                }
                BackgroundCollectionStatus::ReadyToFinish(progress) => {
                    return Ok(BackgroundCollectionStatus::ReadyToFinish(
                        Self::aggregate_progress(&mut total_drained_objects, progress),
                    ));
                }
            }
        }

        Ok(match last_progress {
            Some(progress) => BackgroundCollectionStatus::Progress(progress),
            None => BackgroundCollectionStatus::Idle,
        })
    }

    pub(crate) fn try_tick_with_rounds(
        &mut self,
        mut tick_round: impl FnMut(
            &mut Self,
        ) -> Result<BackgroundCollectionStatus, SharedBackgroundError>,
    ) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        self.begin_tick();

        let rounds = self.config.max_rounds_per_tick.max(1);
        let mut total_drained_objects = 0usize;
        let mut last_progress = None;
        for _ in 0..rounds {
            match tick_round(self) {
                Ok(BackgroundCollectionStatus::Idle) => break,
                Ok(BackgroundCollectionStatus::Finished(cycle)) => {
                    return Ok(BackgroundCollectionStatus::Finished(cycle));
                }
                Ok(BackgroundCollectionStatus::Progress(progress)) => {
                    last_progress = Some(Self::aggregate_progress(
                        &mut total_drained_objects,
                        progress,
                    ));
                }
                Ok(BackgroundCollectionStatus::ReadyToFinish(progress)) => {
                    return Ok(BackgroundCollectionStatus::ReadyToFinish(
                        Self::aggregate_progress(&mut total_drained_objects, progress),
                    ));
                }
                Err(SharedBackgroundError::WouldBlock) if last_progress.is_some() => break,
                Err(error) => return Err(error),
            }
        }

        Ok(match last_progress {
            Some(progress) => BackgroundCollectionStatus::Progress(progress),
            None => BackgroundCollectionStatus::Idle,
        })
    }

    fn snapshot_tick(
        &mut self,
        snapshot: &CollectorSharedSnapshot,
    ) -> Option<BackgroundCollectionStatus> {
        if snapshot.active_major_mark_plan.is_none()
            && snapshot.recommended_background_plan.is_none()
        {
            return Some(self.idle_tick());
        }
        if !self.config.auto_finish_when_ready
            && snapshot.active_major_mark_plan.is_some()
            && let Some(progress) = snapshot.major_mark_progress
            && progress.completed
        {
            return Some(self.ready_to_finish_tick(progress));
        }
        None
    }

    fn blocked_status_from_snapshot(
        &self,
        snapshot: &CollectorSharedSnapshot,
    ) -> Option<BackgroundCollectionStatus> {
        let progress = snapshot.major_mark_progress?;
        if snapshot.active_major_mark_plan.is_none() {
            return None;
        }
        if progress.completed {
            Some(BackgroundCollectionStatus::ReadyToFinish(progress))
        } else {
            Some(BackgroundCollectionStatus::Progress(progress))
        }
    }

    fn ensure_active_shared_session(
        &mut self,
        runtime: &SharedCollectorRuntime,
        nonblocking: bool,
    ) -> Result<bool, SharedBackgroundError> {
        if runtime.active_major_mark_plan()?.is_none() && self.config.auto_start_concurrent {
            if let Some(plan) = runtime.recommended_background_plan()?
                && matches!(plan.kind, CollectionKind::Major | CollectionKind::Full)
            {
                if nonblocking {
                    runtime.try_begin_major_mark(plan)?;
                } else {
                    runtime.begin_major_mark(plan)?;
                }
                self.stats.sessions_started = self.stats.sessions_started.saturating_add(1);
            }
        }

        runtime.active_major_mark_plan().map(|plan| plan.is_some())
    }

    fn tick_shared_round(
        &mut self,
        runtime: &SharedCollectorRuntime,
        nonblocking: bool,
    ) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        if !self.ensure_active_shared_session(runtime, nonblocking)? {
            return Ok(BackgroundCollectionStatus::Idle);
        }

        self.stats.rounds = self.stats.rounds.saturating_add(1);
        let status = if nonblocking {
            runtime.try_service_background_collection_round()?
        } else {
            runtime.service_background_collection_round()?
        };
        if matches!(status, BackgroundCollectionStatus::Finished(_)) {
            self.stats.sessions_finished = self.stats.sessions_finished.saturating_add(1);
        }
        Ok(status)
    }

    fn tick_shared_after_snapshot(
        &mut self,
        runtime: &SharedCollectorRuntime,
    ) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        self.tick_with_rounds(|collector| collector.tick_shared_round(runtime, false))
    }

    fn try_tick_shared_after_snapshot(
        &mut self,
        runtime: &SharedCollectorRuntime,
    ) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        self.try_tick_with_rounds(|collector| collector.tick_shared_round(runtime, true))
    }

    fn tick_shared(
        &mut self,
        runtime: &SharedCollectorRuntime,
    ) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        let snapshot = runtime.collector_snapshot()?;
        if let Some(status) = self.snapshot_tick(&snapshot) {
            return Ok(status);
        }
        match self.try_tick_shared_after_snapshot(runtime) {
            Err(SharedBackgroundError::WouldBlock) => {
                if let Some(status) = self.blocked_status_from_snapshot(&snapshot) {
                    Ok(status)
                } else {
                    self.tick_shared_after_snapshot(runtime)
                }
            }
            other => other,
        }
    }

    fn try_tick_shared(
        &mut self,
        runtime: &SharedCollectorRuntime,
    ) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        let snapshot = runtime.collector_snapshot()?;
        if let Some(status) = self.snapshot_tick(&snapshot) {
            return Ok(status);
        }
        match self.try_tick_shared_after_snapshot(runtime) {
            Err(SharedBackgroundError::WouldBlock) => self
                .blocked_status_from_snapshot(&snapshot)
                .ok_or(SharedBackgroundError::WouldBlock),
            other => other,
        }
    }

    /// Run one background-collection coordinator tick.
    pub fn tick<R: BackgroundCollectionRuntime>(
        &mut self,
        runtime: &mut R,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        self.tick_with_rounds(|collector| collector.tick_round(runtime))
    }

    /// Service background collection until no active session remains or one collection finishes.
    pub fn run_until_idle<R: BackgroundCollectionRuntime>(
        &mut self,
        runtime: &mut R,
    ) -> Result<Option<CollectionStats>, AllocError> {
        loop {
            match self.tick(runtime)? {
                BackgroundCollectionStatus::Idle => return Ok(None),
                BackgroundCollectionStatus::Progress(_) => {}
                BackgroundCollectionStatus::ReadyToFinish(progress) => {
                    if progress.completed {
                        if let Some(cycle) = runtime.finish_active_major_collection_if_ready()? {
                            self.stats.sessions_finished =
                                self.stats.sessions_finished.saturating_add(1);
                            return Ok(Some(cycle));
                        }
                    }
                }
                BackgroundCollectionStatus::Finished(cycle) => return Ok(Some(cycle)),
            }
        }
    }
}

impl<'heap> BackgroundService<'heap> {
    /// Create a new background service loop bound to `heap`.
    pub(crate) fn from_runtime(
        runtime: CollectorRuntime<'heap>,
        config: BackgroundCollectorConfig,
    ) -> Self {
        Self {
            collector: BackgroundCollector::new(config),
            runtime,
        }
    }

    /// Return the service configuration.
    pub fn config(&self) -> BackgroundCollectorConfig {
        self.collector.config()
    }

    /// Return current service statistics.
    pub fn stats(&self) -> BackgroundCollectorStats {
        self.collector.stats()
    }

    /// Return a shared view of the underlying heap.
    pub fn heap(&self) -> &Heap {
        self.runtime.heap()
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(&self) -> Option<crate::plan::CollectionPlan> {
        self.runtime.active_major_mark_plan()
    }

    /// Return progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Option<crate::plan::MajorMarkProgress> {
        self.runtime.major_mark_progress()
    }

    /// Run one background-collection coordinator tick.
    pub fn tick(&mut self) -> Result<BackgroundCollectionStatus, AllocError> {
        self.collector.tick(&mut self.runtime)
    }

    /// Service background collection until no active session remains or one collection finishes.
    pub fn run_until_idle(&mut self) -> Result<Option<CollectionStats>, AllocError> {
        self.collector.run_until_idle(&mut self.runtime)
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained.
    pub fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        self.runtime.prepare_active_reclaim_if_needed()
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.runtime.commit_active_reclaim_if_ready()
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> usize {
        self.runtime.pending_finalizer_count()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&mut self) -> u64 {
        self.runtime.drain_pending_finalizers()
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> RuntimeWorkStatus {
        self.runtime.runtime_work_status()
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.runtime.finish_active_major_collection_if_ready()
    }
}

impl SharedBackgroundService {
    /// Create a new shared background service loop bound to one `SharedHeap`.
    pub fn new(heap: SharedHeap, config: BackgroundCollectorConfig) -> Self {
        Self::from_runtime(heap.collector_runtime(), config)
    }

    pub(crate) fn from_runtime(
        runtime: SharedCollectorRuntime,
        config: BackgroundCollectorConfig,
    ) -> Self {
        Self {
            collector: BackgroundCollector::new(config),
            runtime,
        }
    }

    /// Return the service configuration.
    pub fn config(&self) -> BackgroundCollectorConfig {
        self.collector.config()
    }

    /// Return current service statistics.
    pub fn stats(&self) -> BackgroundCollectorStats {
        self.collector.stats()
    }

    /// Return one consistent snapshot of collector and heap state for this shared service.
    pub fn status(&self) -> Result<SharedBackgroundServiceStatus, SharedBackgroundError> {
        Ok(SharedBackgroundServiceStatus {
            collector: self.collector.stats(),
            heap: self.runtime.status()?,
        })
    }

    /// Return the shared heap backing this service.
    pub fn heap(&self) -> &SharedHeap {
        self.runtime.heap()
    }

    /// Wait for one shared-heap change visible to this service.
    pub fn wait_for_change(
        &self,
        observed_epoch: u64,
        timeout: Duration,
    ) -> Result<(u64, bool), SharedBackgroundError> {
        self.runtime.wait_for_change(observed_epoch, timeout)
    }

    /// Return the current background-state change epoch for this service.
    pub fn background_epoch(&self) -> Result<u64, SharedBackgroundError> {
        self.runtime.background_epoch()
    }

    /// Return background-collector-visible shared heap state for this service.
    pub fn background_status(&self) -> Result<SharedBackgroundStatus, SharedBackgroundError> {
        self.runtime.background_status()
    }

    /// Return one consistent observation of background epoch and background-visible shared heap
    /// state for this service.
    pub fn background_observation(
        &self,
    ) -> Result<SharedBackgroundObservation, SharedBackgroundError> {
        self.runtime.background_observation()
    }

    /// Wait for one background-collector-visible shared heap state change for this service.
    pub fn wait_for_background_change(
        &self,
        observed_epoch: u64,
        observed_status: &SharedBackgroundStatus,
        timeout: Duration,
    ) -> Result<SharedBackgroundWaitResult, SharedBackgroundError> {
        self.runtime
            .wait_for_background_change(observed_epoch, observed_status, timeout)
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.runtime
            .active_major_mark_plan()
            .map_err(|error| match error {
                SharedBackgroundError::LockPoisoned => SharedHeapError::LockPoisoned,
                SharedBackgroundError::WouldBlock => SharedHeapError::WouldBlock,
                SharedBackgroundError::Collection(AllocError::CollectionInProgress) => {
                    SharedHeapError::WouldBlock
                }
                SharedBackgroundError::Collection(_) => SharedHeapError::LockPoisoned,
            })
    }

    /// Return progress for the active major-mark session, if any.
    pub fn major_mark_progress(
        &self,
    ) -> Result<Option<crate::plan::MajorMarkProgress>, SharedHeapError> {
        self.runtime
            .major_mark_progress()
            .map_err(|error| match error {
                SharedBackgroundError::LockPoisoned => SharedHeapError::LockPoisoned,
                SharedBackgroundError::WouldBlock => SharedHeapError::WouldBlock,
                SharedBackgroundError::Collection(AllocError::CollectionInProgress) => {
                    SharedHeapError::WouldBlock
                }
                SharedBackgroundError::Collection(_) => SharedHeapError::LockPoisoned,
            })
    }

    /// Run one background-collection coordinator tick.
    pub fn tick(&mut self) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        self.collector.tick_shared(&self.runtime)
    }

    /// Run one background-collection coordinator tick without blocking on heap lock contention.
    pub fn try_tick(&mut self) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        self.collector.try_tick_shared(&self.runtime)
    }

    /// Service background collection until no active session remains or one collection finishes.
    ///
    /// Unlike the collector-runtime variant, this shared version reacquires the heap lock once
    /// per service round instead of holding it for the whole loop.
    pub fn run_until_idle(&mut self) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        loop {
            match self.tick()? {
                BackgroundCollectionStatus::Idle => return Ok(None),
                BackgroundCollectionStatus::Progress(_) => thread::yield_now(),
                BackgroundCollectionStatus::ReadyToFinish(progress) => {
                    if progress.completed
                        && let Some(cycle) = self.finish_active_major_collection_if_ready()?
                    {
                        return Ok(Some(cycle));
                    }
                    thread::yield_now();
                }
                BackgroundCollectionStatus::Finished(cycle) => return Ok(Some(cycle)),
            }
        }
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained.
    pub fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, SharedBackgroundError> {
        self.runtime.prepare_active_reclaim_if_needed()
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained, without
    /// blocking on heap lock contention.
    pub fn try_prepare_active_reclaim_if_needed(&mut self) -> Result<bool, SharedBackgroundError> {
        self.runtime.try_prepare_active_reclaim_if_needed()
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        match self.runtime.try_commit_active_reclaim_if_ready() {
            Ok(result) => Ok(result),
            Err(SharedBackgroundError::WouldBlock) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> Result<usize, SharedBackgroundError> {
        self.runtime.pending_finalizer_count()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&mut self) -> Result<u64, SharedBackgroundError> {
        self.runtime.drain_pending_finalizers()
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> Result<RuntimeWorkStatus, SharedBackgroundError> {
        self.runtime.runtime_work_status()
    }

    /// Run and drain queued finalizers without blocking on heap contention.
    pub fn try_drain_pending_finalizers(&mut self) -> Result<u64, SharedBackgroundError> {
        self.runtime.try_drain_pending_finalizers()
    }

    /// Commit the active major collection once reclaim has already been prepared, without
    /// blocking on heap lock contention.
    pub fn try_commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        self.runtime.try_commit_active_reclaim_if_ready()
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        match self.runtime.try_finish_active_major_collection_if_ready() {
            Ok(result) => Ok(result),
            Err(SharedBackgroundError::WouldBlock) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Finish the active major collection if its mark work is fully drained, without blocking on
    /// heap lock contention.
    pub fn try_finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        self.runtime.try_finish_active_major_collection_if_ready()
    }

    /// Service background collection until no active session remains or one collection finishes,
    /// without blocking on heap lock contention.
    pub fn try_run_until_idle(&mut self) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        loop {
            match self.try_tick()? {
                BackgroundCollectionStatus::Idle => return Ok(None),
                BackgroundCollectionStatus::Progress(_) => thread::yield_now(),
                BackgroundCollectionStatus::ReadyToFinish(progress) => {
                    if progress.completed
                        && let Some(cycle) = self.try_finish_active_major_collection_if_ready()?
                    {
                        return Ok(Some(cycle));
                    }
                    thread::yield_now();
                }
                BackgroundCollectionStatus::Finished(cycle) => return Ok(Some(cycle)),
            }
        }
    }
}

impl BackgroundWorker {
    pub(crate) fn spawn(runtime: SharedCollectorRuntime, config: BackgroundWorkerConfig) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(BackgroundWorkerCounters::default());
        let worker_stop = Arc::clone(&stop);
        let worker_stats = Arc::clone(&stats);
        let worker_runtime = runtime.clone();
        let handle =
            thread::spawn(move || worker_loop(worker_runtime, config, worker_stop, worker_stats));
        Self {
            stop,
            stats,
            runtime,
            handle: Some(handle),
        }
    }

    /// Request that the worker stop after its current loop iteration.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.runtime.notify_waiters();
        self.runtime.notify_background_waiters();
    }

    /// Return whether the worker thread has already finished.
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
    }

    /// Return a snapshot of current worker statistics.
    pub fn stats(&self) -> Result<BackgroundWorkerStats, BackgroundWorkerError> {
        Ok(self.stats.snapshot())
    }

    /// Return a combined snapshot of worker and shared heap state.
    pub fn status(&self) -> Result<BackgroundWorkerStatus, BackgroundWorkerError> {
        Ok(BackgroundWorkerStatus {
            worker: self.stats()?,
            heap: self
                .runtime
                .status()
                .map_err(|_| BackgroundWorkerError::LockPoisoned)?,
        })
    }

    /// Stop the worker and join its thread, returning final worker statistics.
    pub fn join(mut self) -> Result<BackgroundWorkerStats, BackgroundWorkerError> {
        self.request_stop();
        let Some(handle) = self.handle.take() else {
            return self.stats();
        };
        match handle.join() {
            Ok(Ok(())) => self.stats(),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(BackgroundWorkerError::WorkerPanicked),
        }
    }
}

fn background_wait_duration(
    status: &BackgroundCollectionStatus,
    config: &BackgroundWorkerConfig,
) -> Duration {
    match status {
        BackgroundCollectionStatus::Idle => config.idle_sleep,
        BackgroundCollectionStatus::ReadyToFinish(_) | BackgroundCollectionStatus::Finished(_) => {
            config.busy_sleep
        }
        BackgroundCollectionStatus::Progress(_) => Duration::ZERO,
    }
}

fn worker_loop(
    runtime: SharedCollectorRuntime,
    config: BackgroundWorkerConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<BackgroundWorkerCounters>,
) -> Result<(), BackgroundWorkerError> {
    let mut collector = BackgroundCollector::new(config.collector);

    let wait_for_signal = |stats: &Arc<BackgroundWorkerCounters>,
                           runtime: &SharedCollectorRuntime,
                           stop: &Arc<AtomicBool>,
                           observed_signal_epoch: &mut u64,
                           observed_background: &mut CollectorSharedSnapshot,
                           timeout: Duration|
     -> Result<(), BackgroundWorkerError> {
        if timeout.is_zero() {
            return Ok(());
        }

        stats.add_wait_loops(1);

        let (signal_changed, collector_changed) = runtime
            .wait_for_collector_change(
                observed_signal_epoch,
                observed_background,
                timeout,
                Some(stop),
            )
            .map_err(|_| BackgroundWorkerError::LockPoisoned)?;

        if signal_changed {
            stats.add_signal_wakeups(1);
        }
        if collector_changed {
            stats.add_background_change_wakeups(1);
        } else if signal_changed && !stop.load(Ordering::Acquire) {
            stats.add_ignored_signal_wakeups(1);
        }

        Ok(())
    };

    while !stop.load(Ordering::Acquire) {
        let snapshot = runtime
            .collector_snapshot()
            .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
        if let Some(status) = collector.snapshot_tick(&snapshot) {
            stats.add_loops(1);
            if matches!(status, BackgroundCollectionStatus::Idle) {
                stats.add_idle_loops(1);
                stats.add_snapshot_idle_loops(1);
            }
            stats.store_collector(collector.stats());
            let (mut observed_signal_epoch, mut observed_background) = runtime
                .collector_observation()
                .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
            let wait_for = match status {
                BackgroundCollectionStatus::Idle => config.idle_sleep,
                BackgroundCollectionStatus::ReadyToFinish(_)
                | BackgroundCollectionStatus::Finished(_) => config.busy_sleep,
                BackgroundCollectionStatus::Progress(_) => Duration::ZERO,
            };
            wait_for_signal(
                &stats,
                &runtime,
                &stop,
                &mut observed_signal_epoch,
                &mut observed_background,
                wait_for,
            )?;
            continue;
        }

        let status = match collector.try_tick_shared_after_snapshot(&runtime) {
            Ok(status) => status,
            Err(SharedBackgroundError::Collection(error)) => {
                return Err(BackgroundWorkerError::Collection(error));
            }
            Err(SharedBackgroundError::LockPoisoned) => {
                return Err(BackgroundWorkerError::LockPoisoned);
            }
            Err(SharedBackgroundError::WouldBlock) => {
                let blocked_status = collector.blocked_status_from_snapshot(&snapshot);
                stats.add_loops(1);
                stats.add_contention_loops(1);
                if blocked_status.is_none() {
                    stats.add_idle_loops(1);
                }
                stats.store_collector(collector.stats());
                let (mut observed_signal_epoch, mut observed_background) = runtime
                    .collector_observation()
                    .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
                let wait_for = blocked_status
                    .as_ref()
                    .map(|status| background_wait_duration(status, &config))
                    .unwrap_or(config.idle_sleep);
                wait_for_signal(
                    &stats,
                    &runtime,
                    &stop,
                    &mut observed_signal_epoch,
                    &mut observed_background,
                    wait_for,
                )?;
                continue;
            }
        };

        let (mut observed_signal_epoch, mut observed_background) = runtime
            .collector_observation()
            .map_err(|_| BackgroundWorkerError::LockPoisoned)?;

        stats.add_loops(1);
        if matches!(status, BackgroundCollectionStatus::Idle) {
            stats.add_idle_loops(1);
        }
        stats.store_collector(collector.stats());

        let sleep_for = background_wait_duration(&status, &config);
        wait_for_signal(
            &stats,
            &runtime,
            &stop,
            &mut observed_signal_epoch,
            &mut observed_background,
            sleep_for,
        )?;
    }

    Ok(())
}
