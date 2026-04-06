use crate::background::{
    BackgroundCollectionRuntime, BackgroundCollectorConfig, BackgroundWorker,
    BackgroundWorkerConfig, SharedBackgroundError, SharedBackgroundObservation,
    SharedBackgroundService, SharedBackgroundStatus, SharedBackgroundWaitResult,
    SharedCollectorHandle, SharedHeap, SharedHeapError, SharedHeapStatus, SharedRuntimeHandle,
};
use crate::collector_state::{CollectorSharedSnapshot, CollectorState};
use crate::heap::{AllocError, Heap};
use crate::plan::{
    BackgroundCollectionStatus, CollectionPhase, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
use crate::stats::{CollectionStats, HeapStats};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Collector-side runtime bound to one heap.
#[derive(Debug)]
pub struct CollectorRuntime<'heap> {
    heap: &'heap mut Heap,
}

/// Collector-side runtime bound to one shared heap.
#[derive(Clone, Debug)]
pub struct SharedCollectorRuntime {
    heap: SharedHeap,
    runtime: SharedRuntimeHandle,
    collector: SharedCollectorHandle,
}

impl<'heap> CollectorRuntime<'heap> {
    pub(crate) fn new(heap: &'heap mut Heap) -> Self {
        Self { heap }
    }

    /// Return a shared view of the underlying heap.
    pub fn heap(&self) -> &Heap {
        self.heap
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> HeapStats {
        self.heap.stats()
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> usize {
        self.heap.pending_finalizer_count()
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> RuntimeWorkStatus {
        self.heap.runtime_work_status()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&mut self) -> u64 {
        self.heap.drain_pending_finalizers()
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.heap.recommended_background_plan()
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.heap.active_major_mark_plan()
    }

    /// Return progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Option<MajorMarkProgress> {
        self.heap.major_mark_progress()
    }

    /// Begin a persistent major-mark session for one scheduler-provided plan.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.heap.begin_major_mark(plan)
    }

    /// Advance one scheduler-style concurrent major-mark round using the active plan worker count.
    pub fn poll_active_major_mark(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.heap.poll_active_major_mark()
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained.
    pub fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        self.heap.prepare_active_reclaim_if_needed()
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.heap.finish_active_major_collection_if_ready()
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.heap.commit_active_reclaim_if_ready()
    }

    /// Service one background collection round for the active major-mark session.
    pub fn service_background_collection_round(
        &mut self,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        if self.active_major_mark_plan().is_none() {
            return Ok(BackgroundCollectionStatus::Idle);
        }

        let progress = self
            .poll_active_major_mark()?
            .expect("active major-mark session disappeared during service");
        if progress.completed {
            if let Some(cycle) = self.finish_active_major_collection_if_ready()? {
                Ok(BackgroundCollectionStatus::Finished(cycle))
            } else {
                Ok(BackgroundCollectionStatus::ReadyToFinish(progress))
            }
        } else {
            Ok(BackgroundCollectionStatus::Progress(progress))
        }
    }
}

impl SharedCollectorRuntime {
    pub(crate) fn new(heap: SharedHeap) -> Self {
        let runtime = heap.runtime_handle();
        let collector = heap.collector_handle();
        Self {
            heap,
            runtime,
            collector,
        }
    }

    /// Return the shared heap backing this runtime.
    pub fn heap(&self) -> &SharedHeap {
        &self.heap
    }

    /// Create a shared background service loop bound to this runtime.
    pub fn background_service(&self, config: BackgroundCollectorConfig) -> SharedBackgroundService {
        SharedBackgroundService::from_runtime(self.clone(), config)
    }

    /// Spawn a worker-owned background collector thread bound to this runtime.
    pub fn spawn_background_worker(&self, config: BackgroundWorkerConfig) -> BackgroundWorker {
        BackgroundWorker::spawn(self.clone(), config)
    }

    fn map_shared_heap_error(error: SharedHeapError) -> SharedBackgroundError {
        match error {
            SharedHeapError::LockPoisoned => SharedBackgroundError::LockPoisoned,
            SharedHeapError::WouldBlock => SharedBackgroundError::WouldBlock,
        }
    }

    fn publish_collector_snapshot(
        &self,
        next_collector: CollectorSharedSnapshot,
    ) -> Result<(), SharedHeapError> {
        self.runtime.publish_collector_snapshot(next_collector)
    }

    fn with_heap_read_collector_update<R>(
        &self,
        f: impl FnOnce(&Heap, &mut CollectorState) -> Result<R, AllocError>,
    ) -> Result<Result<R, AllocError>, SharedHeapError> {
        let heap = self
            .heap
            .read()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let result = self.collector.with_state(|collector| {
            f(&heap, collector).map(|value| (value, collector.shared_snapshot()))
        })?;
        match result {
            Ok((value, collector_snapshot)) => {
                self.publish_collector_snapshot(collector_snapshot)?;
                Ok(Ok(value))
            }
            Err(error) => Ok(Err(error)),
        }
    }

    fn try_with_heap_read_collector_update<R>(
        &self,
        f: impl FnOnce(&Heap, &mut CollectorState) -> Result<R, AllocError>,
    ) -> Result<Result<R, AllocError>, SharedHeapError> {
        let heap = self.heap.try_read().map_err(|error| match error {
            std::sync::TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            std::sync::TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        let result = self.collector.try_with_state(|collector| {
            f(&heap, collector).map(|value| (value, collector.shared_snapshot()))
        })?;
        match result {
            Ok((value, collector_snapshot)) => {
                self.publish_collector_snapshot(collector_snapshot)?;
                Ok(Ok(value))
            }
            Err(error) => Ok(Err(error)),
        }
    }

    fn with_runtime_update<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut CollectorRuntime<'heap>) -> Result<R, AllocError>,
    ) -> Result<Result<R, AllocError>, SharedHeapError> {
        let mut heap = self
            .heap
            .lock()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let mut runtime = heap.collector_runtime();
        Ok(f(&mut runtime))
    }

    fn try_with_runtime_update<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut CollectorRuntime<'heap>) -> Result<R, AllocError>,
    ) -> Result<Result<R, AllocError>, SharedHeapError> {
        let mut heap = self.heap.try_lock().map_err(|error| match error {
            std::sync::TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            std::sync::TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        let mut runtime = heap.collector_runtime();
        Ok(f(&mut runtime))
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> Result<HeapStats, SharedBackgroundError> {
        self.runtime
            .observe_heap_status()
            .map(|status| status.stats)
            .map_err(Self::map_shared_heap_error)
    }

    /// Return one consistent shared heap status snapshot for this runtime.
    pub fn status(&self) -> Result<SharedHeapStatus, SharedBackgroundError> {
        self.runtime
            .observe_heap_status()
            .map_err(Self::map_shared_heap_error)
    }

    /// Return the current shared-heap change epoch for this runtime.
    pub fn epoch(&self) -> Result<u64, SharedBackgroundError> {
        self.runtime
            .heap_epoch()
            .map_err(Self::map_shared_heap_error)
    }

    /// Wait for one shared-heap change visible to this runtime.
    pub fn wait_for_change(
        &self,
        observed_epoch: u64,
        timeout: Duration,
    ) -> Result<(u64, bool), SharedBackgroundError> {
        self.runtime
            .wait_for_heap_change(observed_epoch, timeout)
            .map_err(Self::map_shared_heap_error)
    }

    pub(crate) fn notify_waiters(&self) {
        self.runtime.notify_heap();
    }

    pub(crate) fn notify_background_waiters(&self) {
        self.collector.notify();
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> Result<usize, SharedBackgroundError> {
        self.runtime
            .pending_finalizer_count()
            .map_err(Self::map_shared_heap_error)
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> Result<RuntimeWorkStatus, SharedBackgroundError> {
        self.runtime
            .runtime_work_status()
            .map_err(Self::map_shared_heap_error)
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&self) -> Result<u64, SharedBackgroundError> {
        self.runtime
            .drain_pending_finalizers()
            .map_err(Self::map_shared_heap_error)
    }

    /// Run and drain queued finalizers without blocking on heap contention.
    pub fn try_drain_pending_finalizers(&self) -> Result<u64, SharedBackgroundError> {
        self.runtime
            .try_drain_pending_finalizers()
            .map_err(Self::map_shared_heap_error)
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(
        &self,
    ) -> Result<Option<CollectionPlan>, SharedBackgroundError> {
        self.collector
            .read_snapshot(|snapshot| snapshot.recommended_background_plan.clone())
            .map_err(Self::map_shared_heap_error)
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(&self) -> Result<Option<CollectionPlan>, SharedBackgroundError> {
        self.collector
            .read_snapshot(|snapshot| snapshot.active_major_mark_plan.clone())
            .map_err(Self::map_shared_heap_error)
    }

    /// Return progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Result<Option<MajorMarkProgress>, SharedBackgroundError> {
        self.collector
            .read_snapshot(|snapshot| snapshot.major_mark_progress)
            .map_err(Self::map_shared_heap_error)
    }

    /// Return one consistent collector-visible shared snapshot.
    pub(crate) fn collector_snapshot(
        &self,
    ) -> Result<CollectorSharedSnapshot, SharedBackgroundError> {
        self.collector
            .snapshot()
            .map_err(Self::map_shared_heap_error)
    }

    pub(crate) fn collector_observation(
        &self,
    ) -> Result<(u64, CollectorSharedSnapshot), SharedBackgroundError> {
        loop {
            let before_epoch = self.background_epoch()?;
            let snapshot = self.collector_snapshot()?;
            let after_epoch = self.background_epoch()?;
            if before_epoch == after_epoch {
                return Ok((after_epoch, snapshot));
            }
        }
    }

    pub(crate) fn wait_for_collector_change(
        &self,
        observed_epoch: &mut u64,
        observed_snapshot: &mut CollectorSharedSnapshot,
        timeout: Duration,
        stop: Option<&AtomicBool>,
    ) -> Result<(bool, bool), SharedBackgroundError> {
        if timeout.is_zero() {
            return Ok((false, false));
        }

        let started_at = Instant::now();
        let mut remaining = timeout;
        let mut signal_changed = false;
        loop {
            let (next_epoch, changed) = self
                .collector
                .wait_for_change(*observed_epoch, remaining)
                .map_err(Self::map_shared_heap_error)?;
            *observed_epoch = next_epoch;
            signal_changed |= changed;

            if stop.is_some_and(|stop| stop.load(std::sync::atomic::Ordering::Acquire)) {
                return Ok((signal_changed, false));
            }

            let next_snapshot = self.collector_snapshot()?;
            if next_snapshot != *observed_snapshot {
                *observed_snapshot = next_snapshot;
                return Ok((signal_changed, true));
            }

            if changed {
                return Ok((signal_changed, false));
            }

            let elapsed = started_at.elapsed();
            if elapsed >= timeout {
                return Ok((signal_changed, false));
            }
            remaining = timeout.saturating_sub(elapsed);
        }
    }

    /// Return the current background-state change epoch for this runtime.
    pub fn background_epoch(&self) -> Result<u64, SharedBackgroundError> {
        self.collector.epoch().map_err(Self::map_shared_heap_error)
    }

    /// Return background-collector-visible shared heap state for this runtime.
    pub fn background_status(&self) -> Result<SharedBackgroundStatus, SharedBackgroundError> {
        self.runtime
            .observe_background_status()
            .map_err(Self::map_shared_heap_error)
    }

    /// Return one consistent observation of background epoch and background-visible shared heap
    /// state for this runtime.
    pub fn background_observation(
        &self,
    ) -> Result<SharedBackgroundObservation, SharedBackgroundError> {
        self.runtime
            .observe_background_status_with_epoch()
            .map(|(epoch, status)| SharedBackgroundObservation { epoch, status })
            .map_err(Self::map_shared_heap_error)
    }

    /// Wait for one background-collector-visible shared heap state change for this runtime.
    pub fn wait_for_background_change(
        &self,
        observed_epoch: u64,
        observed_status: &SharedBackgroundStatus,
        timeout: Duration,
    ) -> Result<SharedBackgroundWaitResult, SharedBackgroundError> {
        let mut observed_epoch = observed_epoch;
        let mut observed_status = observed_status.clone();
        self.runtime
            .wait_for_background_change(&mut observed_epoch, &mut observed_status, timeout, None)
            .map_err(Self::map_shared_heap_error)
    }

    /// Begin a persistent major-mark session for one scheduler-provided plan.
    pub fn begin_major_mark(&self, plan: CollectionPlan) -> Result<(), SharedBackgroundError> {
        self.with_heap_read_collector_update(|heap, collector| {
            heap.begin_major_mark_with_collector(collector, plan)
        })
        .map_err(Self::map_shared_heap_error)?
        .map_err(SharedBackgroundError::Collection)
    }

    /// Begin a persistent major-mark session without blocking on heap contention.
    pub fn try_begin_major_mark(&self, plan: CollectionPlan) -> Result<(), SharedBackgroundError> {
        self.try_with_heap_read_collector_update(|heap, collector| {
            heap.begin_major_mark_with_collector(collector, plan)
        })
        .map_err(Self::map_shared_heap_error)?
        .map_err(SharedBackgroundError::Collection)
    }

    /// Advance one scheduler-style concurrent major-mark round using the active plan worker
    /// count.
    pub fn poll_active_major_mark(
        &self,
    ) -> Result<Option<MajorMarkProgress>, SharedBackgroundError> {
        self.with_heap_read_collector_update(|heap, collector| {
            heap.poll_active_major_mark_with_collector(collector)
        })
        .map_err(Self::map_shared_heap_error)?
        .map_err(SharedBackgroundError::Collection)
    }

    /// Advance one scheduler-style concurrent major-mark round without blocking on heap
    /// contention.
    pub fn try_poll_active_major_mark(
        &self,
    ) -> Result<Option<MajorMarkProgress>, SharedBackgroundError> {
        self.try_with_heap_read_collector_update(|heap, collector| {
            heap.poll_active_major_mark_with_collector(collector)
        })
        .map_err(Self::map_shared_heap_error)?
        .map_err(SharedBackgroundError::Collection)
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained.
    pub fn prepare_active_reclaim_if_needed(&self) -> Result<bool, SharedBackgroundError> {
        let snapshot = self.collector_snapshot()?;
        if snapshot.active_major_mark_plan.is_none() {
            return Ok(false);
        }
        if snapshot
            .major_mark_progress
            .is_some_and(|progress| !progress.completed)
        {
            return Ok(false);
        }
        if snapshot
            .active_major_mark_plan
            .as_ref()
            .is_some_and(|plan| plan.kind == crate::plan::CollectionKind::Major)
        {
            return self
                .with_heap_read_collector_update(|heap, collector| {
                    heap.prepare_active_major_reclaim_with_collector(collector)
                })
                .map_err(Self::map_shared_heap_error)?
                .map_err(SharedBackgroundError::Collection);
        }
        self.with_runtime_update(|runtime| runtime.prepare_active_reclaim_if_needed())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained, without
    /// blocking on heap contention.
    pub fn try_prepare_active_reclaim_if_needed(&self) -> Result<bool, SharedBackgroundError> {
        let snapshot = self.collector_snapshot()?;
        if snapshot.active_major_mark_plan.is_none() {
            return Ok(false);
        }
        if snapshot
            .major_mark_progress
            .is_some_and(|progress| !progress.completed)
        {
            return Ok(false);
        }
        if snapshot
            .active_major_mark_plan
            .as_ref()
            .is_some_and(|plan| plan.kind == crate::plan::CollectionKind::Major)
        {
            return self
                .try_with_heap_read_collector_update(|heap, collector| {
                    heap.prepare_active_major_reclaim_with_collector(collector)
                })
                .map_err(Self::map_shared_heap_error)?
                .map_err(SharedBackgroundError::Collection);
        }
        self.try_with_runtime_update(|runtime| runtime.prepare_active_reclaim_if_needed())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        let snapshot = self.collector_snapshot()?;
        if snapshot.active_major_mark_plan.is_none() {
            return Ok(None);
        }
        if snapshot
            .major_mark_progress
            .is_some_and(|progress| !progress.completed)
        {
            return Ok(None);
        }
        if snapshot
            .active_major_mark_plan
            .as_ref()
            .is_some_and(|plan| {
                plan.kind == crate::plan::CollectionKind::Major
                    && plan.phase != CollectionPhase::Reclaim
            })
        {
            if self.prepare_active_reclaim_if_needed()? {
                return Ok(None);
            }
        }
        self.with_runtime_update(|runtime| runtime.finish_active_major_collection_if_ready())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        let snapshot = self.collector_snapshot()?;
        if snapshot.active_major_mark_plan.is_none() {
            return Ok(None);
        }
        if snapshot
            .major_mark_progress
            .is_some_and(|progress| !progress.completed)
        {
            return Ok(None);
        }
        if snapshot
            .active_major_mark_plan
            .as_ref()
            .is_some_and(|plan| plan.phase != CollectionPhase::Reclaim)
        {
            return Ok(None);
        }
        self.with_runtime_update(|runtime| runtime.commit_active_reclaim_if_ready())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Finish the active major collection if its mark work is fully drained, without blocking on
    /// heap contention.
    pub fn try_finish_active_major_collection_if_ready(
        &self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        let snapshot = self.collector_snapshot()?;
        if snapshot.active_major_mark_plan.is_none() {
            return Ok(None);
        }
        if snapshot
            .major_mark_progress
            .is_some_and(|progress| !progress.completed)
        {
            return Ok(None);
        }
        if snapshot
            .active_major_mark_plan
            .as_ref()
            .is_some_and(|plan| {
                plan.kind == crate::plan::CollectionKind::Major
                    && plan.phase != CollectionPhase::Reclaim
            })
        {
            if self.try_prepare_active_reclaim_if_needed()? {
                return Ok(None);
            }
        }
        self.try_with_runtime_update(|runtime| runtime.finish_active_major_collection_if_ready())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Commit the active major collection once reclaim has already been prepared, without
    /// blocking on heap contention.
    pub fn try_commit_active_reclaim_if_ready(
        &self,
    ) -> Result<Option<CollectionStats>, SharedBackgroundError> {
        let snapshot = self.collector_snapshot()?;
        if snapshot.active_major_mark_plan.is_none() {
            return Ok(None);
        }
        if snapshot
            .major_mark_progress
            .is_some_and(|progress| !progress.completed)
        {
            return Ok(None);
        }
        if snapshot
            .active_major_mark_plan
            .as_ref()
            .is_some_and(|plan| plan.phase != CollectionPhase::Reclaim)
        {
            return Ok(None);
        }
        self.try_with_runtime_update(|runtime| runtime.commit_active_reclaim_if_ready())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)
    }

    /// Service one background collection round for the active major-mark session.
    pub fn service_background_collection_round(
        &self,
    ) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        if self.active_major_mark_plan()?.is_none() {
            return Ok(BackgroundCollectionStatus::Idle);
        }

        let Some(progress) = self.poll_active_major_mark()? else {
            return Ok(BackgroundCollectionStatus::Idle);
        };
        if progress.completed {
            match self.try_prepare_active_reclaim_if_needed() {
                Ok(true) => return Ok(BackgroundCollectionStatus::ReadyToFinish(progress)),
                Ok(false) | Err(SharedBackgroundError::WouldBlock) => {}
                Err(error) => return Err(error),
            }
            match self.try_commit_active_reclaim_if_ready() {
                Ok(Some(cycle)) => Ok(BackgroundCollectionStatus::Finished(cycle)),
                Ok(None) | Err(SharedBackgroundError::WouldBlock) => {
                    Ok(BackgroundCollectionStatus::ReadyToFinish(progress))
                }
                Err(error) => Err(error),
            }
        } else {
            Ok(BackgroundCollectionStatus::Progress(progress))
        }
    }

    /// Service one background collection round for the active major-mark session without blocking
    /// on heap contention.
    pub fn try_service_background_collection_round(
        &self,
    ) -> Result<BackgroundCollectionStatus, SharedBackgroundError> {
        if self.active_major_mark_plan()?.is_none() {
            return Ok(BackgroundCollectionStatus::Idle);
        }

        let Some(progress) = self.try_poll_active_major_mark()? else {
            return Ok(BackgroundCollectionStatus::Idle);
        };
        if progress.completed {
            match self.try_prepare_active_reclaim_if_needed() {
                Ok(true) => Ok(BackgroundCollectionStatus::ReadyToFinish(progress)),
                Ok(false) => {
                    if let Some(cycle) = self.try_commit_active_reclaim_if_ready()? {
                        Ok(BackgroundCollectionStatus::Finished(cycle))
                    } else {
                        Ok(BackgroundCollectionStatus::ReadyToFinish(progress))
                    }
                }
                Err(SharedBackgroundError::WouldBlock) => {
                    Ok(BackgroundCollectionStatus::ReadyToFinish(progress))
                }
                Err(error) => Err(error),
            }
        } else {
            Ok(BackgroundCollectionStatus::Progress(progress))
        }
    }
}

impl BackgroundCollectionRuntime for CollectorRuntime<'_> {
    fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.active_major_mark_plan()
    }

    fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.recommended_background_plan()
    }

    fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.begin_major_mark(plan)
    }

    fn poll_background_mark_round(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.poll_active_major_mark()
    }

    fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        self.prepare_active_reclaim_if_needed()
    }

    fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.finish_active_major_collection_if_ready()
    }

    fn commit_active_reclaim_if_ready(&mut self) -> Result<Option<CollectionStats>, AllocError> {
        self.commit_active_reclaim_if_ready()
    }
}

impl BackgroundCollectionRuntime for SharedCollectorRuntime {
    fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        SharedCollectorRuntime::active_major_mark_plan(self)
            .expect("shared collector runtime should not be poisoned")
    }

    fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        SharedCollectorRuntime::recommended_background_plan(self)
            .expect("shared collector runtime should not be poisoned")
    }

    fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        SharedCollectorRuntime::begin_major_mark(self, plan).map_err(|error| match error {
            SharedBackgroundError::LockPoisoned | SharedBackgroundError::WouldBlock => {
                AllocError::CollectionInProgress
            }
            SharedBackgroundError::Collection(error) => error,
        })
    }

    fn poll_background_mark_round(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        SharedCollectorRuntime::poll_active_major_mark(self).map_err(|error| match error {
            SharedBackgroundError::LockPoisoned | SharedBackgroundError::WouldBlock => {
                AllocError::CollectionInProgress
            }
            SharedBackgroundError::Collection(error) => error,
        })
    }

    fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        SharedCollectorRuntime::prepare_active_reclaim_if_needed(self).map_err(
            |error| match error {
                SharedBackgroundError::LockPoisoned | SharedBackgroundError::WouldBlock => {
                    AllocError::CollectionInProgress
                }
                SharedBackgroundError::Collection(error) => error,
            },
        )
    }

    fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        SharedCollectorRuntime::finish_active_major_collection_if_ready(self).map_err(|error| {
            match error {
                SharedBackgroundError::LockPoisoned | SharedBackgroundError::WouldBlock => {
                    AllocError::CollectionInProgress
                }
                SharedBackgroundError::Collection(error) => error,
            }
        })
    }

    fn commit_active_reclaim_if_ready(&mut self) -> Result<Option<CollectionStats>, AllocError> {
        SharedCollectorRuntime::commit_active_reclaim_if_ready(self).map_err(|error| match error {
            SharedBackgroundError::LockPoisoned | SharedBackgroundError::WouldBlock => {
                AllocError::CollectionInProgress
            }
            SharedBackgroundError::Collection(error) => error,
        })
    }
}
