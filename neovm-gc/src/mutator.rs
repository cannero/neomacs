use crate::background::BackgroundCollectionRuntime;
use crate::barrier::{BarrierEvent, BarrierKind};
use crate::descriptor::{GcErased, Trace};
use crate::edge::EdgeCell;
use crate::heap::{AllocError, Heap};
use crate::plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
use crate::root::{Gc, HandleScope, Root, RootStack};
use crate::stats::CollectionStats;
use core::ptr::NonNull;

/// Bounded ring retained per mutator for diagnostic
/// inspection of recent barrier events. The count matches
/// the legacy heap-wide ring size so external observers that
/// paginated through the heap ring see the same horizon when
/// they migrate to the mutator-side accessor.
const MAX_BARRIER_EVENTS: usize = 1024;

/// Per-mutator local state.
///
/// Holds data that in the final multi-mutator architecture
/// belongs to one mutator instance and must not be shared
/// across mutators even when they allocate against the same
/// heap: the per-mutator nursery TLAB slab, the per-mutator
/// barrier event ring, and the per-mutator root stack.
#[derive(Debug, Default)]
pub struct MutatorLocal {
    /// Per-mutator nursery TLAB slab. Carved out of the
    /// shared `NurseryState::from_space` via
    /// `NurseryState::reserve_tlab`. Local bumps within the
    /// slab never touch the shared from-space cursor; slab
    /// refill takes the heap lock briefly.
    ///
    /// Invalidation is automatic via the generation stamp
    /// on the TLAB: a post-minor-cycle `swap_spaces_and_reset`
    /// increments the nursery generation, and the next
    /// `try_alloc` against the stale slab returns `None`,
    /// forcing a refill.
    pub(crate) tlab: Option<crate::spaces::nursery_arena::NurseryTlab>,
    /// Bounded diagnostic ring of the most recent barrier
    /// events this mutator has recorded. Updated by
    /// [`MutatorLocal::push_barrier_event`] from the barrier
    /// path in [`crate::runtime::CollectorRuntime::record_post_write_with_local`].
    /// External observers read it via
    /// [`Mutator::recent_barrier_events`].
    ///
    /// Per-mutator ownership (vs a shared heap-wide ring)
    /// removes contention on the ring's write lock in the
    /// multi-mutator target and matches the "per-mutator
    /// diagnostic ring" design in DESIGN.md Appendix A.
    pub(crate) barrier_events: Vec<BarrierEvent>,
    /// Per-mutator root stack. Each `HandleScope` pushes
    /// slots into its mutator's local stack; collection
    /// walks them during stop-the-world cycles and during
    /// concurrent major mark.
    ///
    /// In the single-mutator world this was heap-owned via
    /// `HeapCore::roots`. Moving it onto `MutatorLocal` lets
    /// the collector walk multiple mutators' root stacks
    /// independently in the multi-mutator target. Today's
    /// collector still operates on a single `&mut MutatorLocal`
    /// threaded in from the caller.
    pub(crate) roots: RootStack,
}

impl MutatorLocal {
    /// Return a `NonNull` pointer into this local's root
    /// stack. The pointer is safe to use as long as
    /// `self` is not moved; `HandleScope` relies on stable
    /// addressing of the backing vector while the scope is
    /// alive, which in turn relies on the `MutatorLocal`
    /// itself being pinned by a `&mut` borrow for the scope
    /// duration.
    pub(crate) fn root_stack_ptr(&mut self) -> NonNull<RootStack> {
        NonNull::from(&mut self.roots)
    }

    /// Crate-internal accessor for the root stack. Used by
    /// collector entry points that need to iterate live
    /// roots during a cycle.
    #[allow(dead_code)]
    pub(crate) fn roots(&self) -> &RootStack {
        &self.roots
    }

    /// Mutable accessor for the root stack.
    pub(crate) fn roots_mut(&mut self) -> &mut RootStack {
        &mut self.roots
    }
}

impl MutatorLocal {
    /// Append one barrier event to this mutator's recent
    /// ring, dropping the oldest entries when the ring grows
    /// past `MAX_BARRIER_EVENTS`. Mirrors the trimming the
    /// heap-wide ring did before the move.
    pub(crate) fn push_barrier_event(
        &mut self,
        kind: BarrierKind,
        owner: GcErased,
        slot: Option<usize>,
        old_value: Option<GcErased>,
        new_value: Option<GcErased>,
    ) {
        self.barrier_events.push(BarrierEvent {
            kind,
            owner: unsafe { Gc::from_erased(owner) },
            slot,
            old_value: old_value.map(|value| unsafe { Gc::from_erased(value) }),
            new_value: new_value.map(|value| unsafe { Gc::from_erased(value) }),
        });
        if self.barrier_events.len() > MAX_BARRIER_EVENTS {
            let overflow = self.barrier_events.len() - MAX_BARRIER_EVENTS;
            self.barrier_events.drain(..overflow);
        }
    }
}

/// Mutator view onto the heap.
///
/// Holds a shared `&Heap` borrow plus a per-mutator
/// `MutatorLocal`. Multiple mutators can coexist against
/// the same heap because they all borrow `&Heap`. Each
/// collector operation briefly acquires the heap core write
/// lock via `with_runtime` and releases it at the end of
/// the method.
#[derive(Debug)]
pub struct Mutator<'heap> {
    heap: &'heap Heap,
    local: MutatorLocal,
}

impl<'heap> Mutator<'heap> {
    pub(crate) fn new(heap: &'heap Heap) -> Self {
        Self {
            heap,
            local: MutatorLocal::default(),
        }
    }

    /// Acquire the heap core write lock and run the closure
    /// with a live `CollectorRuntime` built against the lock
    /// guard plus this mutator's local. The lock is released
    /// when the closure returns.
    fn with_runtime<R>(
        &mut self,
        f: impl FnOnce(&mut crate::runtime::CollectorRuntime<'_>) -> R,
    ) -> R {
        let mut guard = self.heap.write_core();
        let mut runtime =
            crate::runtime::CollectorRuntime::with_local(&mut guard, &mut self.local);
        f(&mut runtime)
    }

    /// Create a new rooted handle scope backed by this
    /// mutator's per-local root stack.
    pub fn handle_scope<'scope>(&mut self) -> HandleScope<'scope, 'heap> {
        HandleScope::new(self.local.root_stack_ptr())
    }

    /// Return a shared view of the underlying heap.
    pub fn heap(&self) -> &Heap {
        self.heap
    }

    /// Allocate one managed object.
    ///
    /// Nursery allocations bump within this mutator's TLAB
    /// slab on the fast path. On TLAB miss (including the
    /// post-minor-cycle stale case), the slab is refilled
    /// from the shared from-space via
    /// `NurseryState::reserve_tlab` and the allocation
    /// retries once. On refill failure the allocation falls
    /// through to the shared-cursor bump path and finally
    /// to the system allocator. Non-nursery allocations
    /// bypass the TLAB entirely.
    pub fn alloc<'scope, T: Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, 'heap>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        self.with_runtime(|runtime| runtime.alloc_typed_scoped(scope, value))
    }

    /// Allocate one managed object, collecting first if nursery pressure requires it.
    pub fn alloc_auto<'scope, T: Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, 'heap>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        self.with_runtime(|runtime| {
            runtime.prepare_typed_allocation::<T>()?;
            runtime.alloc_typed_scoped(scope, value)
        })
    }

    /// Return whether this mutator currently holds a
    /// reserved nursery TLAB slab. Test-only observer.
    #[cfg(test)]
    pub(crate) fn has_nursery_tlab(&self) -> bool {
        self.local.tlab.is_some()
    }

    /// Number of live root slots in this mutator's local
    /// root stack. Test-only observer.
    #[cfg(test)]
    pub(crate) fn root_slot_count(&self) -> usize {
        self.local.roots.len()
    }

    /// Drop any per-mutator nursery TLAB slab. Test-only
    /// helper; the generation stamp on the TLAB already
    /// handles staleness automatically across collection
    /// boundaries, so production code has no reason to call
    /// this.
    #[cfg(test)]
    pub(crate) fn invalidate_nursery_tlab(&mut self) {
        self.local.tlab = None;
    }

    /// Create a new rooted handle for an existing managed object.
    pub fn root<'scope, T: ?Sized>(
        &mut self,
        scope: &mut HandleScope<'scope, 'heap>,
        gc: Gc<T>,
    ) -> Root<'scope, T> {
        assert!(
            !self.heap.read_core().prepared_full_reclaim_active(),
            "cannot add new roots while prepared full reclaim is active; finish the active full collection first"
        );
        self.with_runtime(|runtime| runtime.root_during_active_major_mark(gc.erase()));
        scope.root(gc)
    }

    /// Run one collection cycle against this mutator's heap.
    pub fn collect(&mut self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        self.with_runtime(|runtime| runtime.collect(kind))
    }

    /// Run physical old-gen compaction against this mutator's
    /// heap. Mirrors [`Heap::compact_old_gen_physical`] but goes
    /// through the mutator's borrow so scoped roots created from
    /// the same mutator can still be dereferenced after the call.
    ///
    /// Returns the number of records physically evacuated.
    pub fn compact_old_gen_physical(&mut self, density_threshold: f64) -> usize {
        let mut guard = self.heap.write_core();
        guard.compact_old_gen_physical(self.local.roots_mut(), density_threshold)
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
        let mut guard = self.heap.write_core();
        guard.compact_old_gen_aggressive(self.local.roots_mut(), density_threshold, max_passes)
    }

    /// Block-targeted compaction wrapper. Mirrors
    /// [`Heap::compact_old_gen_blocks`] through the mutator
    /// borrow so scoped roots created from the same mutator
    /// stay valid across the call.
    pub fn compact_old_gen_blocks(&mut self, block_indices: &[usize]) -> usize {
        let mut guard = self.heap.write_core();
        guard.compact_old_gen_blocks(self.local.roots_mut(), block_indices)
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

    /// Return the bounded ring of recent barrier events
    /// recorded by this mutator. Events recorded through
    /// other mutators are not visible here.
    ///
    /// Per-mutator ownership of the ring removes contention
    /// on the ring write lock in the multi-mutator target.
    pub fn recent_barrier_events(&self) -> &[BarrierEvent] {
        &self.local.barrier_events
    }

    /// Return the number of recent barrier events retained
    /// in this mutator's diagnostic ring.
    pub fn barrier_event_count(&self) -> usize {
        self.local.barrier_events.len()
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
        let mut guard = self.heap.write_core();
        guard.compact_old_gen_if_fragmented(self.local.roots_mut(), fragmentation_threshold)
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
        self.with_runtime(|runtime| runtime.execute_plan(plan))
    }

    /// Begin a persistent major-mark session for one scheduler-provided plan.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.with_runtime(|runtime| runtime.begin_major_mark(plan))
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&mut self) -> Result<MajorMarkProgress, AllocError> {
        self.with_runtime(|runtime| runtime.advance_major_mark())
    }

    /// Finish the current persistent major-mark session and reclaim.
    pub fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        self.with_runtime(|runtime| runtime.finish_major_collection())
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &mut self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.with_runtime(|runtime| runtime.assist_major_mark(max_slices))
    }

    /// Advance one scheduler-style concurrent major-mark round using the active plan worker count.
    pub fn poll_active_major_mark(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.with_runtime(|runtime| runtime.poll_active_major_mark())
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained.
    pub fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        self.with_runtime(|runtime| runtime.prepare_active_reclaim_if_needed())
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.with_runtime(|runtime| runtime.finish_active_major_collection_if_ready())
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        self.with_runtime(|runtime| runtime.commit_active_reclaim_if_ready())
    }

    /// Service one background collection round for the active major-mark session.
    pub fn service_background_collection_round(
        &mut self,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        self.with_runtime(|runtime| runtime.service_background_collection_round())
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
            !self.heap.read_core().prepared_full_reclaim_active(),
            "cannot mutate heap edges while prepared full reclaim is active; finish the active full collection first"
        );
        let owner_erased = owner.erase();
        let old_erased = old_value.map(Gc::erase);
        let new_erased = new_value.map(Gc::erase);
        self.with_runtime(|runtime| {
            runtime.record_post_write(owner_erased, slot, old_erased, new_erased)
        });
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
            !self.heap.read_core().prepared_full_reclaim_active(),
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
