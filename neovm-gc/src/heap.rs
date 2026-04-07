use crate::background::{BackgroundCollectorConfig, BackgroundService, SharedHeap};
use crate::barrier::{BarrierEvent, BarrierKind};
use crate::collector_exec::collect_global_sources;
use crate::collector_state::{CollectorSharedSnapshot, CollectorStateHandle};
use crate::descriptor::{GcErased, Trace, TypeDesc, fixed_type_desc};
use crate::index_state::HeapIndexState;
use crate::mutator::Mutator;
use crate::object::{ObjectRecord, SpaceKind, estimated_allocation_size};
use crate::plan::{
    CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress, RuntimeWorkStatus,
};
use crate::root::RootStack;
use crate::runtime::CollectorRuntime;
use crate::runtime_state::RuntimeStateHandle;
use crate::spaces::{
    LargeObjectSpaceConfig, NurseryConfig, OldGenConfig, OldGenPlanSelection, OldGenState,
    PinnedSpaceConfig,
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
#[derive(Debug)]
pub struct Heap {
    config: HeapConfig,
    stats: HeapStats,
    roots: RootStack,
    descriptors: HashMap<TypeId, &'static TypeDesc>,
    objects: Vec<ObjectRecord>,
    indexes: HeapIndexState,
    old_gen: OldGenState,
    recent_barrier_events: Vec<BarrierEvent>,
    runtime_state: RuntimeStateHandle,
    collector: CollectorStateHandle,
}

// SAFETY: `Heap` owns all heap allocations and its raw pointers are internal references into that
// owned storage or static descriptors. Sending a `Heap` to another thread does not invalidate those
// pointers. Concurrent access is still not allowed without external synchronization, so `Heap` is
// `Send` but intentionally not `Sync`.
unsafe impl Send for Heap {}

impl Heap {
    /// Create a new heap with `config`.
    pub fn new(config: HeapConfig) -> Self {
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
            recent_barrier_events: Vec::new(),
            runtime_state: RuntimeStateHandle::default(),
            collector: CollectorStateHandle::default(),
        };
        heap.refresh_recommended_plans();
        heap
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
    ) {
        let Self {
            config,
            stats,
            roots,
            objects,
            indexes,
            old_gen,
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

    /// Return the heap configuration.
    pub fn config(&self) -> &HeapConfig {
        &self.config
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> HeapStats {
        let mut stats = self.storage_stats();
        let (finalizers_run, pending_finalizers) = self.runtime_state.snapshot();
        stats.finalizers_run = finalizers_run;
        stats.pending_finalizers = pending_finalizers;
        stats
    }

    pub(crate) fn storage_stats(&self) -> HeapStats {
        let mut stats = self.stats;
        stats.remembered_edges = self.indexes.remembered.edges.len();
        stats.remembered_owners = self.indexes.remembered.owners.len();
        stats.finalizable_candidates = self.indexes.finalizable_candidates.len();
        stats.weak_candidates = self.indexes.weak_candidates.len();
        stats.ephemeron_candidates = self.indexes.ephemeron_candidates.len();
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
        match kind {
            CollectionKind::Minor => {
                let worker_count = self.config.nursery.parallel_minor_workers.max(1);
                let mark_slice_budget = self
                    .objects
                    .iter()
                    .filter(|object| object.space() == SpaceKind::Nursery)
                    .count()
                    .max(1)
                    .div_ceil(worker_count);
                CollectionPlan {
                    kind,
                    phase: CollectionPhase::Evacuate,
                    concurrent: false,
                    parallel: true,
                    worker_count,
                    mark_slice_budget,
                    target_old_regions: 0,
                    selected_old_regions: Vec::new(),
                    estimated_compaction_bytes: 0,
                    estimated_reclaim_bytes: self.stats.nursery.live_bytes,
                }
            }
            CollectionKind::Major | CollectionKind::Full => {
                let old_selection = self.old_gen.major_plan_selection(&self.config.old);
                let selected_old_regions: Vec<_> = old_selection
                    .candidates
                    .iter()
                    .map(|region| region.region_index)
                    .collect();
                let target_old_regions = selected_old_regions.len();
                let estimated_compaction_bytes = old_selection.estimated_compaction_bytes;
                let old_reclaim_bytes = old_selection.estimated_reclaim_bytes;
                let worker_count = self.config.old.concurrent_mark_workers.max(1);
                let mark_slice_budget = self.objects.len().max(1).div_ceil(worker_count);
                let estimated_reclaim_bytes = match kind {
                    CollectionKind::Major => old_reclaim_bytes,
                    CollectionKind::Full => old_reclaim_bytes
                        .saturating_add(self.stats.nursery.live_bytes)
                        .saturating_add(self.stats.large.live_bytes),
                    CollectionKind::Minor => unreachable!(),
                };
                CollectionPlan {
                    kind,
                    phase: CollectionPhase::InitialMark,
                    concurrent: self.config.old.concurrent_mark_workers > 1,
                    parallel: true,
                    worker_count,
                    mark_slice_budget,
                    target_old_regions,
                    selected_old_regions,
                    estimated_compaction_bytes,
                    estimated_reclaim_bytes,
                }
            }
        }
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

    pub(crate) fn allocate_typed<T: Trace + 'static>(
        &mut self,
        value: T,
    ) -> Result<crate::root::Gc<T>, AllocError> {
        if self.prepared_full_reclaim_active() {
            return Err(AllocError::CollectionInProgress);
        }
        let desc = self.descriptor_for::<T>();
        let payload_bytes = core::mem::size_of::<T>();
        let space = self.select_space(desc, payload_bytes)?;
        let mut record = ObjectRecord::allocate(desc, space, value)?;
        let total_size = record.header().total_size();
        if space == SpaceKind::Old {
            let placement = self
                .old_gen
                .allocate_placement(&self.config.old, total_size);
            record.set_old_region_placement(placement);
            self.old_gen.record_object(&record);
            self.stats.old.reserved_bytes = self.old_gen.reserved_bytes();
        }
        let gc = unsafe { crate::root::Gc::from_erased(record.erased()) };
        self.account_allocation(space, total_size);
        self.objects.push(record);
        let index = self.objects.len() - 1;
        let object_key = self.objects[index].object_key();
        self.indexes.object_index.insert(object_key, index);
        let desc = self.objects[index].header().desc();
        self.indexes.record_descriptor_candidates(object_key, desc);
        Ok(gc)
    }

    pub(crate) fn typed_allocation_profile<T: Trace + 'static>(
        &mut self,
    ) -> Result<(SpaceKind, usize), AllocError> {
        let desc = self.descriptor_for::<T>();
        let payload_bytes = core::mem::size_of::<T>();
        let total_bytes = estimated_allocation_size::<T>()?;
        let space = self.select_space(desc, payload_bytes)?;
        Ok((space, total_bytes))
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
        let Some(owner_space) = self.space_for_erased(owner) else {
            return;
        };
        let Some(target) = new_value else {
            return;
        };
        let Some(target_space) = self.space_for_erased(target) else {
            return;
        };

        let owner_is_old = owner_space != SpaceKind::Nursery && owner_space != SpaceKind::Immortal;
        if owner_is_old && target_space == SpaceKind::Nursery {
            self.indexes.record_remembered_edge(owner, target);
        }
    }

    pub(crate) fn prepared_full_reclaim_active(&self) -> bool {
        self.collector.has_prepared_full_reclaim()
    }

    fn descriptor_for<T: Trace + 'static>(&mut self) -> &'static TypeDesc {
        let type_id = TypeId::of::<T>();
        *self
            .descriptors
            .entry(type_id)
            .or_insert_with(|| Box::leak(Box::new(fixed_type_desc::<T>())))
    }

    fn select_space(
        &self,
        desc: &'static TypeDesc,
        payload_bytes: usize,
    ) -> Result<SpaceKind, AllocError> {
        use crate::descriptor::MovePolicy;

        match desc.move_policy {
            MovePolicy::Pinned => Ok(SpaceKind::Pinned),
            MovePolicy::LargeObject => Ok(SpaceKind::Large),
            MovePolicy::Immortal => Ok(SpaceKind::Immortal),
            MovePolicy::Movable => {
                if payload_bytes >= self.config.large.threshold_bytes {
                    return Ok(SpaceKind::Large);
                }
                if payload_bytes > self.config.nursery.max_regular_object_bytes {
                    return Ok(SpaceKind::Old);
                }
                Ok(SpaceKind::Nursery)
            }
            MovePolicy::PromoteToPinned => {
                if payload_bytes >= self.config.large.threshold_bytes {
                    return Ok(SpaceKind::Large);
                }
                if payload_bytes > self.config.nursery.max_regular_object_bytes {
                    return Ok(SpaceKind::Pinned);
                }
                Ok(SpaceKind::Nursery)
            }
        }
    }

    fn account_allocation(&mut self, space: SpaceKind, bytes: usize) {
        match space {
            SpaceKind::Nursery => {
                self.stats.nursery.live_bytes = self.stats.nursery.live_bytes.saturating_add(bytes);
            }
            SpaceKind::Old => {
                self.stats.old.live_bytes = self.stats.old.live_bytes.saturating_add(bytes);
                self.stats.old.reserved_bytes = self.old_gen.reserved_bytes();
            }
            SpaceKind::Pinned => {
                self.stats.pinned.live_bytes = self.stats.pinned.live_bytes.saturating_add(bytes);
            }
            SpaceKind::Large => {
                self.stats.large.live_bytes = self.stats.large.live_bytes.saturating_add(bytes);
                self.stats.large.reserved_bytes =
                    self.stats.large.reserved_bytes.saturating_add(bytes);
            }
            SpaceKind::Immortal => {
                self.stats.immortal.live_bytes =
                    self.stats.immortal.live_bytes.saturating_add(bytes);
                self.stats.immortal.reserved_bytes =
                    self.stats.immortal.reserved_bytes.saturating_add(bytes);
            }
        }
    }

    pub(crate) fn allocation_pressure_plan(
        &self,
        space: SpaceKind,
        bytes: usize,
    ) -> Option<CollectionPlan> {
        match space {
            SpaceKind::Nursery
                if self.stats.nursery.live_bytes.saturating_add(bytes)
                    > self.config.nursery.semispace_bytes =>
            {
                Some(self.plan_for(CollectionKind::Minor))
            }
            SpaceKind::Pinned
                if self.stats.pinned.live_bytes.saturating_add(bytes)
                    > self.config.pinned.reserved_bytes =>
            {
                Some(self.plan_for(CollectionKind::Major))
            }
            SpaceKind::Large
                if self.stats.large.live_bytes.saturating_add(bytes)
                    > self.config.large.soft_limit_bytes =>
            {
                Some(self.plan_for(CollectionKind::Full))
            }
            SpaceKind::Old
            | SpaceKind::Pinned
            | SpaceKind::Large
            | SpaceKind::Nursery
            | SpaceKind::Immortal => None,
        }
    }

    pub(crate) fn record_collection_stats(&mut self, cycle: CollectionStats) {
        self.stats.collections.saturating_add_assign(cycle);
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

    pub(crate) fn space_for_erased(&self, object: GcErased) -> Option<SpaceKind> {
        self.indexes
            .object_index
            .get(&object.object_key())
            .map(|&index| self.objects[index].space())
    }
}
