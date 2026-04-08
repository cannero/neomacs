use crate::background::BackgroundCollectionRuntime;
use crate::descriptor::Trace;
use crate::edge::EdgeCell;
use crate::heap::{AllocError, Heap};
use crate::plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
use crate::root::{Gc, HandleScope, Root};
use crate::stats::CollectionStats;

/// Mutator view onto the heap.
#[derive(Debug)]
pub struct Mutator<'heap> {
    heap: &'heap mut Heap,
}

impl<'heap> Mutator<'heap> {
    pub(crate) fn new(heap: &'heap mut Heap) -> Self {
        Self { heap }
    }

    /// Create a new rooted handle scope.
    pub fn handle_scope<'scope>(&mut self) -> HandleScope<'scope, 'heap> {
        HandleScope::new(self.heap.root_stack_ptr())
    }

    /// Return a shared view of the underlying heap.
    pub fn heap(&self) -> &Heap {
        self.heap
    }

    /// Allocate one managed object.
    pub fn alloc<'scope, T: Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, 'heap>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        self.heap.collector_runtime().alloc_typed(scope, value)
    }

    /// Allocate one managed object, collecting first if nursery pressure requires it.
    pub fn alloc_auto<'scope, T: Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, 'heap>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        self.heap
            .collector_runtime()
            .prepare_typed_allocation::<T>()?;
        self.heap.collector_runtime().alloc_typed(scope, value)
    }

    /// Create a new rooted handle for an existing managed object.
    pub fn root<'scope, T: ?Sized>(
        &mut self,
        scope: &mut HandleScope<'scope, 'heap>,
        gc: Gc<T>,
    ) -> Root<'scope, T> {
        assert!(
            !self.heap.prepared_full_reclaim_active(),
            "cannot add new roots while prepared full reclaim is active; finish the active full collection first"
        );
        self.heap
            .collector_runtime()
            .root_during_active_major_mark(gc.erase());
        scope.root(gc)
    }

    /// Run one collection cycle against this mutator's heap.
    pub fn collect(&mut self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        self.heap.collector_runtime().collect(kind)
    }

    /// Run physical old-gen compaction against this mutator's
    /// heap. Mirrors [`Heap::compact_old_gen_physical`] but goes
    /// through the mutator's borrow so scoped roots created from
    /// the same mutator can still be dereferenced after the call.
    ///
    /// Returns the number of records physically evacuated.
    pub fn compact_old_gen_physical(&mut self, density_threshold: f64) -> usize {
        self.heap.compact_old_gen_physical(density_threshold)
    }

    /// Aggressive compaction wrapper. Mirrors
    /// [`Heap::compact_old_gen_aggressive`] through the mutator
    /// borrow so scoped roots created from the same mutator
    /// stay valid across the call.
    pub fn compact_old_gen_aggressive(
        &mut self,
        density_threshold: f64,
        max_passes: usize,
    ) -> usize {
        self.heap
            .compact_old_gen_aggressive(density_threshold, max_passes)
    }

    /// Block-targeted compaction wrapper. Mirrors
    /// [`Heap::compact_old_gen_blocks`] through the mutator
    /// borrow so scoped roots created from the same mutator
    /// stay valid across the call.
    pub fn compact_old_gen_blocks(&mut self, block_indices: &[usize]) -> usize {
        self.heap.compact_old_gen_blocks(block_indices)
    }

    /// Predicate-only check for opportunistic compaction.
    /// Mirrors [`Heap::should_compact_old_gen`].
    pub fn should_compact_old_gen(&self, fragmentation_threshold: f64) -> bool {
        self.heap.should_compact_old_gen(fragmentation_threshold)
    }

    /// Reset every counter in [`Heap::compaction_stats`] to
    /// zero. Mirrors [`Heap::clear_compaction_stats`].
    pub fn clear_compaction_stats(&mut self) {
        self.heap.clear_compaction_stats();
    }

    /// Cumulative write-barrier traffic counters. Mirrors
    /// [`Heap::barrier_stats`] through the mutator borrow.
    pub fn barrier_stats(&self) -> crate::stats::BarrierStats {
        self.heap.barrier_stats()
    }

    /// Reset every counter in [`Heap::barrier_stats`] to zero.
    /// Mirrors [`Heap::clear_barrier_stats`].
    pub fn clear_barrier_stats(&mut self) {
        self.heap.clear_barrier_stats();
    }

    /// Read the current nursery fill ratio. Mirrors
    /// [`Heap::nursery_fill_ratio`].
    pub fn nursery_fill_ratio(&self) -> f64 {
        self.heap.nursery_fill_ratio()
    }

    /// Read the current old-gen fragmentation ratio. Mirrors
    /// [`Heap::old_gen_fragmentation_ratio`] but routes through
    /// the mutator's borrow.
    pub fn old_gen_fragmentation_ratio(&self) -> f64 {
        self.heap.old_gen_fragmentation_ratio()
    }

    /// Opportunistic compaction trigger. Mirrors
    /// [`Heap::compact_old_gen_if_fragmented`].
    pub fn compact_old_gen_if_fragmented(
        &mut self,
        fragmentation_threshold: f64,
    ) -> (f64, usize) {
        self.heap
            .compact_old_gen_if_fragmented(fragmentation_threshold)
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> usize {
        self.heap.pending_finalizer_count()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&mut self) -> u64 {
        self.heap.drain_pending_finalizers()
    }

    /// Run at most `max` queued finalizers and return the number
    /// that actually ran. See [`Heap::drain_pending_finalizers_bounded`].
    pub fn drain_pending_finalizers_bounded(&mut self, max: usize) -> u64 {
        self.heap.drain_pending_finalizers_bounded(max)
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> RuntimeWorkStatus {
        self.heap.runtime_work_status()
    }

    /// Build a scheduler-visible collection plan from the current heap state.
    pub fn plan_for(&self, kind: CollectionKind) -> CollectionPlan {
        self.heap.plan_for(kind)
    }

    /// Recommend the next collection plan from current heap state.
    pub fn recommended_plan(&self) -> CollectionPlan {
        self.heap.recommended_plan()
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

    /// Execute one scheduler-provided collection plan.
    pub fn execute_plan(&mut self, plan: CollectionPlan) -> Result<CollectionStats, AllocError> {
        self.heap.collector_runtime().execute_plan(plan)
    }

    /// Begin a persistent major-mark session for one scheduler-provided plan.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.heap.collector_runtime().begin_major_mark(plan)
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&mut self) -> Result<MajorMarkProgress, AllocError> {
        self.heap.collector_runtime().advance_major_mark()
    }

    /// Finish the current persistent major-mark session and reclaim.
    pub fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        self.heap.collector_runtime().finish_major_collection()
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &mut self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.heap.collector_runtime().assist_major_mark(max_slices)
    }

    /// Advance one scheduler-style concurrent major-mark round using the active plan worker count.
    pub fn poll_active_major_mark(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.heap.collector_runtime().poll_active_major_mark()
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained.
    pub fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        self.heap
            .collector_runtime()
            .prepare_active_reclaim_if_needed()
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.heap
            .collector_runtime()
            .finish_active_major_collection_if_ready()
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.heap
            .collector_runtime()
            .commit_active_reclaim_if_ready()
    }

    /// Service one background collection round for the active major-mark session.
    pub fn service_background_collection_round(
        &mut self,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        self.heap
            .collector_runtime()
            .service_background_collection_round()
    }

    /// Record a post-write barrier for one mutated GC edge.
    pub fn post_write_barrier<Owner: ?Sized, Value: ?Sized>(
        &mut self,
        owner: Gc<Owner>,
        slot: Option<usize>,
        old_value: Option<Gc<Value>>,
        new_value: Option<Gc<Value>>,
    ) {
        assert!(
            !self.heap.prepared_full_reclaim_active(),
            "cannot mutate heap edges while prepared full reclaim is active; finish the active full collection first"
        );
        self.heap.collector_runtime().record_post_write(
            owner.erase(),
            slot,
            old_value.map(Gc::erase),
            new_value.map(Gc::erase),
        );
    }

    /// Store a managed edge and record the required post-write barrier.
    pub fn store_edge<'scope, Owner: 'static, Value: ?Sized>(
        &mut self,
        owner: &Root<'scope, Owner>,
        slot: usize,
        project: impl FnOnce(&Owner) -> &EdgeCell<Value>,
        new_value: Option<Gc<Value>>,
    ) {
        assert!(
            !self.heap.prepared_full_reclaim_active(),
            "cannot mutate heap edges while prepared full reclaim is active; finish the active full collection first"
        );
        let owner_ref = unsafe { owner.as_gc().as_non_null().as_ref() };
        let edge = project(owner_ref);
        let old_value = edge.replace(new_value);
        self.post_write_barrier(owner.as_gc(), Some(slot), old_value, new_value);
    }
}

impl BackgroundCollectionRuntime for Mutator<'_> {
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
