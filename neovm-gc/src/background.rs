use crate::heap::{AllocError, Heap};
use crate::mutator::Mutator;
use crate::plan::{BackgroundCollectionStatus, CollectionKind, CollectionPlan, MajorMarkProgress};
use crate::runtime::CollectorRuntime;
use crate::stats::{CollectionStats, HeapStats};
use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LockResult, Mutex, MutexGuard, RwLock, TryLockError, TryLockResult};
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
    inner: Arc<Mutex<Heap>>,
    snapshot: Arc<RwLock<SharedHeapSnapshot>>,
    signal: Arc<SharedHeapSignal>,
}

/// Public snapshot of shared heap state that can be read without taking the main heap mutex.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedHeapStatus {
    /// Current heap statistics.
    pub stats: HeapStats,
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

/// Shared-heap failure modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedHeapError {
    /// Shared heap state was poisoned by another panic.
    LockPoisoned,
    /// Shared heap state is currently locked by another thread.
    WouldBlock,
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

#[derive(Clone, Debug)]
struct SharedHeapSnapshot {
    stats: HeapStats,
    recommended_plan: CollectionPlan,
    recommended_background_plan: Option<CollectionPlan>,
    last_completed_plan: Option<CollectionPlan>,
    active_major_mark_plan: Option<CollectionPlan>,
    major_mark_progress: Option<MajorMarkProgress>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SharedBackgroundSnapshot {
    recommended_background_plan: Option<CollectionPlan>,
    active_major_mark_plan: Option<CollectionPlan>,
    major_mark_progress: Option<MajorMarkProgress>,
}

#[derive(Debug, Default)]
struct SharedHeapSignal {
    epoch: Mutex<u64>,
    cv: Condvar,
}

impl SharedHeapSignal {
    fn current_epoch(&self) -> Result<u64, SharedHeapError> {
        self.epoch
            .lock()
            .map(|epoch| *epoch)
            .map_err(|_| SharedHeapError::LockPoisoned)
    }

    fn notify(&self) {
        if let Ok(mut epoch) = self.epoch.lock() {
            *epoch = epoch.saturating_add(1);
            self.cv.notify_all();
        }
    }

    fn wait_for_change(
        &self,
        observed_epoch: u64,
        timeout: Duration,
    ) -> Result<(u64, bool), SharedHeapError> {
        if timeout.is_zero() {
            return Ok((observed_epoch, false));
        }

        let epoch = self
            .epoch
            .lock()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let (epoch, _) = self
            .cv
            .wait_timeout_while(epoch, timeout, |epoch| *epoch == observed_epoch)
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let next_epoch = *epoch;
        Ok((next_epoch, next_epoch != observed_epoch))
    }
}

impl SharedHeapSnapshot {
    fn capture(heap: &Heap) -> Self {
        Self {
            stats: heap.stats(),
            recommended_plan: heap.recommended_plan(),
            recommended_background_plan: heap.recommended_background_plan(),
            last_completed_plan: heap.last_completed_plan(),
            active_major_mark_plan: heap.active_major_mark_plan(),
            major_mark_progress: heap.major_mark_progress(),
        }
    }

    fn public_status(&self) -> SharedHeapStatus {
        SharedHeapStatus {
            stats: self.stats,
            recommended_plan: self.recommended_plan.clone(),
            recommended_background_plan: self.recommended_background_plan.clone(),
            last_completed_plan: self.last_completed_plan.clone(),
            active_major_mark_plan: self.active_major_mark_plan.clone(),
            major_mark_progress: self.major_mark_progress,
        }
    }
}

/// Guard returned by `SharedHeap::lock()` and `SharedHeap::try_lock()`.
#[derive(Debug)]
pub struct SharedHeapGuard<'a> {
    guard: MutexGuard<'a, Heap>,
    snapshot: &'a RwLock<SharedHeapSnapshot>,
    signal: &'a SharedHeapSignal,
}

impl<'a> SharedHeapGuard<'a> {
    fn new(
        guard: MutexGuard<'a, Heap>,
        snapshot: &'a RwLock<SharedHeapSnapshot>,
        signal: &'a SharedHeapSignal,
    ) -> Self {
        Self {
            guard,
            snapshot,
            signal,
        }
    }
}

impl Deref for SharedHeapGuard<'_> {
    type Target = Heap;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl DerefMut for SharedHeapGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}

impl Drop for SharedHeapGuard<'_> {
    fn drop(&mut self) {
        if let Ok(mut snapshot) = self.snapshot.write() {
            *snapshot = SharedHeapSnapshot::capture(&self.guard);
        }
        self.signal.notify();
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
        Self {
            inner: Arc::new(Mutex::new(heap)),
            snapshot: Arc::new(RwLock::new(snapshot)),
            signal: Arc::new(SharedHeapSignal::default()),
        }
    }

    /// Lock the underlying heap.
    pub fn lock(&self) -> LockResult<SharedHeapGuard<'_>> {
        match self.inner.lock() {
            Ok(guard) => Ok(SharedHeapGuard::new(guard, &self.snapshot, &self.signal)),
            Err(error) => Err(std::sync::PoisonError::new(SharedHeapGuard::new(
                error.into_inner(),
                &self.snapshot,
                &self.signal,
            ))),
        }
    }

    /// Try to lock the underlying heap without blocking.
    pub fn try_lock(&self) -> TryLockResult<SharedHeapGuard<'_>> {
        match self.inner.try_lock() {
            Ok(guard) => Ok(SharedHeapGuard::new(guard, &self.snapshot, &self.signal)),
            Err(TryLockError::Poisoned(error)) => {
                Err(TryLockError::Poisoned(std::sync::PoisonError::new(
                    SharedHeapGuard::new(error.into_inner(), &self.snapshot, &self.signal),
                )))
            }
            Err(TryLockError::WouldBlock) => Err(TryLockError::WouldBlock),
        }
    }

    /// Execute one closure with exclusive access to the underlying heap.
    pub fn with_heap<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> Result<R, SharedHeapError> {
        let mut heap = self.lock().map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&mut heap))
    }

    /// Execute one closure with exclusive access to the underlying heap without blocking.
    pub fn try_with_heap<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> Result<R, SharedHeapError> {
        let mut heap = self.try_lock().map_err(|error| match error {
            TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        Ok(f(&mut heap))
    }

    fn read_snapshot<R>(
        &self,
        f: impl FnOnce(&SharedHeapSnapshot) -> R,
    ) -> Result<R, SharedHeapError> {
        let snapshot = self
            .snapshot
            .read()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&snapshot))
    }

    fn background_snapshot(&self) -> Result<SharedBackgroundSnapshot, SharedHeapError> {
        self.read_snapshot(|snapshot| SharedBackgroundSnapshot {
            recommended_background_plan: snapshot.recommended_background_plan.clone(),
            active_major_mark_plan: snapshot.active_major_mark_plan.clone(),
            major_mark_progress: snapshot.major_mark_progress,
        })
    }

    /// Return the current shared-heap change epoch used by signal-backed waiters.
    pub fn epoch(&self) -> Result<u64, SharedHeapError> {
        self.signal.current_epoch()
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
        self.signal.wait_for_change(observed_epoch, timeout)
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

    /// Return current heap statistics.
    pub fn stats(&self) -> Result<HeapStats, SharedHeapError> {
        self.read_snapshot(|snapshot| snapshot.stats)
    }

    /// Return one consistent shared snapshot of heap and background-collector state.
    pub fn status(&self) -> Result<SharedHeapStatus, SharedHeapError> {
        self.read_snapshot(SharedHeapSnapshot::public_status)
    }

    /// Recommend the next collection plan from current heap pressure.
    pub fn recommended_plan(&self) -> Result<crate::plan::CollectionPlan, SharedHeapError> {
        self.read_snapshot(|snapshot| snapshot.recommended_plan.clone())
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.read_snapshot(|snapshot| snapshot.recommended_background_plan.clone())
    }

    /// Return the last completed plan, if any.
    pub fn last_completed_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.read_snapshot(|snapshot| snapshot.last_completed_plan.clone())
    }

    /// Return the active major-mark plan, if any.
    pub fn active_major_mark_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.read_snapshot(|snapshot| snapshot.active_major_mark_plan.clone())
    }

    /// Return progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Result<Option<MajorMarkProgress>, SharedHeapError> {
        self.read_snapshot(|snapshot| snapshot.major_mark_progress)
    }

    /// Spawn a worker-owned background collector thread for this heap.
    pub fn spawn_background_worker(&self, config: BackgroundWorkerConfig) -> BackgroundWorker {
        BackgroundWorker::spawn(self.clone(), config)
    }

    /// Create a shared background service loop for this heap.
    pub fn background_service(&self, config: BackgroundCollectorConfig) -> SharedBackgroundService {
        SharedBackgroundService::new(self.clone(), config)
    }

    /// Wake waiters blocked on `wait_for_change`.
    pub fn notify_waiters(&self) {
        self.signal.notify();
    }
}

/// Background worker configuration for an autonomous collector thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackgroundWorkerConfig {
    /// Background collector coordinator configuration used by the worker.
    pub collector: BackgroundCollectorConfig,
    /// Sleep duration after an idle worker round.
    pub idle_sleep: Duration,
    /// Sleep duration after a progress or finish round.
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
    heap: SharedHeap,
}

/// Join handle and control surface for one autonomous background collector thread.
#[derive(Debug)]
pub struct BackgroundWorker {
    stop: Arc<AtomicBool>,
    stats: Arc<RwLock<BackgroundWorkerStats>>,
    shared: SharedHeap,
    handle: Option<JoinHandle<Result<(), BackgroundWorkerError>>>,
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

    fn snapshot_tick(
        &mut self,
        snapshot: &SharedBackgroundSnapshot,
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

    /// Run one background-collection coordinator tick.
    pub fn tick<R: BackgroundCollectionRuntime>(
        &mut self,
        runtime: &mut R,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        self.stats.ticks = self.stats.ticks.saturating_add(1);

        if runtime.active_major_mark_plan().is_none() && self.config.auto_start_concurrent {
            if let Some(plan) = runtime.recommended_background_plan()
                && matches!(plan.kind, CollectionKind::Major | CollectionKind::Full)
            {
                runtime.begin_major_mark(plan)?;
                self.stats.sessions_started = self.stats.sessions_started.saturating_add(1);
            }
        }

        if runtime.active_major_mark_plan().is_none() {
            return Ok(BackgroundCollectionStatus::Idle);
        }

        let rounds = self.config.max_rounds_per_tick.max(1);
        let mut total_drained_objects = 0usize;
        let mut last_progress = None;
        let mut ready_to_finish = false;
        for _ in 0..rounds {
            self.stats.rounds = self.stats.rounds.saturating_add(1);
            let Some(progress) = runtime.poll_background_mark_round()? else {
                break;
            };
            total_drained_objects = total_drained_objects.saturating_add(progress.drained_objects);

            if progress.completed {
                if self.config.auto_finish_when_ready
                    && let Some(cycle) = runtime.finish_active_major_collection_if_ready()?
                {
                    self.stats.sessions_finished = self.stats.sessions_finished.saturating_add(1);
                    return Ok(BackgroundCollectionStatus::Finished(cycle));
                }
                last_progress = Some(progress);
                ready_to_finish = true;
                break;
            }
            last_progress = Some(progress);
        }

        Ok(match last_progress {
            Some(progress) => {
                let status = crate::plan::MajorMarkProgress {
                    completed: progress.completed,
                    drained_objects: total_drained_objects,
                    mark_steps: progress.mark_steps,
                    mark_rounds: progress.mark_rounds,
                    remaining_work: progress.remaining_work,
                };
                if ready_to_finish {
                    BackgroundCollectionStatus::ReadyToFinish(status)
                } else {
                    BackgroundCollectionStatus::Progress(status)
                }
            }
            None => BackgroundCollectionStatus::Idle,
        })
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
    pub(crate) fn new(heap: &'heap mut Heap, config: BackgroundCollectorConfig) -> Self {
        Self {
            collector: BackgroundCollector::new(config),
            runtime: CollectorRuntime::new(heap),
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
        Self {
            collector: BackgroundCollector::new(config),
            heap,
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
            heap: self.heap.status().map_err(|error| match error {
                SharedHeapError::LockPoisoned => SharedBackgroundError::LockPoisoned,
                SharedHeapError::WouldBlock => SharedBackgroundError::WouldBlock,
            })?,
        })
    }

    /// Return the shared heap backing this service.
    pub fn heap(&self) -> &SharedHeap {
        &self.heap
    }

    /// Wait for one shared-heap change visible to this service.
    pub fn wait_for_change(
        &self,
        observed_epoch: u64,
        timeout: Duration,
    ) -> Result<(u64, bool), SharedBackgroundError> {
        self.heap
            .wait_for_change(observed_epoch, timeout)
            .map_err(|error| match error {
                SharedHeapError::LockPoisoned => SharedBackgroundError::LockPoisoned,
                SharedHeapError::WouldBlock => SharedBackgroundError::WouldBlock,
            })
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.heap.active_major_mark_plan()
    }

    /// Return progress for the active major-mark session, if any.
    pub fn major_mark_progress(
        &self,
    ) -> Result<Option<crate::plan::MajorMarkProgress>, SharedHeapError> {
        self.heap.major_mark_progress()
    }

    /// Run one background-collection coordinator tick.
    pub fn tick(&mut self) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        self.heap
            .with_runtime(|runtime| self.collector.tick(runtime))
            .map_err(|_| SharedBackgroundError::LockPoisoned)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Run one background-collection coordinator tick without blocking on heap lock contention.
    pub fn try_tick(&mut self) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        let snapshot = self
            .heap
            .background_snapshot()
            .map_err(|error| match error {
                SharedHeapError::LockPoisoned => SharedBackgroundError::LockPoisoned,
                SharedHeapError::WouldBlock => SharedBackgroundError::WouldBlock,
            })?;
        if let Some(status) = self.collector.snapshot_tick(&snapshot) {
            return Ok(status);
        }
        self.heap
            .try_with_runtime(|runtime| self.collector.tick(runtime))
            .map_err(|error| match error {
                SharedHeapError::LockPoisoned => SharedBackgroundError::LockPoisoned,
                SharedHeapError::WouldBlock => SharedBackgroundError::WouldBlock,
            })?
            .map_err(SharedBackgroundError::Collection)
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

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        self.heap
            .with_runtime(|runtime| runtime.finish_active_major_collection_if_ready())
            .map_err(|_| SharedBackgroundError::LockPoisoned)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Finish the active major collection if its mark work is fully drained, without blocking on
    /// heap lock contention.
    pub fn try_finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        let snapshot = self
            .heap
            .background_snapshot()
            .map_err(|error| match error {
                SharedHeapError::LockPoisoned => SharedBackgroundError::LockPoisoned,
                SharedHeapError::WouldBlock => SharedBackgroundError::WouldBlock,
            })?;
        if snapshot.active_major_mark_plan.is_none() {
            return Ok(None);
        }
        if snapshot
            .major_mark_progress
            .is_some_and(|progress| !progress.completed)
        {
            return Ok(None);
        }
        self.heap
            .try_with_runtime(|runtime| runtime.finish_active_major_collection_if_ready())
            .map_err(|error| match error {
                SharedHeapError::LockPoisoned => SharedBackgroundError::LockPoisoned,
                SharedHeapError::WouldBlock => SharedBackgroundError::WouldBlock,
            })?
            .map_err(SharedBackgroundError::Collection)
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
    fn spawn(shared: SharedHeap, config: BackgroundWorkerConfig) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(RwLock::new(BackgroundWorkerStats::default()));
        let worker_stop = Arc::clone(&stop);
        let worker_stats = Arc::clone(&stats);
        let worker_shared = shared.clone();
        let handle =
            thread::spawn(move || worker_loop(worker_shared, config, worker_stop, worker_stats));
        Self {
            stop,
            stats,
            shared,
            handle: Some(handle),
        }
    }

    /// Request that the worker stop after its current loop iteration.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.shared.notify_waiters();
    }

    /// Return whether the worker thread has already finished.
    pub fn is_finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
    }

    /// Return a snapshot of current worker statistics.
    pub fn stats(&self) -> Result<BackgroundWorkerStats, BackgroundWorkerError> {
        self.stats
            .read()
            .map(|stats| *stats)
            .map_err(|_| BackgroundWorkerError::LockPoisoned)
    }

    /// Return a combined snapshot of worker and shared heap state.
    pub fn status(&self) -> Result<BackgroundWorkerStatus, BackgroundWorkerError> {
        Ok(BackgroundWorkerStatus {
            worker: self.stats()?,
            heap: self
                .shared
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

fn worker_loop(
    shared: SharedHeap,
    config: BackgroundWorkerConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<RwLock<BackgroundWorkerStats>>,
) -> Result<(), BackgroundWorkerError> {
    let mut collector = BackgroundCollector::new(config.collector);
    let mut observed_signal_epoch = shared
        .epoch()
        .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
    let mut observed_background = shared
        .background_snapshot()
        .map_err(|_| BackgroundWorkerError::LockPoisoned)?;

    let wait_for_signal = |stats: &Arc<RwLock<BackgroundWorkerStats>>,
                           shared: &SharedHeap,
                           stop: &Arc<AtomicBool>,
                           observed_signal_epoch: &mut u64,
                           observed_background: &mut SharedBackgroundSnapshot,
                           timeout: Duration|
     -> Result<(), BackgroundWorkerError> {
        if timeout.is_zero() {
            return Ok(());
        }

        {
            let mut snapshot = stats
                .write()
                .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
            snapshot.wait_loops = snapshot.wait_loops.saturating_add(1);
        }

        let started_at = std::time::Instant::now();
        let mut remaining = timeout;
        loop {
            let (next_epoch, changed) = shared
                .wait_for_change(*observed_signal_epoch, remaining)
                .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
            *observed_signal_epoch = next_epoch;

            if changed {
                let mut snapshot = stats
                    .write()
                    .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
                snapshot.signal_wakeups = snapshot.signal_wakeups.saturating_add(1);
            }

            if stop.load(Ordering::Acquire) {
                return Ok(());
            }

            let next_background = shared
                .background_snapshot()
                .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
            if next_background != *observed_background {
                *observed_background = next_background;
                return Ok(());
            }

            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                return Ok(());
            }
            remaining = timeout.saturating_sub(elapsed);
        }
    };

    while !stop.load(Ordering::Acquire) {
        let snapshot = shared
            .background_snapshot()
            .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
        if let Some(status) = collector.snapshot_tick(&snapshot) {
            let mut snapshot = stats
                .write()
                .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
            snapshot.loops = snapshot.loops.saturating_add(1);
            if matches!(status, BackgroundCollectionStatus::Idle) {
                snapshot.idle_loops = snapshot.idle_loops.saturating_add(1);
            }
            if matches!(status, BackgroundCollectionStatus::Idle) {
                snapshot.snapshot_idle_loops = snapshot.snapshot_idle_loops.saturating_add(1);
            }
            snapshot.collector = collector.stats();
            let wait_for = match status {
                BackgroundCollectionStatus::Idle => config.idle_sleep,
                BackgroundCollectionStatus::Progress(_)
                | BackgroundCollectionStatus::ReadyToFinish(_)
                | BackgroundCollectionStatus::Finished(_) => config.busy_sleep,
            };
            wait_for_signal(
                &stats,
                &shared,
                &stop,
                &mut observed_signal_epoch,
                &mut observed_background,
                wait_for,
            )?;
            continue;
        }

        let status = match shared.try_with_runtime(|runtime| collector.tick(runtime)) {
            Ok(result) => result.map_err(BackgroundWorkerError::Collection)?,
            Err(SharedHeapError::LockPoisoned) => return Err(BackgroundWorkerError::LockPoisoned),
            Err(SharedHeapError::WouldBlock) => {
                let mut snapshot = stats
                    .write()
                    .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
                snapshot.loops = snapshot.loops.saturating_add(1);
                snapshot.idle_loops = snapshot.idle_loops.saturating_add(1);
                snapshot.contention_loops = snapshot.contention_loops.saturating_add(1);
                snapshot.collector = collector.stats();
                wait_for_signal(
                    &stats,
                    &shared,
                    &stop,
                    &mut observed_signal_epoch,
                    &mut observed_background,
                    config.idle_sleep,
                )?;
                continue;
            }
        };

        observed_background = shared
            .background_snapshot()
            .map_err(|_| BackgroundWorkerError::LockPoisoned)?;

        {
            let mut snapshot = stats
                .write()
                .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
            snapshot.loops = snapshot.loops.saturating_add(1);
            if matches!(status, BackgroundCollectionStatus::Idle) {
                snapshot.idle_loops = snapshot.idle_loops.saturating_add(1);
            }
            snapshot.collector = collector.stats();
        }

        let sleep_for = match status {
            BackgroundCollectionStatus::Idle => config.idle_sleep,
            BackgroundCollectionStatus::Progress(_)
            | BackgroundCollectionStatus::ReadyToFinish(_)
            | BackgroundCollectionStatus::Finished(_) => config.busy_sleep,
        };
        wait_for_signal(
            &stats,
            &shared,
            &stop,
            &mut observed_signal_epoch,
            &mut observed_background,
            sleep_for,
        )?;
    }

    Ok(())
}
