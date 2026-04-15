use crate::background::BackgroundCollectionRuntime;
use crate::barrier::{BarrierEvent, BarrierKind};
use crate::descriptor::{GcErased, Trace, TypeDesc};
use crate::edge::EdgeCell;
use crate::heap::{AllocError, Heap};
use crate::plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
use crate::root::{Gc, HandleScope, Root, RootStack};
use crate::stats::CollectionStats;
use core::ptr::NonNull;
use std::any::TypeId;

/// Bounded ring retained per mutator for diagnostic
/// inspection of recent barrier events. The count matches
/// the legacy heap-wide ring size so external observers that
/// paginated through the heap ring see the same horizon when
/// they migrate to the mutator-side accessor.
const MAX_BARRIER_EVENTS: usize = 1024;

/// High-water mark at which
/// [`MutatorLocal::push_barrier_event`] drains the ring
/// back down to [`MAX_BARRIER_EVENTS`]. Set to `2 * MAX` so
/// the O(MAX) `Vec::drain` (which shifts the surviving
/// suffix forward) only fires once per `MAX` pushes,
/// amortizing to O(1) per barrier event.
///
/// Before this amortization was introduced the drain fired
/// on *every* push once the ring hit MAX, turning each
/// barrier call into an O(MAX) `memmove`. A flamegraph of
/// the `multi_mutator_scaling/store_edge` bench showed
/// 88.5% of CPU cycles in `__memmove_avx_unaligned_erms`
/// via `push_barrier_event`'s drain path. Amortizing the
/// drain drops that overhead to O(1) per push at the cost
/// of letting the ring grow to `BARRIER_EVENT_HIGH_WATER`
/// before trimming — the visible ring size now varies
/// between `MAX_BARRIER_EVENTS` and
/// `BARRIER_EVENT_HIGH_WATER`, both bounded.
const BARRIER_EVENT_HIGH_WATER: usize = 2 * MAX_BARRIER_EVENTS;

/// Per-mutator local state.
///
/// Holds data that in the final multi-mutator architecture
/// belongs to one mutator instance and must not be shared
/// across mutators even when they allocate against the same
/// heap: the per-mutator nursery TLAB slab, the per-mutator
/// barrier event ring, and the per-mutator root stack.
#[derive(Debug)]
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
    /// Single-entry descriptor cache for the most recently
    /// allocated payload type in this mutator.
    descriptor_cache: Option<(TypeId, &'static TypeDesc)>,
    /// Per-mutator object-store publish reservations. Each
    /// reservation owns one append-only shard chunk so the
    /// common allocation path only has to touch the shard
    /// index lock, not the object storage lock, once the
    /// chunk is reserved.
    publish_local: crate::object_store::ObjectPublishLocal,
    /// Mutator-owned allocation counter slot plus cached
    /// running totals mirrored into the shared heap stats.
    alloc_counter_local: crate::stats::AllocationCounterLocal,
}

impl Default for MutatorLocal {
    fn default() -> Self {
        Self {
            tlab: None,
            barrier_events: Vec::new(),
            roots: RootStack::default(),
            descriptor_cache: None,
            publish_local: crate::object_store::ObjectPublishLocal::default(),
            alloc_counter_local: crate::stats::AllocationCounterLocal::default(),
        }
    }
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

    fn cached_descriptor<T: Trace + 'static>(&self) -> Option<&'static TypeDesc> {
        self.descriptor_cache
            .and_then(|(type_id, desc)| (type_id == TypeId::of::<T>()).then_some(desc))
    }

    fn remember_descriptor<T: Trace + 'static>(&mut self, desc: &'static TypeDesc) {
        self.descriptor_cache = Some((TypeId::of::<T>(), desc));
    }

    pub(crate) fn publish_local_mut(&mut self) -> &mut crate::object_store::ObjectPublishLocal {
        &mut self.publish_local
    }

    pub(crate) fn publish_and_alloc_counter_local_mut(
        &mut self,
    ) -> (
        &mut crate::object_store::ObjectPublishLocal,
        &mut crate::stats::AllocationCounterLocal,
    ) {
        (&mut self.publish_local, &mut self.alloc_counter_local)
    }

    pub(crate) fn set_alloc_counter_local(
        &mut self,
        local: crate::stats::AllocationCounterLocal,
    ) {
        self.alloc_counter_local = local;
    }

    pub(crate) fn alloc_counter_local_mut(
        &mut self,
    ) -> &mut crate::stats::AllocationCounterLocal {
        &mut self.alloc_counter_local
    }

    #[cfg(test)]
    pub(crate) fn has_alloc_counter_local(&self) -> bool {
        self.alloc_counter_local.is_registered()
    }
}

impl MutatorLocal {
    /// Append one barrier event to this mutator's recent
    /// ring, dropping the oldest entries when the ring
    /// exceeds [`BARRIER_EVENT_HIGH_WATER`]. The drain is
    /// amortized: it runs once every `MAX_BARRIER_EVENTS`
    /// pushes and trims the ring back down to
    /// `MAX_BARRIER_EVENTS`, so the per-push cost is O(1)
    /// on average. The ring size visible to external
    /// observers therefore varies between `MAX_BARRIER_EVENTS`
    /// and `BARRIER_EVENT_HIGH_WATER`.
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
        if self.barrier_events.len() > BARRIER_EVENT_HIGH_WATER {
            let overflow = self.barrier_events.len() - MAX_BARRIER_EVENTS;
            self.barrier_events.drain(..overflow);
        }
    }
}

/// Mutator view onto the heap.
///
/// Holds a shared `&Heap` borrow plus a per-mutator
/// `MutatorLocal`. Multiple mutators can coexist against
/// the same heap because they all borrow `&Heap`.
/// Collector-style operations take the safepoint write lock
/// through `with_runtime`; the common allocation path holds
/// only a safepoint read lock and takes the heap-core write
/// lock only when it actually needs shared heap state.
#[derive(Debug)]
pub struct Mutator<'heap> {
    heap: &'heap Heap,
    local: MutatorLocal,
    handle_scope_state: crate::root::HandleScopeState<'heap>,
}

impl<'heap> Mutator<'heap> {
    pub(crate) fn new(heap: &'heap Heap) -> Self {
        let mut local = MutatorLocal::default();
        local.set_alloc_counter_local(heap.allocation_counter_local());
        Self {
            heap,
            local,
            handle_scope_state: crate::root::HandleScopeState::new(heap),
        }
    }

    /// Acquire the safepoint write lock plus the heap-core
    /// write lock and run the closure with a live
    /// `CollectorRuntime` built against those guards plus
    /// this mutator's local. Used by collection and other
    /// collector-style operations that must exclude other
    /// mutators.
    fn with_runtime<R>(
        &mut self,
        f: impl FnOnce(&mut crate::runtime::CollectorRuntime<'_>) -> R,
    ) -> R {
        self.handle_scope_state.release_safepoint();
        let _safepoint = self.heap.write_safepoint();
        let refresh_plans = self.heap.take_collector_plans_dirty();
        let mut guard = self.heap.write_core();
        if refresh_plans {
            guard.refresh_recommended_plans();
        }
        let mut runtime = crate::runtime::CollectorRuntime::with_local(&mut guard, &mut self.local);
        let result = f(&mut runtime);
        drop(runtime);
        self.heap
            .store_nursery_generation(guard.nursery().generation());
        result
    }

    /// Create a new rooted handle scope backed by this
    /// mutator's per-local root stack.
    pub fn handle_scope<'scope>(&mut self) -> HandleScope<'scope, 'heap> {
        let had_safepoint = self.handle_scope_state.has_safepoint();
        self.handle_scope_state.begin_scope();
        if !had_safepoint {
            self.local.publish_local_mut().clear();
        }
        HandleScope::new_with_state(
            self.local.root_stack_ptr(),
            NonNull::from(&mut self.handle_scope_state),
        )
    }

    /// Return a shared view of the underlying heap.
    pub fn heap(&self) -> &Heap {
        self.heap
    }

    fn alloc_typed_scoped<'scope, 'handle_heap, T: Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, 'handle_heap>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        let had_safepoint = self.handle_scope_state.has_safepoint();
        self.handle_scope_state.ensure_safepoint();
        if !had_safepoint {
            self.local.publish_local_mut().clear();
        }
        let _safepoint =
            (!self.handle_scope_state.has_safepoint()).then(|| self.heap.read_safepoint());
        let Self { heap, local, .. } = self;

        let snapshot = heap.allocation_snapshot::<T>(local.cached_descriptor::<T>())?;
        let config = snapshot.config;
        let desc = snapshot.desc;
        let space = snapshot.space;
        local.remember_descriptor::<T>(desc);
        let mut value = Some(value);
        let mut old_reserved_bytes = 0usize;
        let mut nursery_total_size = None;

        let record = match space {
            crate::object::SpaceKind::Nursery => {
                let (layout, payload_offset) = crate::object::allocation_layout_for::<T>()?;
                nursery_total_size = Some(layout.size());
                match local
                    .tlab
                    .as_mut()
                    .and_then(|tlab| tlab.try_alloc(snapshot.nursery_generation, layout))
                {
                    Some(base) => unsafe {
                        crate::object::ObjectRecord::allocate_in_arena::<T>(
                            desc,
                            space,
                            base,
                            layout,
                            payload_offset,
                            value.take().expect("allocation value should be present"),
                        )
                    },
                    None => {
                        let mut core = heap.write_core();
                        let base = crate::runtime::try_bump_nursery_tlab_or_refill(
                            &mut local.tlab,
                            core.nursery_mut(),
                            layout,
                            config.nursery.tlab_bytes,
                        )
                        .or_else(|| core.nursery_mut().try_alloc(layout));
                        match base {
                            Some(base) => unsafe {
                                crate::object::ObjectRecord::allocate_in_arena::<T>(
                                    desc,
                                    space,
                                    base,
                                    layout,
                                    payload_offset,
                                    value.take().expect("allocation value should be present"),
                                )
                            },
                            None => crate::object::ObjectRecord::allocate(
                                desc,
                                space,
                                value.take().expect("allocation value should be present"),
                            )?,
                        }
                    }
                }
            }
            crate::object::SpaceKind::Old => {
                let (layout, payload_offset) = crate::object::allocation_layout_for::<T>()?;
                let mut core = heap.write_core();
                match core
                    .old_gen_mut()
                    .try_alloc_in_block_with_reserved(&config.old, layout)
                {
                    Some((placement, base, reserved_bytes)) => {
                        old_reserved_bytes = reserved_bytes;
                        let mut record = unsafe {
                            crate::object::ObjectRecord::allocate_in_arena::<T>(
                                desc,
                                space,
                                base,
                                layout,
                                payload_offset,
                                value.take().expect("allocation value should be present"),
                            )
                        };
                        record.set_old_block_placement(placement);
                        record
                    }
                    None => {
                        old_reserved_bytes = core.old_gen().reserved_bytes();
                        crate::object::ObjectRecord::allocate(
                            desc,
                            space,
                            value.take().expect("allocation value should be present"),
                        )?
                    }
                }
            }
            _ => crate::object::ObjectRecord::allocate(
                desc,
                space,
                value.take().expect("allocation value should be present"),
            )?,
        };

        let (publish_local, alloc_counter_local) = local.publish_and_alloc_counter_local_mut();
        let commit = match nursery_total_size {
            Some(total_size) => heap.commit_allocated_record_shared_prepared_nursery(
                record,
                total_size,
                publish_local,
                alloc_counter_local,
            )?,
            None => heap.commit_allocated_record_shared(
                record,
                old_reserved_bytes,
                publish_local,
                alloc_counter_local,
                true,
            )?,
        };
        if commit.plans_dirty {
            heap.mark_collector_plans_dirty();
        }
        let gc = unsafe { Gc::from_erased(commit.gc) };
        Ok(scope.root(gc))
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
        self.alloc_typed_scoped(scope, value)
    }

    /// Allocate one managed object, collecting first if nursery pressure requires it.
    pub fn alloc_auto<'scope, T: Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, 'heap>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        self.with_runtime(|runtime| runtime.prepare_typed_allocation::<T>())?;
        self.alloc_typed_scoped(scope, value)
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
        self.handle_scope_state.release_safepoint();
        let _safepoint = self.heap.write_safepoint();
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
        self.handle_scope_state.release_safepoint();
        let _safepoint = self.heap.write_safepoint();
        let mut guard = self.heap.write_core();
        guard.compact_old_gen_aggressive(self.local.roots_mut(), density_threshold, max_passes)
    }

    /// Block-targeted compaction wrapper. Mirrors
    /// [`Heap::compact_old_gen_blocks`] through the mutator
    /// borrow so scoped roots created from the same mutator
    /// stay valid across the call.
    pub fn compact_old_gen_blocks(&mut self, block_indices: &[usize]) -> usize {
        self.handle_scope_state.release_safepoint();
        let _safepoint = self.heap.write_safepoint();
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
    pub fn compact_old_gen_if_fragmented(&mut self, fragmentation_threshold: f64) -> (f64, usize) {
        self.handle_scope_state.release_safepoint();
        let _safepoint = self.heap.write_safepoint();
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
    ///
    /// Runs under a mutator-held safepoint read lock plus a
    /// `HeapCore` read lock. That safepoint matters for
    /// correctness: it prevents `begin_major_mark` from
    /// starting in the middle of a pointer mutation, so the
    /// barrier can use the collector's atomic active-mark
    /// mirror as an exact "is SATB required right now?"
    /// predicate instead of taking the collector mutex on the
    /// common non-marking path.
    ///
    /// All other barrier work — stats bookkeeping,
    /// per-mutator event logging, active-major-mark assist,
    /// and remembered-set fallback — is either atomic, uses
    /// its own internal mutex, or mutates per-mutator state.
    fn post_write_barrier_slow(
        &mut self,
        owner_erased: GcErased,
        old_erased: Option<GcErased>,
        new_erased: Option<GcErased>,
        active_major_mark: bool,
        needs_remembered_edge: bool,
    ) {
        let core = self.heap.read_core();
        let collector = core.collector_handle_ref();
        if active_major_mark {
            let objects = core.objects();
            collector
                .record_active_major_post_write_and_refresh(
                    objects.raw(),
                    owner_erased,
                    old_erased,
                    new_erased,
                    core.config().old.mutator_assist_slices,
                    || core.storage_stats(),
                    core.old_gen(),
                    core.old_config(),
                    |kind| core.plan_for(kind),
                )
                .expect("post-write active major-mark assist should not fail");
        }

        if needs_remembered_edge {
            core.record_remembered_edge_if_needed(owner_erased, new_erased);
        }
    }

    fn post_write_barrier_erased(
        &mut self,
        owner_erased: GcErased,
        slot: Option<usize>,
        old_erased: Option<GcErased>,
        new_erased: Option<GcErased>,
    ) {
        self.handle_scope_state.ensure_safepoint();
        let _safepoint =
            (!self.handle_scope_state.has_safepoint()).then(|| self.heap.read_safepoint());
        assert!(
            !self.heap.prepared_full_reclaim_active(),
            "cannot mutate heap edges while prepared full reclaim is active; finish the active full collection first"
        );
        let active_major_mark = self.heap.has_active_major_mark();
        let record_satb = old_erased.is_some() && active_major_mark;

        self.heap.bump_barrier_stats(BarrierKind::PostWrite);
        self.local.push_barrier_event(
            BarrierKind::PostWrite,
            owner_erased,
            slot,
            old_erased,
            new_erased,
        );
        if record_satb {
            self.heap.bump_barrier_stats(BarrierKind::SatbPreWrite);
            self.local.push_barrier_event(
                BarrierKind::SatbPreWrite,
                owner_erased,
                slot,
                old_erased,
                new_erased,
            );
        }

        let needs_remembered_edge = new_erased.is_some_and(|target| {
            let owner_space = unsafe { owner_erased.header().as_ref().space() };
            let target_space = unsafe { target.header().as_ref().space() };
            owner_space != crate::object::SpaceKind::Nursery
                && owner_space != crate::object::SpaceKind::Immortal
                && target_space == crate::object::SpaceKind::Nursery
        });
        if !active_major_mark && !needs_remembered_edge {
            return;
        }
        self.post_write_barrier_slow(
            owner_erased,
            old_erased,
            new_erased,
            active_major_mark,
            needs_remembered_edge,
        );
    }

    /// Record a post-write barrier for one mutated GC edge.
    ///
    /// This is only exposed for callers that already mutated
    /// the edge themselves. `store_edge` is preferred because
    /// it holds the safepoint read lock across the actual
    /// pointer store and the barrier, which avoids races with
    /// `begin_major_mark`.
    pub fn post_write_barrier<Owner: ?Sized, Value: ?Sized>(
        &mut self,
        owner: Gc<Owner>,
        slot: Option<usize>,
        old_value: Option<Gc<Value>>,
        new_value: Option<Gc<Value>>,
    ) {
        self.post_write_barrier_erased(
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
        let owner_gc = owner.as_gc();
        let owner_ref = unsafe { owner_gc.as_non_null().as_ref() };
        self.handle_scope_state.ensure_safepoint();
        let _safepoint =
            (!self.handle_scope_state.has_safepoint()).then(|| self.heap.read_safepoint());
        let edge = project(owner_ref);
        let old_value = edge.replace(new_value);
        self.post_write_barrier_erased(
            owner_gc.erase(),
            Some(slot),
            old_value.map(Gc::erase),
            new_value.map(Gc::erase),
        );
    }
}

impl Drop for Mutator<'_> {
    fn drop(&mut self) {
        self.heap
            .release_allocation_counter_local(self.local.alloc_counter_local_mut());
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
