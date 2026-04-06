use crate::background::{
    BackgroundCollectionRuntime, SharedBackgroundError, SharedHeap, SharedHeapError,
};
use crate::collector_state::CollectorSharedSnapshot;
use crate::heap::{AllocError, Heap};
use crate::plan::{BackgroundCollectionStatus, CollectionPlan, MajorMarkProgress};
use crate::stats::{CollectionStats, HeapStats};

/// Collector-side runtime bound to one heap.
#[derive(Debug)]
pub struct CollectorRuntime<'heap> {
    heap: &'heap mut Heap,
}

/// Collector-side runtime bound to one shared heap.
#[derive(Clone, Debug)]
pub struct SharedCollectorRuntime {
    heap: SharedHeap,
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

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.heap.finish_active_major_collection_if_ready()
    }

    /// Service one background collection round for the active major-mark session.
    pub fn service_background_collection_round(
        &mut self,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        self.heap.service_background_collection_round()
    }
}

impl SharedCollectorRuntime {
    pub(crate) fn new(heap: SharedHeap) -> Self {
        Self { heap }
    }

    /// Return the shared heap backing this runtime.
    pub fn heap(&self) -> &SharedHeap {
        &self.heap
    }

    fn map_shared_heap_error(error: SharedHeapError) -> SharedBackgroundError {
        match error {
            SharedHeapError::LockPoisoned => SharedBackgroundError::LockPoisoned,
            SharedHeapError::WouldBlock => SharedBackgroundError::WouldBlock,
        }
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> Result<HeapStats, SharedBackgroundError> {
        self.heap.stats().map_err(Self::map_shared_heap_error)
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(
        &self,
    ) -> Result<Option<CollectionPlan>, SharedBackgroundError> {
        self.heap
            .recommended_background_plan()
            .map_err(Self::map_shared_heap_error)
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(&self) -> Result<Option<CollectionPlan>, SharedBackgroundError> {
        self.heap
            .active_major_mark_plan()
            .map_err(Self::map_shared_heap_error)
    }

    /// Return progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Result<Option<MajorMarkProgress>, SharedBackgroundError> {
        self.heap
            .major_mark_progress()
            .map_err(Self::map_shared_heap_error)
    }

    /// Return one consistent collector-visible shared snapshot.
    pub(crate) fn collector_snapshot(
        &self,
    ) -> Result<CollectorSharedSnapshot, SharedBackgroundError> {
        self.heap
            .collector_snapshot()
            .map_err(Self::map_shared_heap_error)
    }

    /// Return one consistent observation of background epoch and collector-visible shared state.
    pub(crate) fn observe_collector_snapshot(
        &self,
    ) -> Result<(u64, CollectorSharedSnapshot), SharedBackgroundError> {
        self.heap
            .observe_collector_snapshot()
            .map_err(Self::map_shared_heap_error)
    }

    /// Begin a persistent major-mark session for one scheduler-provided plan.
    pub fn begin_major_mark(&self, plan: CollectionPlan) -> Result<(), SharedBackgroundError> {
        let collector_snapshot = self
            .heap
            .with_heap_read(|heap| heap.begin_major_mark_in_place_with_snapshot(plan))
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.heap
            .publish_collector_snapshot(collector_snapshot)
            .map_err(Self::map_shared_heap_error)
    }

    /// Begin a persistent major-mark session without blocking on heap contention.
    pub fn try_begin_major_mark(&self, plan: CollectionPlan) -> Result<(), SharedBackgroundError> {
        let collector_snapshot = self
            .heap
            .try_with_heap_read(|heap| heap.begin_major_mark_in_place_with_snapshot(plan))
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.heap
            .publish_collector_snapshot(collector_snapshot)
            .map_err(Self::map_shared_heap_error)
    }

    /// Advance one scheduler-style concurrent major-mark round using the active plan worker
    /// count.
    pub fn poll_active_major_mark(
        &self,
    ) -> Result<Option<MajorMarkProgress>, SharedBackgroundError> {
        let (progress, collector_snapshot) = self
            .heap
            .with_heap_read(|heap| heap.poll_active_major_mark_with_snapshot())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.heap
            .publish_collector_snapshot(collector_snapshot)
            .map_err(Self::map_shared_heap_error)?;
        Ok(progress)
    }

    /// Advance one scheduler-style concurrent major-mark round without blocking on heap
    /// contention.
    pub fn try_poll_active_major_mark(
        &self,
    ) -> Result<Option<MajorMarkProgress>, SharedBackgroundError> {
        let (progress, collector_snapshot) = self
            .heap
            .try_with_heap_read(|heap| heap.poll_active_major_mark_with_snapshot())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.heap
            .publish_collector_snapshot(collector_snapshot)
            .map_err(Self::map_shared_heap_error)?;
        Ok(progress)
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
        self.heap
            .with_runtime(|runtime| runtime.finish_active_major_collection_if_ready())
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
        self.heap
            .try_with_runtime(|runtime| runtime.finish_active_major_collection_if_ready())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)
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

    fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.finish_active_major_collection_if_ready()
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
}
