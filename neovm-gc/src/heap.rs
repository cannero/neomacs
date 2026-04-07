use crate::background::{BackgroundCollectorConfig, BackgroundService, SharedHeap};
use crate::barrier::{BarrierEvent, BarrierKind};
use crate::collector_exec::collect_global_sources;
use crate::collector_state::{CollectorSharedSnapshot, CollectorStateHandle};
use crate::descriptor::{GcErased, Trace, TypeDesc, fixed_type_desc};
use crate::index_state::HeapIndexState;
use crate::mutator::Mutator;
use crate::object::{ObjectRecord, SpaceKind};
use crate::pause_stats::{PauseHistogram, PauseStatsHandle};
use crate::plan::{
    CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress, RuntimeWorkStatus,
};
use crate::root::RootStack;
use crate::runtime::CollectorRuntime;
use crate::runtime_state::RuntimeStateHandle;
use crate::spaces::{
    LargeObjectSpaceConfig, NurseryConfig, NurseryState, OldGenConfig, OldGenPlanSelection,
    OldGenState, PinnedSpaceConfig,
};
use crate::stats::{CollectionStats, HeapStats, OldRegionStats};
use core::any::TypeId;
use core::ptr::NonNull;
use std::collections::HashMap;

/// Heap creation configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeapConfig {
    /// Nursery configuration.
    pub nursery: NurseryConfig,
    /// Old-generation configuration.
    pub old: OldGenConfig,
    /// Pinned-space configuration.
    pub pinned: PinnedSpaceConfig,
    /// Large-object-space configuration.
    pub large: LargeObjectSpaceConfig,
}

impl Default for HeapConfig {
    fn default() -> Self {
        Self {
            nursery: NurseryConfig::default(),
            old: OldGenConfig::default(),
            pinned: PinnedSpaceConfig::default(),
            large: LargeObjectSpaceConfig::default(),
        }
    }
}

/// Allocation error for the managed heap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AllocError {
    /// Object size overflowed layout computation.
    LayoutOverflow,
    /// Allocator returned null for the requested size.
    OutOfMemory {
        /// Total allocation size requested from the system allocator.
        requested_bytes: usize,
    },
    /// A persistent major collection session is already active.
    CollectionInProgress,
    /// No persistent major collection session is currently active.
    NoCollectionInProgress,
    /// The requested collection kind is not supported for this API.
    UnsupportedCollectionKind {
        /// Collection kind that could not be honored.
        kind: CollectionKind,
    },
}

/// Global heap object.
///
/// Field order matters: `ObjectRecord` storage (both the live `objects`
/// vec and the `runtime_state` pending-finalizer queue) must drop
/// BEFORE the `NurseryState` that backs their arena allocations,
/// otherwise arena-owned records would run their payload `Drop`
/// implementations against already-freed buffers.
#[derive(Debug)]
pub struct Heap {
    config: HeapConfig,
    stats: HeapStats,
    roots: RootStack,
    descriptors: HashMap<TypeId, &'static TypeDesc>,
    // --- record storage (drops first, before nursery) ---
    objects: Vec<ObjectRecord>,
    runtime_state: RuntimeStateHandle,
    // --- index and collector bookkeeping ---
    indexes: HeapIndexState,
    old_gen: OldGenState,
    recent_barrier_events: Vec<BarrierEvent>,
    collector: CollectorStateHandle,
    pause_stats: PauseStatsHandle,
    // --- arena buffers (drops last, after all records) ---
    /// Bump-pointer semispace nursery arenas (Phase 1).
    nursery: NurseryState,
}

// SAFETY: `Heap` owns all heap allocations and its raw pointers are internal references into that
// owned storage or static descriptors. Sending a `Heap` to another thread does not invalidate those
// pointers. Concurrent access is still not allowed without external synchronization, so `Heap` is
// `Send` but intentionally not `Sync`.
unsafe impl Send for Heap {}

impl Heap {
    /// Create a new heap with `config`.
    pub fn new(config: HeapConfig) -> Self {
        let nursery = NurseryState::new(config.nursery.semispace_bytes);
        let heap = Self {
            stats: HeapStats {
                nursery: crate::stats::SpaceStats {
                    reserved_bytes: config.nursery.semispace_bytes.saturating_mul(2),
                    live_bytes: 0,
                },
                old: crate::stats::SpaceStats {
                    reserved_bytes: 0,
                    live_bytes: 0,
                },
                pinned: crate::stats::SpaceStats {
                    reserved_bytes: config.pinned.reserved_bytes,
                    live_bytes: 0,
                },
                large: crate::stats::SpaceStats::default(),
                immortal: crate::stats::SpaceStats::default(),
                collections: crate::stats::CollectionStats::default(),
                remembered_edges: 0,
                remembered_owners: 0,
                finalizable_candidates: 0,
                weak_candidates: 0,
                ephemeron_candidates: 0,
                finalizers_run: 0,
                pending_finalizers: 0,
            },
            config,
            roots: RootStack::default(),
            descriptors: HashMap::default(),
            objects: Vec::new(),
            indexes: HeapIndexState::default(),
            old_gen: OldGenState::default(),
            runtime_state: RuntimeStateHandle::default(),
            recent_barrier_events: Vec::new(),
            collector: CollectorStateHandle::default(),
            pause_stats: PauseStatsHandle::new(),
            nursery,
        };
        heap.refresh_recommended_plans();
        heap
    }

    #[allow(dead_code)]
    pub(crate) fn nursery(&self) -> &NurseryState {
        &self.nursery
    }

    pub(crate) fn nursery_mut(&mut self) -> &mut NurseryState {
        &mut self.nursery
    }

    pub(crate) fn runtime_state_handle(&self) -> RuntimeStateHandle {
        self.runtime_state.clone()
    }

    pub(crate) fn collector_handle(&self) -> CollectorStateHandle {
        self.collector.clone()
    }

    pub(crate) fn global_sources(&self) -> Vec<GcErased> {
        collect_global_sources(&self.roots, &self.objects)
    }

    pub(crate) fn objects(&self) -> &[ObjectRecord] {
        &self.objects
    }

    pub(crate) fn indexes(&self) -> &HeapIndexState {
        &self.indexes
    }

    pub(crate) fn old_gen(&self) -> &OldGenState {
        &self.old_gen
    }

    pub(crate) fn old_gen_mut(&mut self) -> &mut OldGenState {
        &mut self.old_gen
    }

    pub(crate) fn old_config(&self) -> &OldGenConfig {
        &self.config.old
    }

    pub(crate) fn collection_exec_parts(
        &mut self,
    ) -> (
        &mut RootStack,
        &mut Vec<ObjectRecord>,
        &mut HeapIndexState,
        &mut OldGenState,
        &mut HeapStats,
        &OldGenConfig,
        &NurseryConfig,
        &mut NurseryState,
    ) {
        let Self {
            config,
            stats,
            roots,
            objects,
            indexes,
            old_gen,
            nursery,
            ..
        } = self;
        (
            roots,
            objects,
            indexes,
            old_gen,
            stats,
            &config.old,
            &config.nursery,
            nursery,
        )
    }

    pub(crate) fn finished_reclaim_commit_parts(
        &mut self,
    ) -> (
        &mut Vec<ObjectRecord>,
        &mut HeapIndexState,
        &mut OldGenState,
        &mut HeapStats,
    ) {
        let Self {
            objects,
            indexes,
            old_gen,
            stats,
            ..
        } = self;
        (objects, indexes, old_gen, stats)
    }

    pub(crate) fn allocation_commit_parts(
        &mut self,
    ) -> (
        &mut Vec<ObjectRecord>,
        &mut HeapIndexState,
        &mut OldGenState,
        &mut HeapStats,
        &OldGenConfig,
    ) {
        let Self {
            config,
            objects,
            indexes,
            old_gen,
            stats,
            ..
        } = self;
        (objects, indexes, old_gen, stats, &config.old)
    }

    /// Return the heap configuration.
    pub fn config(&self) -> &HeapConfig {
        &self.config
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> HeapStats {
        let mut stats = self.storage_stats();
        self.runtime_state.apply_runtime_stats(&mut stats);
        stats
    }

    pub(crate) fn storage_stats(&self) -> HeapStats {
        let mut stats = self.stats;
        self.indexes.apply_storage_stats(&mut stats);
        stats
    }

    pub(crate) fn runtime_finalizer_stats(&self) -> (u64, usize) {
        self.runtime_state.snapshot()
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> RuntimeWorkStatus {
        self.runtime_state.runtime_work_status()
    }

    pub(crate) fn collector_shared_snapshot(&self) -> CollectorSharedSnapshot {
        self.collector.shared_snapshot()
    }

    /// Build a scheduler-visible collection plan from current heap state.
    pub fn plan_for(&self, kind: CollectionKind) -> CollectionPlan {
        crate::collector_policy::build_plan(
            kind,
            &self.objects,
            &self.stats,
            &self.config.nursery,
            &self.config.old,
            &self.old_gen,
        )
    }

    /// Recommend the next collection plan from current heap pressure.
    pub fn recommended_plan(&self) -> CollectionPlan {
        self.collector.recommended_plan()
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.collector.recommended_background_plan()
    }

    pub(crate) fn refresh_recommended_plans(&self) {
        self.collector.refresh_cached_plans(
            &self.storage_stats(),
            &self.old_gen,
            &self.config.old,
            |kind| self.plan_for(kind),
        );
    }

    /// Return the phases traversed by the most recently executed collection.
    pub fn recent_phase_trace(&self) -> Vec<CollectionPhase> {
        self.collector.recent_phase_trace()
    }

    /// Return the most recently completed collection plan, if any.
    pub fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.collector.last_completed_plan()
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.collector.active_major_mark_plan()
    }

    /// Return current progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Option<MajorMarkProgress> {
        self.collector.major_mark_progress()
    }

    /// Begin a persistent major-mark session for `plan`.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        CollectorRuntime::new(self).begin_major_mark(plan)
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&mut self) -> Result<MajorMarkProgress, AllocError> {
        CollectorRuntime::new(self).advance_major_mark()
    }

    /// Finish the current persistent major-mark session and reclaim.
    pub fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        CollectorRuntime::new(self).finish_major_collection()
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &mut self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        CollectorRuntime::new(self).assist_major_mark(max_slices)
    }

    /// Advance one scheduler-style concurrent major-mark round using the plan worker count.
    pub fn poll_active_major_mark(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        CollectorRuntime::new(self).poll_active_major_mark()
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        CollectorRuntime::new(self).finish_active_major_collection_if_ready()
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        CollectorRuntime::new(self).commit_active_reclaim_if_ready()
    }

    /// Return logical old-generation region statistics.
    pub fn old_region_stats(&self) -> Vec<OldRegionStats> {
        self.old_gen.region_stats()
    }

    /// Return the currently selected old-region compaction candidates.
    pub fn major_region_candidates(&self) -> Vec<OldRegionStats> {
        let OldGenPlanSelection { candidates, .. } =
            self.old_gen.major_plan_selection(&self.config.old);
        candidates
    }

    /// Number of live objects currently tracked by the heap.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Return the number of queued finalizers waiting to run.
    pub fn pending_finalizer_count(&self) -> usize {
        self.runtime_state.pending_finalizer_count()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&self) -> u64 {
        self.runtime_state.drain_pending_finalizers()
    }

    /// Number of remembered old-to-young edges currently tracked.
    pub fn remembered_edge_count(&self) -> usize {
        self.indexes.remembered.edges.len()
    }

    #[cfg(test)]
    pub(crate) fn remembered_owner_count(&self) -> usize {
        self.indexes.remembered.owners.len()
    }

    /// Number of recent barrier events retained for diagnostics.
    pub fn barrier_event_count(&self) -> usize {
        self.recent_barrier_events.len()
    }

    /// Recorded recent barrier events retained for diagnostics.
    pub fn recent_barrier_events(&self) -> &[BarrierEvent] {
        &self.recent_barrier_events
    }

    #[cfg(test)]
    pub(crate) fn root_slot_count(&self) -> usize {
        self.roots.len()
    }

    pub(crate) fn root_stack_ptr(&mut self) -> NonNull<RootStack> {
        NonNull::from(&mut self.roots)
    }

    /// Create a mutator bound to this heap.
    pub fn mutator(&mut self) -> Mutator<'_> {
        Mutator::new(self)
    }

    /// Create a collector-side runtime bound to this heap.
    pub fn collector_runtime(&mut self) -> CollectorRuntime<'_> {
        CollectorRuntime::new(self)
    }

    /// Create a background collection service loop bound to this heap.
    pub fn background_service(
        &mut self,
        config: BackgroundCollectorConfig,
    ) -> BackgroundService<'_> {
        self.collector_runtime().background_service(config)
    }

    /// Convert this heap into a shared synchronized heap wrapper.
    pub fn into_shared(self) -> SharedHeap {
        SharedHeap::from_heap(self)
    }

    /// Run one stop-the-world collection cycle.
    pub fn collect(&mut self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        CollectorRuntime::new(self).collect(kind)
    }

    /// Execute one scheduler-provided collection plan.
    pub fn execute_plan(&mut self, plan: CollectionPlan) -> Result<CollectionStats, AllocError> {
        CollectorRuntime::new(self).execute_plan(plan)
    }

    pub(crate) fn push_barrier_event(
        &mut self,
        kind: BarrierKind,
        owner: GcErased,
        slot: Option<usize>,
        old_value: Option<GcErased>,
        new_value: Option<GcErased>,
    ) {
        const MAX_BARRIER_EVENTS: usize = 1024;

        self.recent_barrier_events.push(BarrierEvent {
            kind,
            owner: unsafe { crate::root::Gc::from_erased(owner) },
            slot,
            old_value: old_value.map(|value| unsafe { crate::root::Gc::from_erased(value) }),
            new_value: new_value.map(|value| unsafe { crate::root::Gc::from_erased(value) }),
        });
        if self.recent_barrier_events.len() > MAX_BARRIER_EVENTS {
            let overflow = self.recent_barrier_events.len() - MAX_BARRIER_EVENTS;
            self.recent_barrier_events.drain(..overflow);
        }
    }

    pub(crate) fn record_remembered_edge_if_needed(
        &mut self,
        owner: GcErased,
        new_value: Option<GcErased>,
    ) {
        self.indexes
            .record_remembered_edge_if_needed(&self.objects, owner, new_value);
    }

    pub(crate) fn prepared_full_reclaim_active(&self) -> bool {
        self.collector.has_prepared_full_reclaim()
    }

    pub(crate) fn descriptor_for<T: Trace + 'static>(&mut self) -> &'static TypeDesc {
        let type_id = TypeId::of::<T>();
        *self
            .descriptors
            .entry(type_id)
            .or_insert_with(|| Box::leak(Box::new(fixed_type_desc::<T>())))
    }

    pub(crate) fn allocation_pressure_plan(
        &self,
        space: SpaceKind,
        bytes: usize,
    ) -> Option<CollectionPlan> {
        crate::collector_policy::allocation_pressure_plan(
            &self.stats,
            &self.config.nursery,
            &self.config.pinned,
            &self.config.large,
            space,
            bytes,
            |kind| self.plan_for(kind),
        )
    }

    pub(crate) fn record_collection_stats(&mut self, cycle: CollectionStats) {
        self.stats.collections.saturating_add_assign(cycle);
        if cycle.pause_nanos > 0 {
            self.pause_stats.record(cycle.pause_nanos);
        }
    }

    /// Return a snapshot of recent stop-the-world pause statistics (P50/P95/P99
    /// of pause nanoseconds over a bounded rolling window).
    pub fn pause_histogram(&self) -> PauseHistogram {
        self.pause_stats.snapshot()
    }

    #[allow(dead_code)]
    pub(crate) fn pause_stats_handle(&self) -> PauseStatsHandle {
        self.pause_stats.clone()
    }

    #[cfg(test)]
    pub(crate) fn contains<T>(&self, gc: crate::root::Gc<T>) -> bool {
        self.indexes
            .object_index
            .contains_key(&gc.erase().object_key())
    }

    #[cfg(test)]
    pub(crate) fn finalizable_candidate_count(&self) -> usize {
        self.indexes.finalizable_candidates.len()
    }

    #[cfg(test)]
    pub(crate) fn weak_candidate_count(&self) -> usize {
        self.indexes.weak_candidates.len()
    }

    #[cfg(test)]
    pub(crate) fn ephemeron_candidate_count(&self) -> usize {
        self.indexes.ephemeron_candidates.len()
    }

    #[cfg(test)]
    pub(crate) fn space_of<T>(&self, gc: crate::root::Gc<T>) -> Option<SpaceKind> {
        self.indexes
            .object_index
            .get(&gc.erase().object_key())
            .map(|&index| self.objects[index].space())
    }
}

/// Drain pending finalizers at the controlled boundary of `Heap` drop so
/// that any arena- or old-block-backed `ObjectRecord`s sitting in
/// `RuntimeState::pending_finalizers` run their payload `drop_in_place`
/// while the backing buffers in `NurseryState` / `OldGenState` are still
/// alive.
///
/// Without this, a `SharedHeap` clone of `RuntimeStateHandle` can keep
/// the `RuntimeState` alive past `Heap`'s drop. When that Arc finally
/// hits zero, the pending `ObjectRecord`s try to deref headers in
/// arena or old-block buffers that have already been freed as part
/// of `Heap`'s field-order drop sequence.
impl Drop for Heap {
    fn drop(&mut self) {
        let _ = self.runtime_state.drain_pending_finalizers();
    }
}
