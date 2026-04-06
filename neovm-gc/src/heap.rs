use core::any::TypeId;
use core::marker::PhantomData;
use core::ptr::NonNull;
use core::slice;
use std::collections::{HashMap, HashSet};
use std::thread;

use crate::background::{BackgroundCollectorConfig, BackgroundService, SharedHeap};
use crate::barrier::{BarrierEvent, BarrierKind, RememberedEdge};
use crate::collector_state::{CollectorState, MajorMarkState};
use crate::descriptor::{
    EphemeronVisitor, GcErased, Relocator, Trace, Tracer, TypeDesc, WeakProcessor, fixed_type_desc,
};
use crate::mark::MarkWorklist;
use crate::mutator::Mutator;
use crate::object::{
    ObjectHeader, ObjectRecord, OldRegionPlacement, SpaceKind, estimated_allocation_size,
};
use crate::plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress,
};
use crate::root::{HandleScope, Root, RootStack};
use crate::runtime::CollectorRuntime;
use crate::spaces::{LargeObjectSpaceConfig, NurseryConfig, OldGenConfig, PinnedSpaceConfig};
use crate::stats::{CollectionStats, HeapStats, OldRegionStats};

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
    old_regions: Vec<OldRegion>,
    remembered_edges: Vec<RememberedEdge>,
    recent_barrier_events: Vec<BarrierEvent>,
    collector: CollectorState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct HeapSharedSnapshot {
    pub(crate) stats: HeapStats,
    pub(crate) recommended_plan: CollectionPlan,
    pub(crate) recommended_background_plan: Option<CollectionPlan>,
    pub(crate) last_completed_plan: Option<CollectionPlan>,
    pub(crate) active_major_mark_plan: Option<CollectionPlan>,
    pub(crate) major_mark_progress: Option<MajorMarkProgress>,
}

// SAFETY: `Heap` owns all heap allocations and its raw pointers are internal references into that
// owned storage or static descriptors. Sending a `Heap` to another thread does not invalidate those
// pointers. Concurrent access is still not allowed without external synchronization, so `Heap` is
// `Send` but intentionally not `Sync`.
unsafe impl Send for Heap {}

#[derive(Debug)]
struct OldRegion {
    capacity_bytes: usize,
    used_bytes: usize,
    live_bytes: usize,
    object_count: usize,
    occupied_lines: HashSet<usize>,
}

struct EvacuationOutcome {
    forwarding: HashMap<NonNull<ObjectHeader>, GcErased>,
    promoted_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct OldRegionCollectionStats {
    compacted_regions: u64,
    reclaimed_regions: u64,
}

impl Heap {
    /// Create a new heap with `config`.
    pub fn new(config: HeapConfig) -> Self {
        let mut heap = Self {
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
            },
            config,
            roots: RootStack::default(),
            descriptors: HashMap::default(),
            objects: Vec::new(),
            old_regions: Vec::new(),
            remembered_edges: Vec::new(),
            recent_barrier_events: Vec::new(),
            collector: CollectorState::default(),
        };
        heap.refresh_recommended_plans();
        heap
    }

    /// Return the heap configuration.
    pub fn config(&self) -> &HeapConfig {
        &self.config
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> HeapStats {
        self.stats
    }

    pub(crate) fn shared_snapshot(&self) -> HeapSharedSnapshot {
        HeapSharedSnapshot {
            stats: self.stats(),
            recommended_plan: self.recommended_plan(),
            recommended_background_plan: self.recommended_background_plan(),
            last_completed_plan: self.last_completed_plan(),
            active_major_mark_plan: self.active_major_mark_plan(),
            major_mark_progress: self.major_mark_progress(),
        }
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
                let old_candidates = self.major_region_candidates();
                let selected_old_regions: Vec<_> = old_candidates
                    .iter()
                    .map(|region| region.region_index)
                    .collect();
                let target_old_regions = selected_old_regions.len();
                let estimated_compaction_bytes = old_candidates
                    .iter()
                    .map(|region| region.live_bytes)
                    .sum::<usize>();
                let old_reclaim_bytes = old_candidates
                    .iter()
                    .map(|region| region.hole_bytes)
                    .sum::<usize>();
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

    fn compute_recommended_plan(&self) -> CollectionPlan {
        if let Some(plan) = self.active_major_mark_plan() {
            return plan;
        }
        if self.stats.nursery.live_bytes > 0 {
            return self.plan_for(CollectionKind::Minor);
        }
        if self.stats.large.live_bytes > 0 {
            return self.plan_for(CollectionKind::Full);
        }
        if !self.old_regions.is_empty() || self.stats.pinned.live_bytes > 0 {
            return self.plan_for(CollectionKind::Major);
        }
        self.plan_for(CollectionKind::Minor)
    }

    fn compute_recommended_background_plan(&self) -> Option<CollectionPlan> {
        if let Some(plan) = self.active_major_mark_plan() {
            return Some(plan);
        }
        if self.config.old.concurrent_mark_workers <= 1 {
            return None;
        }
        if self.stats.large.live_bytes > 0 {
            return Some(self.plan_for(CollectionKind::Full));
        }
        if !self.old_regions.is_empty() || self.stats.pinned.live_bytes > 0 {
            return Some(self.plan_for(CollectionKind::Major));
        }
        None
    }

    fn refresh_recommended_plans(&mut self) {
        self.collector.set_cached_plans(
            self.compute_recommended_plan(),
            self.compute_recommended_background_plan(),
        );
    }

    /// Return the phases traversed by the most recently executed collection.
    pub fn recent_phase_trace(&self) -> &[CollectionPhase] {
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
        if self.collector.has_active_major_mark() {
            return Err(AllocError::CollectionInProgress);
        }
        if !matches!(plan.kind, CollectionKind::Major | CollectionKind::Full) {
            return Err(AllocError::UnsupportedCollectionKind { kind: plan.kind });
        }

        self.collector.clear_recent_phase_trace();
        for object in &self.objects {
            object.clear_mark();
        }

        self.record_phase(CollectionPhase::InitialMark);
        if plan.concurrent {
            self.record_phase(CollectionPhase::ConcurrentMark);
        }

        let index = self.object_index();
        let mut tracer = MarkTracer::new(&self.objects, &index);
        self.for_each_global_source(|object| tracer.mark_erased(object));

        self.collector.begin_major_mark(MajorMarkState {
            plan,
            worklist: tracer.into_worklist(),
            mark_steps: 0,
            mark_rounds: 0,
        });
        self.refresh_recommended_plans();
        Ok(())
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&mut self) -> Result<MajorMarkProgress, AllocError> {
        let Some(mut state) = self.collector.take_major_mark_state() else {
            return Err(AllocError::NoCollectionInProgress);
        };

        let index = self.object_index();
        let mut tracer = MarkTracer::with_worklist(&self.objects, &index, state.worklist);
        let drained_objects = tracer.drain_one_slice(state.plan.mark_slice_budget);
        if drained_objects > 0 {
            state.mark_steps = state.mark_steps.saturating_add(1);
            state.mark_rounds = state.mark_rounds.saturating_add(1);
        }
        let remaining_work = tracer.pending_count();
        let completed = remaining_work == 0;
        state.worklist = tracer.into_worklist();
        let mark_steps = state.mark_steps;
        let mark_rounds = state.mark_rounds;
        self.collector.restore_major_mark_state(state);
        self.refresh_recommended_plans();

        Ok(MajorMarkProgress {
            completed,
            drained_objects,
            mark_steps,
            mark_rounds,
            remaining_work,
        })
    }

    /// Finish the current persistent major-mark session and reclaim.
    pub fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        let Some(mut state) = self.collector.take_major_mark_state() else {
            return Err(AllocError::NoCollectionInProgress);
        };

        let before_bytes = self.total_tracked_bytes();
        self.record_phase(CollectionPhase::Remark);
        let index = self.object_index();
        let mut tracer = MarkTracer::with_worklist(&self.objects, &index, state.worklist);
        let (mark_steps, mark_rounds) = tracer.drain_parallel_until_empty(
            state.plan.worker_count.max(1),
            state.plan.mark_slice_budget,
        );
        state.mark_steps = state.mark_steps.saturating_add(mark_steps);
        state.mark_rounds = state.mark_rounds.saturating_add(mark_rounds);
        let (ephemeron_steps, ephemeron_rounds) = self.trace_major_ephemerons(
            &mut tracer,
            state.plan.worker_count.max(1),
            state.plan.mark_slice_budget,
        );
        state.mark_steps = state.mark_steps.saturating_add(ephemeron_steps);
        state.mark_rounds = state.mark_rounds.saturating_add(ephemeron_rounds);

        let (forwarding, promoted_bytes) = match state.plan.kind {
            CollectionKind::Major => (HashMap::new(), 0usize),
            CollectionKind::Full => {
                self.record_phase(CollectionPhase::Evacuate);
                let evacuation = self.evacuate_marked_nursery()?;
                self.relocate_roots_and_edges(&evacuation.forwarding);
                (evacuation.forwarding, evacuation.promoted_bytes)
            }
            CollectionKind::Minor => {
                return Err(AllocError::UnsupportedCollectionKind {
                    kind: state.plan.kind,
                });
            }
        };
        let index = self.object_index();
        self.process_weak_references(
            state.plan.kind,
            state.plan.worker_count.max(1),
            &forwarding,
            &index,
        );
        self.record_phase(CollectionPhase::Reclaim);
        let finalized_objects = self.sweep_full();
        self.prune_remembered_edges();
        let old_region_stats = self.recompute_live_bytes(Some(state.plan.clone()));
        let after_bytes = self.total_tracked_bytes();
        let cycle = CollectionStats {
            collections: 1,
            minor_collections: 0,
            major_collections: 1,
            promoted_bytes: promoted_bytes as u64,
            mark_steps: state.mark_steps,
            mark_rounds: state.mark_rounds,
            reclaimed_bytes: before_bytes.saturating_sub(after_bytes) as u64,
            finalized_objects,
            compacted_regions: old_region_stats.compacted_regions,
            reclaimed_regions: old_region_stats.reclaimed_regions,
        };
        self.record_collection_stats(cycle);
        self.collector.set_last_completed_plan(Some(CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..state.plan
        }));
        self.refresh_recommended_plans();
        Ok(cycle)
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &mut self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        if !self.collector.has_active_major_mark() {
            return Ok(None);
        }
        if max_slices == 0 {
            return Ok(self.major_mark_progress());
        }

        let mut total_drained_objects = 0usize;
        let mut final_progress = None;
        for _ in 0..max_slices {
            let progress = self.advance_major_mark()?;
            total_drained_objects = total_drained_objects.saturating_add(progress.drained_objects);
            let completed = progress.completed;
            final_progress = Some(progress);
            if completed {
                break;
            }
        }
        Ok(final_progress.map(|progress| MajorMarkProgress {
            completed: progress.completed,
            drained_objects: total_drained_objects,
            mark_steps: progress.mark_steps,
            mark_rounds: progress.mark_rounds,
            remaining_work: progress.remaining_work,
        }))
    }

    /// Advance one scheduler-style concurrent major-mark round using the plan worker count.
    pub fn poll_active_major_mark(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        let Some(mut state) = self.collector.take_major_mark_state() else {
            return Ok(None);
        };

        let index = self.object_index();
        let mut tracer = MarkTracer::with_worklist(&self.objects, &index, state.worklist);
        let (drained_objects, drained_slices) =
            tracer.drain_worker_round(state.plan.worker_count.max(1), state.plan.mark_slice_budget);
        state.mark_steps = state.mark_steps.saturating_add(drained_slices);
        if drained_objects > 0 {
            state.mark_rounds = state.mark_rounds.saturating_add(1);
        }
        let remaining_work = tracer.pending_count();
        let completed = remaining_work == 0;
        state.worklist = tracer.into_worklist();
        let mark_steps = state.mark_steps;
        let mark_rounds = state.mark_rounds;
        self.collector.restore_major_mark_state(state);
        self.refresh_recommended_plans();

        Ok(Some(MajorMarkProgress {
            completed,
            drained_objects,
            mark_steps,
            mark_rounds,
            remaining_work,
        }))
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        let Some(state) = self.collector.active_major_mark_state() else {
            return Ok(None);
        };
        if !state.worklist.is_empty() {
            return Ok(None);
        }
        self.finish_major_collection().map(Some)
    }

    /// Service one background collection round for the active major-mark session.
    pub fn service_background_collection_round(
        &mut self,
    ) -> Result<BackgroundCollectionStatus, AllocError> {
        if !self.collector.has_active_major_mark() {
            return Ok(BackgroundCollectionStatus::Idle);
        }

        let progress = self
            .poll_active_major_mark()?
            .expect("active major-mark session disappeared during service");
        if progress.completed {
            let cycle = self
                .finish_active_major_collection_if_ready()?
                .expect("completed session should be ready to finish");
            Ok(BackgroundCollectionStatus::Finished(cycle))
        } else {
            Ok(BackgroundCollectionStatus::Progress(progress))
        }
    }

    /// Return logical old-generation region statistics.
    pub fn old_region_stats(&self) -> Vec<OldRegionStats> {
        self.region_stats_from_metadata(&self.layout_regions_with_live_objects())
    }

    /// Return the currently selected old-region compaction candidates.
    pub fn major_region_candidates(&self) -> Vec<OldRegionStats> {
        let mut candidates: Vec<_> = self
            .old_region_stats()
            .into_iter()
            .filter(|region| {
                region.object_count > 0
                    && region.hole_bytes > 0
                    && region.hole_bytes >= self.config.old.selective_reclaim_threshold_bytes
            })
            .collect();
        candidates.sort_by(compare_compaction_candidate_priority);
        let max_regions = self.config.old.compaction_candidate_limit;
        let max_bytes = self.config.old.max_compaction_bytes_per_cycle;
        let mut selected = Vec::new();
        let mut selected_bytes = 0usize;
        for candidate in candidates {
            if selected.len() >= max_regions {
                break;
            }
            if selected_bytes.saturating_add(candidate.live_bytes) > max_bytes {
                continue;
            }
            selected_bytes = selected_bytes.saturating_add(candidate.live_bytes);
            selected.push(candidate);
        }
        selected
    }

    /// Number of live objects currently tracked by the heap.
    pub fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Number of remembered old-to-young edges currently tracked.
    pub fn remembered_edge_count(&self) -> usize {
        self.remembered_edges.len()
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
        BackgroundService::new(self, config)
    }

    /// Convert this heap into a shared synchronized heap wrapper.
    pub fn into_shared(self) -> SharedHeap {
        SharedHeap::from_heap(self)
    }

    /// Run one stop-the-world collection cycle.
    pub fn collect(&mut self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        if self.collector.has_active_major_mark() {
            return Err(AllocError::CollectionInProgress);
        }
        self.execute_plan(self.plan_for(kind))
    }

    /// Execute one scheduler-provided collection plan.
    pub fn execute_plan(&mut self, plan: CollectionPlan) -> Result<CollectionStats, AllocError> {
        if self.collector.has_active_major_mark() {
            return Err(AllocError::CollectionInProgress);
        }
        self.collector.clear_recent_phase_trace();
        let before_bytes = self.total_tracked_bytes();
        for object in &self.objects {
            object.clear_mark();
        }

        let index = self.object_index();
        let (mark_steps, mark_rounds) = match plan.kind {
            CollectionKind::Minor => {
                self.trace_minor(&index, plan.worker_count.max(1), plan.mark_slice_budget)
            }
            CollectionKind::Major | CollectionKind::Full => {
                self.record_phase(CollectionPhase::InitialMark);
                if plan.concurrent {
                    self.record_phase(CollectionPhase::ConcurrentMark);
                }
                self.record_phase(CollectionPhase::Remark);
                self.trace_major(&index, plan.worker_count.max(1), plan.mark_slice_budget)
            }
        };

        let cycle = match plan.kind {
            CollectionKind::Minor => {
                self.record_phase(CollectionPhase::Evacuate);
                let evacuation = self.evacuate_marked_nursery()?;
                self.relocate_roots_and_edges(&evacuation.forwarding);
                self.process_weak_references(
                    plan.kind,
                    plan.worker_count.max(1),
                    &evacuation.forwarding,
                    &index,
                );
                self.record_phase(CollectionPhase::Reclaim);
                let finalized_objects = self.sweep_minor();
                self.prune_remembered_edges();
                let old_region_stats = self.recompute_live_bytes(Some(plan.clone()));
                let after_bytes = self.total_tracked_bytes();
                CollectionStats {
                    collections: 1,
                    minor_collections: 1,
                    major_collections: 0,
                    promoted_bytes: evacuation.promoted_bytes as u64,
                    mark_steps,
                    mark_rounds,
                    reclaimed_bytes: before_bytes.saturating_sub(after_bytes) as u64,
                    finalized_objects,
                    compacted_regions: old_region_stats.compacted_regions,
                    reclaimed_regions: old_region_stats.reclaimed_regions,
                }
            }
            CollectionKind::Major => {
                let empty_forwarding: HashMap<NonNull<ObjectHeader>, GcErased> = HashMap::new();
                self.process_weak_references(
                    plan.kind,
                    plan.worker_count.max(1),
                    &empty_forwarding,
                    &index,
                );
                self.record_phase(CollectionPhase::Reclaim);
                let finalized_objects = self.sweep_full();
                self.prune_remembered_edges();
                let old_region_stats = self.recompute_live_bytes(Some(plan.clone()));
                let after_bytes = self.total_tracked_bytes();
                CollectionStats {
                    collections: 1,
                    minor_collections: 0,
                    major_collections: 1,
                    promoted_bytes: 0,
                    mark_steps,
                    mark_rounds,
                    reclaimed_bytes: before_bytes.saturating_sub(after_bytes) as u64,
                    finalized_objects,
                    compacted_regions: old_region_stats.compacted_regions,
                    reclaimed_regions: old_region_stats.reclaimed_regions,
                }
            }
            CollectionKind::Full => {
                self.record_phase(CollectionPhase::Evacuate);
                let evacuation = self.evacuate_marked_nursery()?;
                self.relocate_roots_and_edges(&evacuation.forwarding);
                self.process_weak_references(
                    plan.kind,
                    plan.worker_count.max(1),
                    &evacuation.forwarding,
                    &index,
                );
                self.record_phase(CollectionPhase::Reclaim);
                let finalized_objects = self.sweep_full();
                self.prune_remembered_edges();
                let old_region_stats = self.recompute_live_bytes(Some(plan.clone()));
                let after_bytes = self.total_tracked_bytes();
                CollectionStats {
                    collections: 1,
                    minor_collections: 0,
                    major_collections: 1,
                    promoted_bytes: evacuation.promoted_bytes as u64,
                    mark_steps,
                    mark_rounds,
                    reclaimed_bytes: before_bytes.saturating_sub(after_bytes) as u64,
                    finalized_objects,
                    compacted_regions: old_region_stats.compacted_regions,
                    reclaimed_regions: old_region_stats.reclaimed_regions,
                }
            }
        };
        self.record_collection_stats(cycle);
        self.collector.set_last_completed_plan(Some(CollectionPlan {
            phase: CollectionPhase::Reclaim,
            ..plan
        }));
        self.refresh_recommended_plans();
        Ok(cycle)
    }

    pub(crate) fn alloc_typed<'scope, T: Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, '_>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        let desc = self.descriptor_for::<T>();
        let payload_bytes = core::mem::size_of::<T>();
        let space = self.select_space(desc, payload_bytes)?;
        let mut record = ObjectRecord::allocate(desc, space, value)?;
        let total_size = record.header().total_size();
        if space == SpaceKind::Old {
            let placement = self.allocate_old_region_placement(total_size);
            record.set_old_region_placement(placement);
            self.record_old_region_object(&record);
        }
        let gc = unsafe { crate::root::Gc::from_erased(record.erased()) };
        self.account_allocation(space, total_size);
        self.objects.push(record);
        if self.collector.has_active_major_mark() {
            self.mark_for_active_major_session(gc.erase());
            self.assist_major_mark_in_place();
        }
        self.refresh_recommended_plans();
        Ok(scope.root(gc))
    }

    pub(crate) fn alloc_typed_auto<'scope, T: Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, '_>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        let desc = self.descriptor_for::<T>();
        let payload_bytes = core::mem::size_of::<T>();
        let total_bytes = estimated_allocation_size::<T>()?;
        let space = self.select_space(desc, payload_bytes)?;
        if !self.collector.has_active_major_mark()
            && let Some(plan) = self.allocation_pressure_plan(space, total_bytes)
        {
            if plan.concurrent && matches!(plan.kind, CollectionKind::Major | CollectionKind::Full)
            {
                self.begin_major_mark(plan)?;
            } else {
                self.execute_plan(plan)?;
            }
        }
        self.alloc_typed(scope, value)
    }

    pub(crate) fn record_post_write(
        &mut self,
        owner: GcErased,
        slot: Option<usize>,
        old_value: Option<GcErased>,
        new_value: Option<GcErased>,
    ) {
        const MAX_BARRIER_EVENTS: usize = 1024;

        let mut push_event = |kind: BarrierKind| {
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
        };

        push_event(BarrierKind::PostWrite);

        if self.collector.has_active_major_mark() {
            if let Some(value) = old_value {
                push_event(BarrierKind::SatbPreWrite);
                self.mark_for_active_major_session(value);
            }
            if self.is_marked_erased(owner)
                && let Some(value) = new_value
            {
                self.mark_for_active_major_session(value);
            }
        }

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
            self.remembered_edges.push(RememberedEdge {
                owner: unsafe { crate::root::Gc::from_erased(owner) },
                target: unsafe { crate::root::Gc::from_erased(target) },
            });
        }

        self.assist_major_mark_in_place();
        if self.collector.has_active_major_mark() {
            self.refresh_recommended_plans();
        }
    }

    pub(crate) fn root_during_active_major_mark(&mut self, object: GcErased) {
        self.mark_for_active_major_session(object);
        self.assist_major_mark_in_place();
        self.refresh_recommended_plans();
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
                self.stats.old.reserved_bytes = self
                    .old_regions
                    .iter()
                    .map(|region| region.capacity_bytes)
                    .sum();
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

    fn allocation_pressure_plan(&self, space: SpaceKind, bytes: usize) -> Option<CollectionPlan> {
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

    fn is_marked_erased(&self, object: GcErased) -> bool {
        let Some(space) = self.space_for_erased(object) else {
            return false;
        };
        if space == SpaceKind::Immortal {
            return true;
        }
        self.objects
            .iter()
            .find(|record| record.header_ptr() == object.header())
            .is_some_and(ObjectRecord::is_marked)
    }

    fn mark_for_active_major_session(&mut self, object: GcErased) {
        if self.space_for_erased(object).is_none() {
            return;
        }

        let Some(index) = self
            .objects
            .iter()
            .position(|record| record.header_ptr() == object.header())
        else {
            return;
        };

        let record = &self.objects[index];
        if record.mark_if_unmarked() {
            if let Some(state) = self.collector.active_major_mark_state_mut() {
                state.worklist.push(index);
            }
        }
    }

    fn assist_major_mark_in_place(&mut self) {
        let assist_slices = self.config.old.mutator_assist_slices;
        if assist_slices == 0 || !self.collector.has_active_major_mark() {
            return;
        }
        let _progress = self
            .assist_major_mark(assist_slices)
            .expect("mutator assist on active major-mark session should not fail");
    }

    fn object_index(&self) -> HashMap<NonNull<ObjectHeader>, usize> {
        self.objects
            .iter()
            .enumerate()
            .map(|(index, object)| (object.header_ptr(), index))
            .collect()
    }

    fn for_each_global_source(&self, mut f: impl FnMut(GcErased)) {
        for root in self.roots.iter() {
            f(root);
        }
        for object in &self.objects {
            if object.space() == SpaceKind::Immortal {
                f(object.erased());
            }
        }
    }

    fn trace_major(
        &self,
        index: &HashMap<NonNull<ObjectHeader>, usize>,
        worker_count: usize,
        slice_budget: usize,
    ) -> (u64, u64) {
        let mut session = MajorMarkSession::new(&self.objects, index, worker_count, slice_budget);
        self.for_each_global_source(|object| session.seed(object));
        session.drain_parallel();
        session.run_ephemeron_fixpoint_parallel();
        (session.mark_steps(), session.mark_rounds())
    }

    fn trace_major_ephemerons(
        &self,
        tracer: &mut MarkTracer<'_>,
        worker_count: usize,
        slice_budget: usize,
    ) -> (u64, u64) {
        let mut mark_steps = 0u64;
        let mut mark_rounds = 0u64;
        loop {
            let mut visitor = MajorEphemeronTracer::new(tracer);
            for object in &self.objects {
                if object.is_marked() {
                    object.visit_ephemerons(&mut visitor);
                }
            }
            let changed = visitor.changed;
            let tracer = visitor.finish();
            let (steps, rounds) =
                tracer.drain_parallel_until_empty(worker_count.max(1), slice_budget);
            mark_steps = mark_steps.saturating_add(steps);
            mark_rounds = mark_rounds.saturating_add(rounds);
            if !changed {
                break;
            }
        }
        (mark_steps, mark_rounds)
    }

    fn trace_minor(
        &self,
        index: &HashMap<NonNull<ObjectHeader>, usize>,
        worker_count: usize,
        slice_budget: usize,
    ) -> (u64, u64) {
        let mut tracer = MinorTracer::new(&self.objects, index);
        self.for_each_global_source(|object| tracer.scan_source(object));

        let mut scanned_owners = HashSet::new();
        for edge in &self.remembered_edges {
            let owner = edge.owner.erase().header();
            if scanned_owners.insert(owner) {
                tracer.scan_source(edge.owner.erase());
            }
        }
        let (mut mark_steps, mut mark_rounds) =
            tracer.drain_parallel_until_empty(worker_count, slice_budget);
        let (ephemeron_steps, ephemeron_rounds) =
            self.trace_minor_ephemerons(&mut tracer, worker_count, slice_budget);
        mark_steps = mark_steps.saturating_add(ephemeron_steps);
        mark_rounds = mark_rounds.saturating_add(ephemeron_rounds);
        (mark_steps, mark_rounds)
    }

    fn trace_minor_ephemerons(
        &self,
        tracer: &mut MinorTracer<'_>,
        worker_count: usize,
        slice_budget: usize,
    ) -> (u64, u64) {
        let mut mark_steps = 0u64;
        let mut mark_rounds = 0u64;
        loop {
            let changed = if worker_count.max(1) == 1 || self.objects.len() <= 1 {
                let mut visitor = MinorEphemeronTracer::new(tracer);
                for object in &self.objects {
                    let survives = object.space() != SpaceKind::Nursery || object.is_marked();
                    if survives {
                        object.visit_ephemerons(&mut visitor);
                    }
                }
                let changed = visitor.changed;
                let _tracer = visitor.finish();
                changed
            } else {
                self.scan_minor_ephemerons_parallel(tracer, worker_count)
            };
            let (steps, rounds) = tracer.drain_parallel_until_empty(worker_count, slice_budget);
            mark_steps = mark_steps.saturating_add(steps);
            mark_rounds = mark_rounds.saturating_add(rounds);
            if !changed {
                break;
            }
        }
        (mark_steps, mark_rounds)
    }

    fn scan_minor_ephemerons_parallel(
        &self,
        tracer: &mut MinorTracer<'_>,
        worker_count: usize,
    ) -> bool {
        let workers = worker_count.max(1).min(self.objects.len().max(1));
        let chunk_size = self.objects.len().max(1).div_ceil(workers);
        let shared = ParallelMarkShared::new(&self.objects, tracer.index);
        let worker_outputs = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for worker_index in 0..workers {
                let shared = shared;
                let start = worker_index.saturating_mul(chunk_size);
                let end = (start + chunk_size).min(self.objects.len());
                if start >= end {
                    continue;
                }
                handles.push(scope.spawn(move || {
                    let mut worker = shared.minor_tracer(MarkWorklist::default());
                    let changed = {
                        let mut visitor = MinorEphemeronTracer::new(&mut worker);
                        for object in &shared.objects()[start..end] {
                            let survives =
                                object.space() != SpaceKind::Nursery || object.is_marked();
                            if survives {
                                object.visit_ephemerons(&mut visitor);
                            }
                        }
                        visitor.changed
                    };
                    (changed, worker.into_worklist())
                }));
            }

            let mut outputs = Vec::with_capacity(handles.len());
            for handle in handles {
                outputs.push(
                    handle
                        .join()
                        .expect("parallel minor ephemeron worker panicked"),
                );
            }
            outputs
        });

        let mut changed = false;
        for (worker_changed, mut worklist) in worker_outputs {
            changed |= worker_changed;
            tracer.young_worklist.append(&mut worklist);
        }
        changed
    }

    fn evacuate_marked_nursery(&mut self) -> Result<EvacuationOutcome, AllocError> {
        let mut forwarding = HashMap::new();
        let mut evacuated: Vec<(ObjectRecord, SpaceKind)> = Vec::new();
        let mut promoted_bytes = 0usize;

        for object in &self.objects {
            if object.space() == SpaceKind::Nursery && object.is_marked() {
                let next_age = object.header().age().saturating_add(1);
                let target_space = if next_age >= self.config.nursery.promotion_age {
                    match object.header().desc().move_policy {
                        crate::descriptor::MovePolicy::PromoteToPinned => SpaceKind::Pinned,
                        _ => SpaceKind::Old,
                    }
                } else {
                    SpaceKind::Nursery
                };
                let new_record = object.evacuate_to_space(target_space)?;
                new_record.set_marked(true);
                forwarding.insert(object.header_ptr(), new_record.erased());
                evacuated.push((new_record, target_space));
            }
        }

        let mut records = Vec::with_capacity(evacuated.len());
        for (mut new_record, target_space) in evacuated {
            if target_space == SpaceKind::Old {
                let placement = self.allocate_old_region_placement(new_record.total_size());
                new_record.set_old_region_placement(placement);
                self.record_old_region_object(&new_record);
                promoted_bytes = promoted_bytes.saturating_add(new_record.total_size());
            }
            records.push(new_record);
        }

        self.objects.extend(records);
        Ok(EvacuationOutcome {
            forwarding,
            promoted_bytes,
        })
    }

    fn relocate_roots_and_edges(&mut self, forwarding: &HashMap<NonNull<ObjectHeader>, GcErased>) {
        if forwarding.is_empty() {
            return;
        }

        let mut relocator = ForwardingRelocator::new(forwarding);
        self.roots.relocate_all(&mut relocator);

        for object in &self.objects {
            let copied_nursery_survivor = object.space() == SpaceKind::Nursery
                && object.is_marked()
                && !object.header().is_moved_out();
            if object.space() != SpaceKind::Nursery || copied_nursery_survivor {
                object.relocate_edges(&mut relocator);
            }
        }

        for edge in &mut self.remembered_edges {
            edge.owner = unsafe {
                crate::root::Gc::from_erased(relocator.relocate_erased(edge.owner.erase()))
            };
            edge.target = unsafe {
                crate::root::Gc::from_erased(relocator.relocate_erased(edge.target.erase()))
            };
        }
    }

    fn sweep_minor(&mut self) -> u64 {
        let mut finalized_objects = 0u64;
        self.objects.retain(|object| {
            let keep = object.space() == SpaceKind::Immortal
                || object.space() != SpaceKind::Nursery
                || (object.is_marked() && !object.header().is_moved_out());
            if !keep && object.run_finalizer() {
                finalized_objects = finalized_objects.saturating_add(1);
            }
            keep
        });
        for object in &self.objects {
            object.clear_mark();
        }
        finalized_objects
    }

    fn sweep_full(&mut self) -> u64 {
        let mut finalized_objects = 0u64;
        self.objects.retain(|object| {
            let keep = object.space() == SpaceKind::Immortal
                || (object.is_marked() && !object.header().is_moved_out());
            if !keep && object.run_finalizer() {
                finalized_objects = finalized_objects.saturating_add(1);
            }
            keep
        });
        for object in &self.objects {
            object.clear_mark();
        }
        finalized_objects
    }

    fn recompute_live_bytes(
        &mut self,
        completed_plan: Option<CollectionPlan>,
    ) -> OldRegionCollectionStats {
        self.stats.nursery.live_bytes = 0;
        self.stats.old.live_bytes = 0;
        self.stats.pinned.live_bytes = 0;
        self.stats.large.live_bytes = 0;
        self.stats.large.reserved_bytes = 0;
        self.stats.immortal.live_bytes = 0;
        self.stats.immortal.reserved_bytes = 0;
        let old_region_stats = self.recompute_old_region_metadata_for_plan(completed_plan);

        for object in &self.objects {
            match object.space() {
                SpaceKind::Nursery => {
                    self.stats.nursery.live_bytes = self
                        .stats
                        .nursery
                        .live_bytes
                        .saturating_add(object.total_size());
                }
                SpaceKind::Old => {
                    self.stats.old.live_bytes = self
                        .stats
                        .old
                        .live_bytes
                        .saturating_add(object.total_size());
                }
                SpaceKind::Pinned => {
                    self.stats.pinned.live_bytes = self
                        .stats
                        .pinned
                        .live_bytes
                        .saturating_add(object.total_size());
                }
                SpaceKind::Large => {
                    self.stats.large.live_bytes = self
                        .stats
                        .large
                        .live_bytes
                        .saturating_add(object.total_size());
                    self.stats.large.reserved_bytes = self
                        .stats
                        .large
                        .reserved_bytes
                        .saturating_add(object.total_size());
                }
                SpaceKind::Immortal => {
                    self.stats.immortal.live_bytes = self
                        .stats
                        .immortal
                        .live_bytes
                        .saturating_add(object.total_size());
                    self.stats.immortal.reserved_bytes = self
                        .stats
                        .immortal
                        .reserved_bytes
                        .saturating_add(object.total_size());
                }
            }
        }
        self.stats.old.reserved_bytes = self
            .old_regions
            .iter()
            .map(|region| region.capacity_bytes)
            .sum();
        old_region_stats
    }

    fn record_collection_stats(&mut self, cycle: CollectionStats) {
        self.stats.collections.collections = self
            .stats
            .collections
            .collections
            .saturating_add(cycle.collections);
        self.stats.collections.minor_collections = self
            .stats
            .collections
            .minor_collections
            .saturating_add(cycle.minor_collections);
        self.stats.collections.major_collections = self
            .stats
            .collections
            .major_collections
            .saturating_add(cycle.major_collections);
        self.stats.collections.promoted_bytes = self
            .stats
            .collections
            .promoted_bytes
            .saturating_add(cycle.promoted_bytes);
        self.stats.collections.mark_steps = self
            .stats
            .collections
            .mark_steps
            .saturating_add(cycle.mark_steps);
        self.stats.collections.mark_rounds = self
            .stats
            .collections
            .mark_rounds
            .saturating_add(cycle.mark_rounds);
        self.stats.collections.reclaimed_bytes = self
            .stats
            .collections
            .reclaimed_bytes
            .saturating_add(cycle.reclaimed_bytes);
        self.stats.collections.finalized_objects = self
            .stats
            .collections
            .finalized_objects
            .saturating_add(cycle.finalized_objects);
        self.stats.collections.compacted_regions = self
            .stats
            .collections
            .compacted_regions
            .saturating_add(cycle.compacted_regions);
        self.stats.collections.reclaimed_regions = self
            .stats
            .collections
            .reclaimed_regions
            .saturating_add(cycle.reclaimed_regions);
    }

    fn record_phase(&mut self, phase: CollectionPhase) {
        self.collector.push_phase(phase);
    }

    fn total_tracked_bytes(&self) -> usize {
        self.objects.iter().map(ObjectRecord::total_size).sum()
    }

    fn process_weak_references(
        &self,
        kind: CollectionKind,
        worker_count: usize,
        forwarding: &HashMap<NonNull<ObjectHeader>, GcErased>,
        index: &HashMap<NonNull<ObjectHeader>, usize>,
    ) {
        let worker_count = worker_count.max(1);
        if worker_count == 1 || self.objects.len() <= 1 {
            let mut processor = WeakRetention::new(&self.objects, index, forwarding, kind);
            for object in &self.objects {
                if Self::survives_collection_kind(kind, object) {
                    object.process_weak_edges(&mut processor);
                }
            }
            return;
        }

        let workers = worker_count.min(self.objects.len().max(1));
        let chunk_size = self.objects.len().max(1).div_ceil(workers);
        let shared = ParallelWeakShared::new(&self.objects, index, forwarding, kind);
        thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for worker_index in 0..workers {
                let shared = shared;
                let start = worker_index.saturating_mul(chunk_size);
                let end = (start + chunk_size).min(self.objects.len());
                if start >= end {
                    continue;
                }
                handles.push(scope.spawn(move || {
                    let mut processor = shared.processor();
                    for object in &shared.objects()[start..end] {
                        if Heap::survives_collection_kind(kind, object) {
                            object.process_weak_edges(&mut processor);
                        }
                    }
                }));
            }
            for handle in handles {
                handle.join().expect("parallel weak worker panicked");
            }
        });
    }

    fn survives_collection_kind(kind: CollectionKind, object: &ObjectRecord) -> bool {
        if object.space() == SpaceKind::Immortal {
            return true;
        }
        match kind {
            CollectionKind::Minor => object.space() != SpaceKind::Nursery || object.is_marked(),
            CollectionKind::Major | CollectionKind::Full => object.is_marked(),
        }
    }

    fn prune_remembered_edges(&mut self) {
        let live_headers: HashSet<_> = self.objects.iter().map(ObjectRecord::header_ptr).collect();
        let header_spaces: HashMap<_, _> = self
            .objects
            .iter()
            .map(|object| (object.header_ptr(), object.space()))
            .collect();
        self.remembered_edges.retain(|edge| {
            let owner = edge.owner.erase().header();
            let target = edge.target.erase().header();
            live_headers.contains(&owner)
                && live_headers.contains(&target)
                && header_spaces.get(&owner).copied().is_some_and(|space| {
                    space != SpaceKind::Nursery && space != SpaceKind::Immortal
                })
                && header_spaces.get(&target).copied() == Some(SpaceKind::Nursery)
        });
    }

    #[cfg(test)]
    pub(crate) fn contains<T>(&self, gc: crate::root::Gc<T>) -> bool {
        let header = gc.erase().header();
        self.objects
            .iter()
            .any(|object| object.header_ptr() == header)
    }

    #[cfg(test)]
    pub(crate) fn space_of<T>(&self, gc: crate::root::Gc<T>) -> Option<SpaceKind> {
        let header = gc.erase().header();
        self.objects
            .iter()
            .find(|object| object.header_ptr() == header)
            .map(ObjectRecord::space)
    }

    fn space_for_erased(&self, object: GcErased) -> Option<SpaceKind> {
        let header = object.header();
        self.objects
            .iter()
            .find(|record| record.header_ptr() == header)
            .map(ObjectRecord::space)
    }

    fn allocate_old_region_placement(&mut self, bytes: usize) -> OldRegionPlacement {
        let align = self.config.old.line_bytes.max(8);
        if let Some((region_index, offset)) = self.try_reserve_in_existing_region(bytes, align) {
            return self.make_old_region_placement(region_index, offset, bytes);
        }

        let capacity_bytes = self.config.old.region_bytes.max(bytes);
        self.old_regions.push(OldRegion {
            capacity_bytes,
            used_bytes: 0,
            live_bytes: 0,
            object_count: 0,
            occupied_lines: HashSet::new(),
        });
        let region_index = self.old_regions.len() - 1;
        let offset = self.old_regions[region_index].used_bytes;
        self.old_regions[region_index].used_bytes = bytes;
        self.make_old_region_placement(region_index, offset, bytes)
    }

    fn try_reserve_in_existing_region(
        &mut self,
        bytes: usize,
        align: usize,
    ) -> Option<(usize, usize)> {
        for (region_index, region) in self.old_regions.iter_mut().enumerate() {
            let offset = align_up(region.used_bytes, align);
            if offset.saturating_add(bytes) <= region.capacity_bytes {
                region.used_bytes = offset.saturating_add(bytes);
                return Some((region_index, offset));
            }
        }
        None
    }

    fn make_old_region_placement(
        &self,
        region_index: usize,
        offset_bytes: usize,
        bytes: usize,
    ) -> OldRegionPlacement {
        let line_bytes = self.config.old.line_bytes.max(1);
        let line_start = offset_bytes / line_bytes;
        let line_count = bytes.div_ceil(line_bytes).max(1);
        OldRegionPlacement {
            region_index,
            offset_bytes,
            line_start,
            line_count,
        }
    }

    fn record_old_region_object(&mut self, object: &ObjectRecord) {
        let Some(placement) = object.old_region_placement() else {
            return;
        };
        let region = &mut self.old_regions[placement.region_index];
        region.live_bytes = region.live_bytes.saturating_add(object.total_size());
        region.object_count = region.object_count.saturating_add(1);
        for line in placement.line_start..placement.line_start + placement.line_count {
            region.occupied_lines.insert(line);
        }
        self.stats.old.reserved_bytes = self
            .old_regions
            .iter()
            .map(|entry| entry.capacity_bytes)
            .sum();
    }

    fn recompute_old_region_metadata_for_plan(
        &mut self,
        completed_plan: Option<CollectionPlan>,
    ) -> OldRegionCollectionStats {
        let target_old_regions = completed_plan
            .filter(|plan| matches!(plan.kind, CollectionKind::Major | CollectionKind::Full))
            .map_or_else(Vec::new, |plan| plan.selected_old_regions);
        if target_old_regions.is_empty() {
            return self.refresh_old_region_metadata_preserving_live_layout();
        }
        self.rebuild_old_region_metadata_with_selected_compaction(&target_old_regions)
    }

    fn layout_regions_with_live_objects(&self) -> Vec<OldRegion> {
        let mut regions: Vec<_> = self
            .old_regions
            .iter()
            .map(|region| OldRegion {
                capacity_bytes: region.capacity_bytes,
                used_bytes: region.used_bytes,
                live_bytes: 0,
                object_count: 0,
                occupied_lines: HashSet::new(),
            })
            .collect();

        for object in &self.objects {
            if object.space() != SpaceKind::Old {
                continue;
            }
            let Some(placement) = object.old_region_placement() else {
                continue;
            };
            let Some(region) = regions.get_mut(placement.region_index) else {
                continue;
            };
            region.live_bytes = region.live_bytes.saturating_add(object.total_size());
            region.object_count = region.object_count.saturating_add(1);
            for line in placement.line_start..placement.line_start + placement.line_count {
                region.occupied_lines.insert(line);
            }
        }

        regions
    }

    fn region_stats_from_metadata(&self, regions: &[OldRegion]) -> Vec<OldRegionStats> {
        regions
            .iter()
            .enumerate()
            .map(|(region_index, region)| OldRegionStats {
                region_index,
                reserved_bytes: region.capacity_bytes,
                used_bytes: region.used_bytes,
                live_bytes: region.live_bytes,
                free_bytes: region.capacity_bytes.saturating_sub(region.live_bytes),
                hole_bytes: region.used_bytes.saturating_sub(region.live_bytes),
                tail_bytes: region.capacity_bytes.saturating_sub(region.used_bytes),
                object_count: region.object_count,
                occupied_lines: region.occupied_lines.len(),
            })
            .collect()
    }

    fn refresh_old_region_metadata_preserving_live_layout(&mut self) -> OldRegionCollectionStats {
        let current_regions = self.layout_regions_with_live_objects();
        let reclaimed_regions = current_regions
            .iter()
            .filter(|region| region.object_count == 0)
            .count() as u64;
        let mut preserved_regions = Vec::new();
        let mut index_map = HashMap::new();

        for (old_index, region) in current_regions.into_iter().enumerate() {
            if region.object_count == 0 {
                continue;
            }
            index_map.insert(old_index, preserved_regions.len());
            preserved_regions.push(region);
        }

        for object in &mut self.objects {
            if object.space() != SpaceKind::Old {
                continue;
            }
            let Some(mut placement) = object.old_region_placement() else {
                continue;
            };
            if let Some(&new_index) = index_map.get(&placement.region_index) {
                placement.region_index = new_index;
                object.set_old_region_placement(placement);
            }
        }

        self.old_regions = preserved_regions;
        OldRegionCollectionStats {
            compacted_regions: 0,
            reclaimed_regions,
        }
    }

    fn rebuild_old_region_metadata_with_selected_compaction(
        &mut self,
        selected: &[usize],
    ) -> OldRegionCollectionStats {
        let selected: HashSet<_> = selected.iter().copied().collect();
        let current_regions = self.layout_regions_with_live_objects();
        let old_region_count = current_regions.len();
        let mut rebuilt_regions = Vec::new();
        let mut preserved_index_map = HashMap::new();

        for (old_index, region) in current_regions.iter().enumerate() {
            if region.object_count == 0 || selected.contains(&old_index) {
                continue;
            }
            preserved_index_map.insert(old_index, rebuilt_regions.len());
            rebuilt_regions.push(OldRegion {
                capacity_bytes: region.capacity_bytes,
                used_bytes: region.used_bytes,
                live_bytes: 0,
                object_count: 0,
                occupied_lines: HashSet::new(),
            });
        }

        let compacted_base_index = rebuilt_regions.len();
        let old_config = self.config.old;
        let mut compacted_regions = Vec::new();

        for object in &mut self.objects {
            if object.space() != SpaceKind::Old {
                continue;
            }

            let Some(mut placement) = object.old_region_placement() else {
                continue;
            };
            if selected.contains(&placement.region_index) {
                let compacted = reserve_old_region_placement_in(
                    &mut compacted_regions,
                    &old_config,
                    object.total_size(),
                );
                placement.region_index = compacted_base_index + compacted.region_index;
                placement.offset_bytes = compacted.offset_bytes;
                placement.line_start = compacted.line_start;
                placement.line_count = compacted.line_count;
                object.set_old_region_placement(placement);
            } else if let Some(&new_index) = preserved_index_map.get(&placement.region_index) {
                placement.region_index = new_index;
                object.set_old_region_placement(placement);
            }
        }

        rebuilt_regions.extend(compacted_regions);
        for object in &self.objects {
            if object.space() != SpaceKind::Old {
                continue;
            }
            let Some(placement) = object.old_region_placement() else {
                continue;
            };
            let region = &mut rebuilt_regions[placement.region_index];
            region.live_bytes = region.live_bytes.saturating_add(object.total_size());
            region.object_count = region.object_count.saturating_add(1);
            for line in placement.line_start..placement.line_start + placement.line_count {
                region.occupied_lines.insert(line);
            }
        }

        self.old_regions = rebuilt_regions;
        OldRegionCollectionStats {
            compacted_regions: selected.len() as u64,
            reclaimed_regions: old_region_count.saturating_sub(self.old_regions.len()) as u64,
        }
    }
}

fn compare_compaction_candidate_priority(
    left: &OldRegionStats,
    right: &OldRegionStats,
) -> core::cmp::Ordering {
    let left_live = left.live_bytes.max(1) as u128;
    let right_live = right.live_bytes.max(1) as u128;
    let left_efficiency = (left.hole_bytes as u128).saturating_mul(right_live);
    let right_efficiency = (right.hole_bytes as u128).saturating_mul(left_live);

    right_efficiency
        .cmp(&left_efficiency)
        .then_with(|| right.hole_bytes.cmp(&left.hole_bytes))
        .then_with(|| left.live_bytes.cmp(&right.live_bytes))
        .then_with(|| right.free_bytes.cmp(&left.free_bytes))
        .then_with(|| left.object_count.cmp(&right.object_count))
        .then_with(|| left.region_index.cmp(&right.region_index))
}

fn align_up(value: usize, align: usize) -> usize {
    if align <= 1 {
        value
    } else {
        let rem = value % align;
        if rem == 0 {
            value
        } else {
            value + (align - rem)
        }
    }
}

fn reserve_old_region_placement_in(
    regions: &mut Vec<OldRegion>,
    config: &OldGenConfig,
    bytes: usize,
) -> OldRegionPlacement {
    let align = config.line_bytes.max(8);

    for (region_index, region) in regions.iter_mut().enumerate() {
        let offset = align_up(region.used_bytes, align);
        if offset.saturating_add(bytes) <= region.capacity_bytes {
            region.used_bytes = offset.saturating_add(bytes);
            return make_old_region_placement(config, region_index, offset, bytes);
        }
    }

    let capacity_bytes = config.region_bytes.max(bytes);
    regions.push(OldRegion {
        capacity_bytes,
        used_bytes: bytes,
        live_bytes: 0,
        object_count: 0,
        occupied_lines: HashSet::new(),
    });
    let region_index = regions.len() - 1;
    make_old_region_placement(config, region_index, 0, bytes)
}

fn make_old_region_placement(
    config: &OldGenConfig,
    region_index: usize,
    offset_bytes: usize,
    bytes: usize,
) -> OldRegionPlacement {
    let line_bytes = config.line_bytes.max(1);
    let line_start = offset_bytes / line_bytes;
    let line_count = bytes.div_ceil(line_bytes).max(1);
    OldRegionPlacement {
        region_index,
        offset_bytes,
        line_start,
        line_count,
    }
}

struct WeakRetention<'a> {
    objects: &'a [ObjectRecord],
    index: &'a HashMap<NonNull<ObjectHeader>, usize>,
    forwarding: &'a HashMap<NonNull<ObjectHeader>, GcErased>,
    kind: CollectionKind,
}

impl<'a> WeakRetention<'a> {
    fn new(
        objects: &'a [ObjectRecord],
        index: &'a HashMap<NonNull<ObjectHeader>, usize>,
        forwarding: &'a HashMap<NonNull<ObjectHeader>, GcErased>,
        kind: CollectionKind,
    ) -> Self {
        Self {
            objects,
            index,
            forwarding,
            kind,
        }
    }

    fn record_for(&self, object: GcErased) -> Option<&'a ObjectRecord> {
        self.index
            .get(&object.header())
            .map(|&index| &self.objects[index])
    }
}

impl WeakProcessor for WeakRetention<'_> {
    fn remap_or_drop(&mut self, object: GcErased) -> Option<GcErased> {
        if let Some(&forwarded) = self.forwarding.get(&object.header()) {
            return Some(forwarded);
        }
        let Some(record) = self.record_for(object) else {
            return None;
        };
        if record.space() == SpaceKind::Immortal {
            return Some(object);
        }
        match self.kind {
            CollectionKind::Minor => {
                (record.space() != SpaceKind::Nursery || record.is_marked()).then_some(object)
            }
            CollectionKind::Major | CollectionKind::Full => record.is_marked().then_some(object),
        }
    }
}

#[derive(Clone, Copy)]
struct ParallelWeakShared<'a> {
    objects_ptr: *const ObjectRecord,
    objects_len: usize,
    index_ptr: *const HashMap<NonNull<ObjectHeader>, usize>,
    forwarding_ptr: *const HashMap<NonNull<ObjectHeader>, GcErased>,
    kind: CollectionKind,
    _marker: PhantomData<&'a ()>,
}

impl<'a> ParallelWeakShared<'a> {
    fn new(
        objects: &'a [ObjectRecord],
        index: &'a HashMap<NonNull<ObjectHeader>, usize>,
        forwarding: &'a HashMap<NonNull<ObjectHeader>, GcErased>,
        kind: CollectionKind,
    ) -> Self {
        Self {
            objects_ptr: objects.as_ptr(),
            objects_len: objects.len(),
            index_ptr: index as *const _,
            forwarding_ptr: forwarding as *const _,
            kind,
            _marker: PhantomData,
        }
    }

    fn objects(self) -> &'a [ObjectRecord] {
        unsafe { slice::from_raw_parts(self.objects_ptr, self.objects_len) }
    }

    fn processor(self) -> WeakRetention<'a> {
        WeakRetention::new(
            self.objects(),
            unsafe { &*self.index_ptr },
            unsafe { &*self.forwarding_ptr },
            self.kind,
        )
    }
}

// SAFETY: `ParallelWeakShared` is used only during stop-the-world weak processing.
// Workers read a stable liveness/forwarding view and mutate weak slots on disjoint
// object payloads through per-object interior mutability.
unsafe impl Send for ParallelWeakShared<'_> {}
unsafe impl Sync for ParallelWeakShared<'_> {}

struct ForwardingRelocator<'a> {
    forwarding: &'a HashMap<NonNull<ObjectHeader>, GcErased>,
}

impl<'a> ForwardingRelocator<'a> {
    fn new(forwarding: &'a HashMap<NonNull<ObjectHeader>, GcErased>) -> Self {
        Self { forwarding }
    }
}

impl Relocator for ForwardingRelocator<'_> {
    fn relocate_erased(&mut self, object: GcErased) -> GcErased {
        self.forwarding
            .get(&object.header())
            .copied()
            .unwrap_or(object)
    }
}

struct MajorEphemeronTracer<'a, 'b> {
    tracer: &'a mut MarkTracer<'b>,
    changed: bool,
}

impl<'a, 'b> MajorEphemeronTracer<'a, 'b> {
    fn new(tracer: &'a mut MarkTracer<'b>) -> Self {
        Self {
            tracer,
            changed: false,
        }
    }

    fn finish(self) -> &'a mut MarkTracer<'b> {
        self.tracer
    }
}

impl EphemeronVisitor for MajorEphemeronTracer<'_, '_> {
    fn visit_ephemeron(&mut self, key: GcErased, value: GcErased) {
        let Some(&key_index) = self.tracer.index.get(&key.header()) else {
            return;
        };
        if !self.tracer.objects[key_index].is_marked() {
            return;
        }

        let Some(&value_index) = self.tracer.index.get(&value.header()) else {
            return;
        };
        let value_record = &self.tracer.objects[value_index];
        if value_record.mark_if_unmarked() {
            self.tracer.worklist.push(value_index);
            self.changed = true;
        }
    }
}

struct MajorMarkSession<'a> {
    objects: &'a [ObjectRecord],
    tracer: MarkTracer<'a>,
    worker_count: usize,
    slice_budget: usize,
    mark_steps: u64,
    mark_rounds: u64,
}

impl<'a> MajorMarkSession<'a> {
    fn new(
        objects: &'a [ObjectRecord],
        index: &'a HashMap<NonNull<ObjectHeader>, usize>,
        worker_count: usize,
        slice_budget: usize,
    ) -> Self {
        Self {
            objects,
            tracer: MarkTracer::new(objects, index),
            worker_count,
            slice_budget,
            mark_steps: 0,
            mark_rounds: 0,
        }
    }

    fn seed(&mut self, root: GcErased) {
        self.tracer.mark_erased(root);
    }

    fn drain_parallel(&mut self) {
        let (steps, rounds) = self
            .tracer
            .drain_parallel_until_empty(self.worker_count, self.slice_budget);
        self.mark_steps = self.mark_steps.saturating_add(steps);
        self.mark_rounds = self.mark_rounds.saturating_add(rounds);
    }

    fn run_ephemeron_fixpoint_parallel(&mut self) {
        loop {
            let changed = if self.worker_count.max(1) == 1 || self.objects.len() <= 1 {
                let mut visitor = MajorEphemeronTracer::new(&mut self.tracer);
                for object in self.objects {
                    if object.is_marked() {
                        object.visit_ephemerons(&mut visitor);
                    }
                }
                let changed = visitor.changed;
                let _tracer = visitor.finish();
                changed
            } else {
                self.scan_ephemerons_parallel()
            };
            let (steps, rounds) = self
                .tracer
                .drain_parallel_until_empty(self.worker_count, self.slice_budget);
            self.mark_steps = self.mark_steps.saturating_add(steps);
            self.mark_rounds = self.mark_rounds.saturating_add(rounds);
            if !changed {
                break;
            }
        }
    }

    fn scan_ephemerons_parallel(&mut self) -> bool {
        let workers = self.worker_count.max(1).min(self.objects.len().max(1));
        let chunk_size = self.objects.len().max(1).div_ceil(workers);
        let shared = ParallelMarkShared::new(self.objects, self.tracer.index);
        let worker_outputs = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(workers);
            for worker_index in 0..workers {
                let shared = shared;
                let start = worker_index.saturating_mul(chunk_size);
                let end = (start + chunk_size).min(self.objects.len());
                if start >= end {
                    continue;
                }
                handles.push(scope.spawn(move || {
                    let mut worker = shared.tracer(MarkWorklist::default());
                    let changed = {
                        let mut visitor = MajorEphemeronTracer::new(&mut worker);
                        for object in &shared.objects()[start..end] {
                            if object.is_marked() {
                                object.visit_ephemerons(&mut visitor);
                            }
                        }
                        visitor.changed
                    };
                    (changed, worker.into_worklist())
                }));
            }

            let mut outputs = Vec::with_capacity(handles.len());
            for handle in handles {
                outputs.push(handle.join().expect("parallel ephemeron worker panicked"));
            }
            outputs
        });

        let mut changed = false;
        for (worker_changed, mut worklist) in worker_outputs {
            changed |= worker_changed;
            self.tracer.worklist.append(&mut worklist);
        }
        changed
    }

    fn mark_steps(&self) -> u64 {
        self.mark_steps
    }

    fn mark_rounds(&self) -> u64 {
        self.mark_rounds
    }
}

struct MinorEphemeronTracer<'a, 'b> {
    tracer: &'a mut MinorTracer<'b>,
    changed: bool,
}

impl<'a, 'b> MinorEphemeronTracer<'a, 'b> {
    fn new(tracer: &'a mut MinorTracer<'b>) -> Self {
        Self {
            tracer,
            changed: false,
        }
    }

    fn finish(self) -> &'a mut MinorTracer<'b> {
        self.tracer
    }
}

impl EphemeronVisitor for MinorEphemeronTracer<'_, '_> {
    fn visit_ephemeron(&mut self, key: GcErased, value: GcErased) {
        let Some(&key_index) = self.tracer.index.get(&key.header()) else {
            return;
        };
        let key_record = &self.tracer.objects[key_index];
        let key_is_live = key_record.space() != SpaceKind::Nursery || key_record.is_marked();
        if !key_is_live {
            return;
        }

        let Some(&value_index) = self.tracer.index.get(&value.header()) else {
            return;
        };
        let value_record = &self.tracer.objects[value_index];
        if value_record.space() == SpaceKind::Nursery && value_record.mark_if_unmarked() {
            self.tracer.young_worklist.push(value_index);
            self.changed = true;
        }
    }
}

struct MarkTracer<'a> {
    objects: &'a [ObjectRecord],
    index: &'a HashMap<NonNull<ObjectHeader>, usize>,
    worklist: MarkWorklist<usize>,
}

#[derive(Clone, Copy)]
struct ParallelMarkShared<'a> {
    objects_ptr: *const ObjectRecord,
    objects_len: usize,
    index_ptr: *const HashMap<NonNull<ObjectHeader>, usize>,
    _marker: PhantomData<&'a ()>,
}

impl<'a> ParallelMarkShared<'a> {
    fn new(objects: &'a [ObjectRecord], index: &'a HashMap<NonNull<ObjectHeader>, usize>) -> Self {
        Self {
            objects_ptr: objects.as_ptr(),
            objects_len: objects.len(),
            index_ptr: index as *const _,
            _marker: PhantomData,
        }
    }

    fn tracer(self, worklist: MarkWorklist<usize>) -> MarkTracer<'a> {
        MarkTracer::with_worklist(self.objects(), self.index(), worklist)
    }

    fn minor_tracer(self, worklist: MarkWorklist<usize>) -> MinorTracer<'a> {
        MinorTracer::with_worklist(self.objects(), self.index(), worklist)
    }

    fn objects(self) -> &'a [ObjectRecord] {
        unsafe { slice::from_raw_parts(self.objects_ptr, self.objects_len) }
    }

    fn index(self) -> &'a HashMap<NonNull<ObjectHeader>, usize> {
        unsafe { &*self.index_ptr }
    }
}

// SAFETY: `ParallelMarkShared` is only constructed for stop-the-world mark rounds.
// During those rounds, the object graph and index are read-only across workers.
// The only shared mutation is through per-object atomic mark bits, while each
// worker owns a private worklist.
unsafe impl Send for ParallelMarkShared<'_> {}
unsafe impl Sync for ParallelMarkShared<'_> {}

impl<'a> MarkTracer<'a> {
    const SPLIT_THRESHOLD: usize = 32;

    fn new(objects: &'a [ObjectRecord], index: &'a HashMap<NonNull<ObjectHeader>, usize>) -> Self {
        Self {
            objects,
            index,
            worklist: MarkWorklist::default(),
        }
    }

    fn with_worklist(
        objects: &'a [ObjectRecord],
        index: &'a HashMap<NonNull<ObjectHeader>, usize>,
        worklist: MarkWorklist<usize>,
    ) -> Self {
        Self {
            objects,
            index,
            worklist,
        }
    }

    fn into_worklist(self) -> MarkWorklist<usize> {
        self.worklist
    }

    fn mark_index(&mut self, index: usize) {
        let object = &self.objects[index];
        if object.mark_if_unmarked() {
            self.worklist.push(index);
        }
    }

    fn pending_count(&self) -> usize {
        self.worklist.len()
    }

    fn drain_one_slice(&mut self, slice_budget: usize) -> usize {
        let budget = slice_budget.max(1);
        let mut drained = 0usize;

        if self.worklist.len() > Self::SPLIT_THRESHOLD {
            let mut spill = self.worklist.split_half();
            while drained < budget {
                let Some(index) = spill.pop() else {
                    break;
                };
                self.objects[index].trace_edges(self);
                drained += 1;
            }
            while let Some(index) = spill.pop() {
                self.worklist.push(index);
            }
        } else {
            while drained < budget {
                let Some(index) = self.worklist.pop() else {
                    break;
                };
                self.objects[index].trace_edges(self);
                drained += 1;
            }
        }

        drained
    }

    fn drain_worker_round(&mut self, worker_count: usize, slice_budget: usize) -> (usize, u64) {
        let workers = worker_count.max(1);
        if workers == 1 || self.worklist.len() <= 1 {
            let drained = self.drain_one_slice(slice_budget);
            return (drained, u64::from(drained > 0));
        }

        let mut worker_lists = vec![core::mem::take(&mut self.worklist)];
        while worker_lists.len() < workers {
            let Some((split_index, split_len)) = worker_lists
                .iter()
                .enumerate()
                .map(|(index, list)| (index, list.len()))
                .max_by_key(|(_, len)| *len)
            else {
                break;
            };
            if split_len <= 1 {
                break;
            }
            let stolen = worker_lists[split_index].split_half();
            worker_lists.push(stolen);
        }

        if worker_lists.len() == 1 {
            let mut only_worker = MarkTracer::with_worklist(
                self.objects,
                self.index,
                worker_lists.pop().expect("single worker list"),
            );
            let drained = only_worker.drain_one_slice(slice_budget);
            self.worklist = only_worker.into_worklist();
            return (drained, u64::from(drained > 0));
        }

        let mut drained_objects = 0usize;
        let mut drained_slices = 0u64;
        let shared = ParallelMarkShared::new(self.objects, self.index);
        let worker_outputs = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_lists.len());
            for worker_list in worker_lists {
                let shared = shared;
                handles.push(scope.spawn(move || {
                    let mut worker = shared.tracer(worker_list);
                    let drained = worker.drain_one_slice(slice_budget);
                    let remainder = worker.into_worklist();
                    (drained, remainder)
                }));
            }

            let mut outputs = Vec::with_capacity(handles.len());
            for handle in handles {
                outputs.push(handle.join().expect("parallel mark worker panicked"));
            }
            outputs
        });

        for (drained, mut remainder) in worker_outputs {
            if drained > 0 {
                drained_objects = drained_objects.saturating_add(drained);
                drained_slices = drained_slices.saturating_add(1);
            }
            self.worklist.append(&mut remainder);
        }

        (drained_objects, drained_slices)
    }

    fn drain_parallel_until_empty(
        &mut self,
        worker_count: usize,
        slice_budget: usize,
    ) -> (u64, u64) {
        let mut slices = 0u64;
        let mut rounds = 0u64;
        while !self.worklist.is_empty() {
            let (_drained_objects, drained_slices) =
                self.drain_worker_round(worker_count, slice_budget);
            if drained_slices > 0 {
                slices = slices.saturating_add(drained_slices);
                rounds = rounds.saturating_add(1);
            }
        }
        (slices, rounds)
    }
}

impl Tracer for MarkTracer<'_> {
    fn mark_erased(&mut self, object: GcErased) {
        if let Some(&index) = self.index.get(&object.header()) {
            self.mark_index(index);
        }
    }
}

struct MinorTracer<'a> {
    objects: &'a [ObjectRecord],
    index: &'a HashMap<NonNull<ObjectHeader>, usize>,
    young_worklist: MarkWorklist<usize>,
}

impl<'a> MinorTracer<'a> {
    const SPLIT_THRESHOLD: usize = 32;

    fn new(objects: &'a [ObjectRecord], index: &'a HashMap<NonNull<ObjectHeader>, usize>) -> Self {
        Self {
            objects,
            index,
            young_worklist: MarkWorklist::default(),
        }
    }

    fn with_worklist(
        objects: &'a [ObjectRecord],
        index: &'a HashMap<NonNull<ObjectHeader>, usize>,
        young_worklist: MarkWorklist<usize>,
    ) -> Self {
        Self {
            objects,
            index,
            young_worklist,
        }
    }

    fn into_worklist(self) -> MarkWorklist<usize> {
        self.young_worklist
    }

    fn scan_source(&mut self, object: GcErased) {
        let Some(&index) = self.index.get(&object.header()) else {
            return;
        };
        let source = &self.objects[index];
        if source.space() == SpaceKind::Nursery {
            self.mark_young(index);
        } else {
            source.trace_edges(self);
        }
    }

    fn mark_young(&mut self, index: usize) {
        let object = &self.objects[index];
        if object.space() == SpaceKind::Nursery && object.mark_if_unmarked() {
            self.young_worklist.push(index);
        }
    }

    fn drain_one_slice(&mut self, slice_budget: usize) -> usize {
        let budget = slice_budget.max(1);
        let mut drained = 0usize;

        if self.young_worklist.len() > Self::SPLIT_THRESHOLD {
            let mut spill = self.young_worklist.split_half();
            while drained < budget {
                let Some(index) = spill.pop() else {
                    break;
                };
                self.objects[index].trace_edges(self);
                drained += 1;
            }
            while let Some(index) = spill.pop() {
                self.young_worklist.push(index);
            }
        } else {
            while drained < budget {
                let Some(index) = self.young_worklist.pop() else {
                    break;
                };
                self.objects[index].trace_edges(self);
                drained += 1;
            }
        }

        drained
    }

    fn drain_worker_round(&mut self, worker_count: usize, slice_budget: usize) -> (usize, u64) {
        let workers = worker_count.max(1);
        if workers == 1 || self.young_worklist.len() <= 1 {
            let drained = self.drain_one_slice(slice_budget);
            return (drained, u64::from(drained > 0));
        }

        let mut worker_lists = vec![core::mem::take(&mut self.young_worklist)];
        while worker_lists.len() < workers {
            let Some((split_index, split_len)) = worker_lists
                .iter()
                .enumerate()
                .map(|(index, list)| (index, list.len()))
                .max_by_key(|(_, len)| *len)
            else {
                break;
            };
            if split_len <= 1 {
                break;
            }
            let stolen = worker_lists[split_index].split_half();
            worker_lists.push(stolen);
        }

        if worker_lists.len() == 1 {
            let mut only_worker = MinorTracer::with_worklist(
                self.objects,
                self.index,
                worker_lists.pop().expect("single worker list"),
            );
            let drained = only_worker.drain_one_slice(slice_budget);
            self.young_worklist = only_worker.into_worklist();
            return (drained, u64::from(drained > 0));
        }

        let shared = ParallelMarkShared::new(self.objects, self.index);
        let worker_outputs = thread::scope(|scope| {
            let mut handles = Vec::with_capacity(worker_lists.len());
            for worker_list in worker_lists {
                let shared = shared;
                handles.push(scope.spawn(move || {
                    let mut worker = shared.minor_tracer(worker_list);
                    let drained = worker.drain_one_slice(slice_budget);
                    let remainder = worker.into_worklist();
                    (drained, remainder)
                }));
            }

            let mut outputs = Vec::with_capacity(handles.len());
            for handle in handles {
                outputs.push(handle.join().expect("parallel minor worker panicked"));
            }
            outputs
        });

        let mut drained_objects = 0usize;
        let mut drained_slices = 0u64;
        for (drained, mut remainder) in worker_outputs {
            if drained > 0 {
                drained_objects = drained_objects.saturating_add(drained);
                drained_slices = drained_slices.saturating_add(1);
            }
            self.young_worklist.append(&mut remainder);
        }

        (drained_objects, drained_slices)
    }

    fn drain_parallel_until_empty(
        &mut self,
        worker_count: usize,
        slice_budget: usize,
    ) -> (u64, u64) {
        let mut slices = 0u64;
        let mut rounds = 0u64;
        while !self.young_worklist.is_empty() {
            let (_drained_objects, drained_slices) =
                self.drain_worker_round(worker_count, slice_budget);
            if drained_slices > 0 {
                slices = slices.saturating_add(drained_slices);
                rounds = rounds.saturating_add(1);
            }
        }
        (slices, rounds)
    }
}

impl Tracer for MinorTracer<'_> {
    fn mark_erased(&mut self, object: GcErased) {
        let Some(&index) = self.index.get(&object.header()) else {
            return;
        };
        if self.objects[index].space() == SpaceKind::Nursery {
            self.mark_young(index);
        }
    }
}
