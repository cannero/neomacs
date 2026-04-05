use crate::background::BackgroundCollectionRuntime;
use crate::heap::{AllocError, Heap};
use crate::plan::{BackgroundCollectionStatus, CollectionPlan, MajorMarkProgress};
use crate::stats::{CollectionStats, HeapStats};

/// Collector-side runtime bound to one heap.
#[derive(Debug)]
pub struct CollectorRuntime<'heap> {
    heap: &'heap mut Heap,
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
