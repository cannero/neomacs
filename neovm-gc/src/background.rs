use crate::heap::{AllocError, Heap};
use crate::mutator::Mutator;
use crate::plan::{BackgroundCollectionStatus, CollectionKind};
use crate::runtime::CollectorRuntime;
use crate::stats::{CollectionStats, HeapStats};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LockResult, Mutex, MutexGuard};
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
}

/// Shared-heap failure modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedHeapError {
    /// Shared heap state was poisoned by another panic.
    LockPoisoned,
}

/// Shared background service failure modes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedBackgroundError {
    /// Shared heap state was poisoned by another panic.
    LockPoisoned,
    /// The collector reported one collection/runtime error.
    Collection(AllocError),
}

impl SharedHeap {
    /// Create a new shared heap with `config`.
    pub fn new(config: crate::heap::HeapConfig) -> Self {
        Self::from_heap(Heap::new(config))
    }

    /// Wrap one heap for shared synchronized access.
    pub fn from_heap(heap: Heap) -> Self {
        Self {
            inner: Arc::new(Mutex::new(heap)),
        }
    }

    /// Lock the underlying heap.
    pub fn lock(&self) -> LockResult<MutexGuard<'_, Heap>> {
        self.inner.lock()
    }

    /// Execute one closure with exclusive access to the underlying heap.
    pub fn with_heap<R>(&self, f: impl FnOnce(&mut Heap) -> R) -> Result<R, SharedHeapError> {
        let mut heap = self
            .inner
            .lock()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&mut heap))
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

    /// Return current heap statistics.
    pub fn stats(&self) -> Result<HeapStats, SharedHeapError> {
        self.with_heap(|heap| heap.stats())
    }

    /// Recommend the next collection plan from current heap pressure.
    pub fn recommended_plan(&self) -> Result<crate::plan::CollectionPlan, SharedHeapError> {
        self.with_heap(|heap| heap.recommended_plan())
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.with_heap(|heap| heap.recommended_background_plan())
    }

    /// Return the last completed plan, if any.
    pub fn last_completed_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.with_heap(|heap| heap.last_completed_plan())
    }

    /// Return the active major-mark plan, if any.
    pub fn active_major_mark_plan(
        &self,
    ) -> Result<Option<crate::plan::CollectionPlan>, SharedHeapError> {
        self.with_heap(|heap| heap.active_major_mark_plan())
    }

    /// Spawn a worker-owned background collector thread for this heap.
    pub fn spawn_background_worker(&self, config: BackgroundWorkerConfig) -> BackgroundWorker {
        BackgroundWorker::spawn(self.clone(), config)
    }

    /// Create a shared background service loop for this heap.
    pub fn background_service(&self, config: BackgroundCollectorConfig) -> SharedBackgroundService {
        SharedBackgroundService::new(self.clone(), config)
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
    /// Background collector coordinator statistics accumulated by the worker.
    pub collector: BackgroundCollectorStats,
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
    stats: Arc<Mutex<BackgroundWorkerStats>>,
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

    /// Return the shared heap backing this service.
    pub fn heap(&self) -> &SharedHeap {
        &self.heap
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
        self.heap
            .with_runtime(|runtime| runtime.major_mark_progress())
    }

    /// Run one background-collection coordinator tick.
    pub fn tick(&mut self) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        self.heap
            .with_runtime(|runtime| self.collector.tick(runtime))
            .map_err(|_| SharedBackgroundError::LockPoisoned)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Service background collection until no active session remains or one collection finishes.
    pub fn run_until_idle(&mut self) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        self.heap
            .with_runtime(|runtime| self.collector.run_until_idle(runtime))
            .map_err(|_| SharedBackgroundError::LockPoisoned)?
            .map_err(SharedBackgroundError::Collection)
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
}

impl BackgroundWorker {
    fn spawn(shared: SharedHeap, config: BackgroundWorkerConfig) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Mutex::new(BackgroundWorkerStats::default()));
        let worker_stop = Arc::clone(&stop);
        let worker_stats = Arc::clone(&stats);
        let handle = thread::spawn(move || worker_loop(shared, config, worker_stop, worker_stats));
        Self {
            stop,
            stats,
            handle: Some(handle),
        }
    }

    /// Request that the worker stop after its current loop iteration.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
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
            .lock()
            .map(|stats| *stats)
            .map_err(|_| BackgroundWorkerError::LockPoisoned)
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
    stats: Arc<Mutex<BackgroundWorkerStats>>,
) -> Result<(), BackgroundWorkerError> {
    let mut collector = BackgroundCollector::new(config.collector);

    while !stop.load(Ordering::Acquire) {
        let status = {
            let mut heap = shared
                .lock()
                .map_err(|_| BackgroundWorkerError::LockPoisoned)?;
            let mut runtime = heap.collector_runtime();
            collector
                .tick(&mut runtime)
                .map_err(BackgroundWorkerError::Collection)?
        };

        {
            let mut snapshot = stats
                .lock()
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
        if !sleep_for.is_zero() {
            thread::sleep(sleep_for);
        }
    }

    Ok(())
}
