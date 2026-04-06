use core::any::TypeId;
use core::ptr::NonNull;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::background::{BackgroundCollectorConfig, BackgroundService, SharedHeap};
use crate::barrier::{BarrierEvent, BarrierKind};
use crate::collector_exec::{
    collect_global_sources, process_weak_references_for_candidates, trace_major as run_major_trace,
    trace_major_ephemerons_for_candidates, trace_minor as run_minor_trace,
};
use crate::collector_policy::refresh_cached_plans as refresh_cached_collector_plans;
use crate::collector_session::{
    active_reclaim_prep_request, advance_major_mark_slice, begin_major_mark,
    build_prepared_active_reclaim, complete_active_reclaim_prep, finish_active_collection,
    finish_major_mark, poll_active_major_mark_with_completion,
    prepare_active_major_reclaim_with_request, prepare_active_reclaim,
};
use crate::collector_state::{CollectorSharedSnapshot, CollectorState};
use crate::descriptor::{GcErased, Trace, TypeDesc, fixed_type_desc};
use crate::index_state::{ForwardingMap, HeapIndexState};
use crate::mutator::Mutator;
use crate::object::{ObjectRecord, SpaceKind, estimated_allocation_size};
use crate::plan::{
    CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress, RuntimeWorkStatus,
};
use crate::reclaim::{
    PreparedReclaim, finish_prepared_reclaim_cycle,
    prepare_full_reclaim as orchestrate_full_reclaim,
    prepare_major_reclaim as orchestrate_major_reclaim, prepare_reclaim,
    sweep_minor_and_rebuild_post_collection as rebuild_minor_after_collection,
};
use crate::root::{HandleScope, Root, RootStack};
use crate::runtime::CollectorRuntime;
use crate::runtime_state::RuntimeState;
use crate::spaces::nursery::{
    evacuate_marked_nursery as evacuate_nursery_space,
    relocate_roots_and_edges as relocate_forwarded_roots_and_edges,
};
use crate::spaces::{
    LargeObjectSpaceConfig, NurseryConfig, OldGenConfig, OldGenPlanSelection, OldGenState,
    PinnedSpaceConfig,
};
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
    indexes: HeapIndexState,
    old_gen: OldGenState,
    recent_barrier_events: Vec<BarrierEvent>,
    runtime_state: Arc<Mutex<RuntimeState>>,
    collector: Mutex<CollectorState>,
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
            runtime_state: Arc::new(Mutex::new(RuntimeState::default())),
            collector: Mutex::new(CollectorState::default()),
        };
        heap.refresh_recommended_plans();
        heap
    }

    fn collector(&self) -> std::sync::MutexGuard<'_, CollectorState> {
        self.collector
            .lock()
            .expect("collector state should not be poisoned")
    }

    fn runtime_state(&self) -> std::sync::MutexGuard<'_, RuntimeState> {
        self.runtime_state
            .lock()
            .expect("runtime state should not be poisoned")
    }

    pub(crate) fn runtime_state_handle(&self) -> Arc<Mutex<RuntimeState>> {
        Arc::clone(&self.runtime_state)
    }

    /// Return the heap configuration.
    pub fn config(&self) -> &HeapConfig {
        &self.config
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> HeapStats {
        let runtime_state = self.runtime_state();
        let mut stats = self.storage_stats();
        let (finalizers_run, pending_finalizers) = runtime_state.snapshot();
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
        self.runtime_state().snapshot()
    }

    /// Return runtime-side follow-up work that remains outside GC commit.
    pub fn runtime_work_status(&self) -> RuntimeWorkStatus {
        RuntimeWorkStatus::from_pending_finalizers(self.pending_finalizer_count())
    }

    pub(crate) fn collector_shared_snapshot(&self) -> CollectorSharedSnapshot {
        self.collector().shared_snapshot()
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
        self.collector().recommended_plan()
    }

    /// Recommend the next background concurrent collection plan, if any.
    pub fn recommended_background_plan(&self) -> Option<CollectionPlan> {
        self.collector().recommended_background_plan()
    }

    fn refresh_recommended_plans(&self) {
        let mut collector = self.collector();
        self.refresh_collector_cached_plans(&mut collector);
    }

    fn refresh_collector_cached_plans(&self, collector: &mut CollectorState) {
        refresh_cached_collector_plans(
            collector,
            &self.stats,
            &self.old_gen,
            &self.config.old,
            |kind| self.plan_for(kind),
        );
    }

    fn refreshed_collector_snapshot(
        &self,
        collector: &mut CollectorState,
    ) -> CollectorSharedSnapshot {
        self.refresh_collector_cached_plans(collector);
        collector.shared_snapshot()
    }

    /// Return the phases traversed by the most recently executed collection.
    pub fn recent_phase_trace(&self) -> Vec<CollectionPhase> {
        self.collector().recent_phase_trace().to_vec()
    }

    /// Return the most recently completed collection plan, if any.
    pub fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.collector().last_completed_plan()
    }

    /// Return the active major-mark plan, if one is in progress.
    pub fn active_major_mark_plan(&self) -> Option<CollectionPlan> {
        self.collector().active_major_mark_plan()
    }

    /// Return current progress for the active major-mark session, if any.
    pub fn major_mark_progress(&self) -> Option<MajorMarkProgress> {
        self.collector().major_mark_progress()
    }

    /// Begin a persistent major-mark session for `plan`.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.begin_major_mark_in_place(plan)
    }

    pub(crate) fn begin_major_mark_in_place(&self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.begin_major_mark_in_place_with_snapshot(plan)
            .map(|_| ())
    }

    pub(crate) fn begin_major_mark_in_place_with_snapshot(
        &self,
        plan: CollectionPlan,
    ) -> Result<CollectorSharedSnapshot, AllocError> {
        let mut collector = self.collector();
        begin_major_mark(
            &mut collector,
            &self.objects,
            &self.indexes.object_index,
            plan,
            collect_global_sources(&self.roots, &self.objects),
        )?;
        Ok(self.refreshed_collector_snapshot(&mut collector))
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&self) -> Result<MajorMarkProgress, AllocError> {
        let progress = advance_major_mark_slice(
            &mut self.collector(),
            &self.objects,
            &self.indexes.object_index,
        )?;
        self.refresh_recommended_plans();
        Ok(progress)
    }

    /// Finish the current persistent major-mark session and reclaim.
    pub fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        let pause_start = Instant::now();
        let Some(state) = self.collector().take_major_mark_state() else {
            return Err(AllocError::NoCollectionInProgress);
        };
        let before_bytes = self.stats.total_live_bytes();
        self.record_phase(CollectionPhase::Remark);
        let mut state = state;
        finish_major_mark(
            &mut state,
            &self.objects,
            &self.indexes.object_index,
            |tracer, plan| {
                trace_major_ephemerons_for_candidates(
                    &self.objects,
                    &self.indexes.object_index,
                    &self.indexes.ephemeron_candidates,
                    tracer,
                    plan.worker_count.max(1),
                    plan.mark_slice_budget,
                )
            },
        );
        let finished = finish_active_collection(state, |plan| match plan.kind {
            CollectionKind::Major => Ok(self.prepare_major_reclaim(plan)),
            CollectionKind::Full => self.prepare_full_reclaim(plan),
            CollectionKind::Minor => Err(AllocError::UnsupportedCollectionKind {
                kind: CollectionKind::Minor,
            }),
        })?;
        self.record_phase(CollectionPhase::Reclaim);
        let runtime_state = self.runtime_state_handle();
        let mut cycle = finish_prepared_reclaim_cycle(
            &mut self.objects,
            &mut self.indexes,
            &mut self.old_gen,
            &mut self.stats,
            before_bytes,
            finished.mark_steps,
            finished.mark_rounds,
            finished.reclaim_prepare_nanos,
            finished.prepared_reclaim,
            move |object| {
                let mut runtime_state = runtime_state
                    .lock()
                    .expect("runtime state should not be poisoned");
                runtime_state.enqueue_pending_finalizer(object)
            },
        );
        cycle.pause_nanos = Self::saturating_duration_nanos(pause_start.elapsed());
        self.record_collection_stats(cycle);
        self.collector()
            .set_last_completed_plan(Some(finished.completed_plan));
        self.refresh_recommended_plans();
        Ok(cycle)
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        if !self.collector().has_active_major_mark() {
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
            elapsed_nanos: progress.elapsed_nanos,
            mark_steps: progress.mark_steps,
            mark_rounds: progress.mark_rounds,
            remaining_work: progress.remaining_work,
        }))
    }

    /// Advance one scheduler-style concurrent major-mark round using the plan worker count.
    pub fn poll_active_major_mark(&self) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.poll_active_major_mark_with_snapshot()
            .map(|(progress, _)| progress)
    }

    pub(crate) fn poll_active_major_mark_with_snapshot(
        &self,
    ) -> Result<(Option<MajorMarkProgress>, CollectorSharedSnapshot), AllocError> {
        let mut collector = self.collector();
        let progress = poll_active_major_mark_with_completion(
            &mut collector,
            &self.objects,
            &self.indexes.object_index,
            |tracer, plan| {
                trace_major_ephemerons_for_candidates(
                    &self.objects,
                    &self.indexes.object_index,
                    &self.indexes.ephemeron_candidates,
                    tracer,
                    plan.worker_count.max(1),
                    plan.mark_slice_budget,
                )
            },
            |plan| {
                let empty_forwarding: ForwardingMap = HashMap::new();
                process_weak_references_for_candidates(
                    &self.objects,
                    &self.indexes.weak_candidates,
                    CollectionKind::Major,
                    plan.worker_count.max(1),
                    &empty_forwarding,
                    &self.indexes.object_index,
                );
                self.prepare_reclaim(CollectionKind::Major, plan)
            },
        )?;
        let Some(progress) = progress else {
            return Ok((None, collector.shared_snapshot()));
        };
        let snapshot = self.refreshed_collector_snapshot(&mut collector);
        Ok((Some(progress), snapshot))
    }

    pub(crate) fn prepare_active_major_reclaim_with_snapshot(
        &self,
    ) -> Result<(bool, CollectorSharedSnapshot), AllocError> {
        let mut collector = self.collector();
        let prepared = prepare_active_major_reclaim_with_request(
            &mut collector,
            &self.objects,
            &self.indexes.object_index,
            |tracer, plan| {
                trace_major_ephemerons_for_candidates(
                    &self.objects,
                    &self.indexes.object_index,
                    &self.indexes.ephemeron_candidates,
                    tracer,
                    plan.worker_count.max(1),
                    plan.mark_slice_budget,
                )
            },
            |plan| {
                let empty_forwarding: ForwardingMap = HashMap::new();
                process_weak_references_for_candidates(
                    &self.objects,
                    &self.indexes.weak_candidates,
                    CollectionKind::Major,
                    plan.worker_count.max(1),
                    &empty_forwarding,
                    &self.indexes.object_index,
                );
                self.prepare_reclaim(CollectionKind::Major, plan)
            },
        )?;
        let snapshot = self.refreshed_collector_snapshot(&mut collector);
        Ok((prepared, snapshot))
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        if self.prepare_active_reclaim_if_needed()? {
            self.refresh_recommended_plans();
            return Ok(None);
        }
        self.commit_active_reclaim_if_ready()
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        if !self.collector().active_major_mark_is_ready() {
            return Ok(None);
        }
        self.finish_major_collection().map(Some)
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
        self.runtime_state().pending_finalizer_count()
    }

    /// Run and drain queued finalizers.
    pub fn drain_pending_finalizers(&self) -> u64 {
        self.runtime_state().drain_pending_finalizers()
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
        BackgroundService::new(self, config)
    }

    /// Convert this heap into a shared synchronized heap wrapper.
    pub fn into_shared(self) -> SharedHeap {
        SharedHeap::from_heap(self)
    }

    /// Run one stop-the-world collection cycle.
    pub fn collect(&mut self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        if self.collector().has_active_major_mark() {
            return Err(AllocError::CollectionInProgress);
        }
        self.execute_plan(self.plan_for(kind))
    }

    /// Execute one scheduler-provided collection plan.
    pub fn execute_plan(&mut self, plan: CollectionPlan) -> Result<CollectionStats, AllocError> {
        if self.collector().has_active_major_mark() {
            return Err(AllocError::CollectionInProgress);
        }
        let pause_start = Instant::now();
        self.collector().clear_recent_phase_trace();
        let before_bytes = self.stats.total_live_bytes();
        for object in &self.objects {
            object.clear_mark();
        }

        let sources = collect_global_sources(&self.roots, &self.objects);
        let (mark_steps, mark_rounds) = match plan.kind {
            CollectionKind::Minor => run_minor_trace(
                &self.objects,
                &self.indexes.object_index,
                &self.indexes.remembered.owners,
                &self
                    .indexes
                    .candidate_indices(&self.indexes.ephemeron_candidates),
                plan.worker_count.max(1),
                plan.mark_slice_budget,
                sources.iter().copied(),
            ),
            CollectionKind::Major | CollectionKind::Full => {
                self.record_phase(CollectionPhase::InitialMark);
                if plan.concurrent {
                    self.record_phase(CollectionPhase::ConcurrentMark);
                }
                self.record_phase(CollectionPhase::Remark);
                run_major_trace(
                    &self.objects,
                    &self.indexes.object_index,
                    plan.worker_count.max(1),
                    plan.mark_slice_budget,
                    sources.iter().copied(),
                )
            }
        };

        let mut cycle = match plan.kind {
            CollectionKind::Minor => {
                self.record_phase(CollectionPhase::Evacuate);
                let evacuation = evacuate_nursery_space(
                    &mut self.objects,
                    &mut self.indexes,
                    &mut self.old_gen,
                    &self.config.old,
                    &self.config.nursery,
                    &mut self.stats,
                )?;
                relocate_forwarded_roots_and_edges(
                    &mut self.roots,
                    &self.objects,
                    &mut self.indexes,
                    &evacuation.forwarding,
                );
                process_weak_references_for_candidates(
                    &self.objects,
                    &self.indexes.weak_candidates,
                    plan.kind,
                    plan.worker_count.max(1),
                    &evacuation.forwarding,
                    &self.indexes.object_index,
                );
                self.record_phase(CollectionPhase::Reclaim);
                let rebuild =
                    self.sweep_minor_and_rebuild_post_collection(plan.kind, Some(plan.clone()));
                CollectionStats::completed_minor_cycle(
                    mark_steps,
                    mark_rounds,
                    evacuation.promoted_bytes,
                    before_bytes,
                    rebuild.after_bytes,
                    rebuild.queued_finalizers,
                    rebuild.old_region_stats,
                )
            }
            CollectionKind::Major => {
                let reclaim_prepare_start = Instant::now();
                let prepared_reclaim = self.prepare_major_reclaim(&plan);
                self.record_phase(CollectionPhase::Reclaim);
                let runtime_state = self.runtime_state_handle();
                finish_prepared_reclaim_cycle(
                    &mut self.objects,
                    &mut self.indexes,
                    &mut self.old_gen,
                    &mut self.stats,
                    before_bytes,
                    mark_steps,
                    mark_rounds,
                    Self::saturating_duration_nanos(reclaim_prepare_start.elapsed()),
                    prepared_reclaim,
                    move |object| {
                        let mut runtime_state = runtime_state
                            .lock()
                            .expect("runtime state should not be poisoned");
                        runtime_state.enqueue_pending_finalizer(object)
                    },
                )
            }
            CollectionKind::Full => {
                let reclaim_prepare_start = Instant::now();
                let prepared_reclaim = self.prepare_full_reclaim(&plan)?;
                self.record_phase(CollectionPhase::Reclaim);
                let runtime_state = self.runtime_state_handle();
                finish_prepared_reclaim_cycle(
                    &mut self.objects,
                    &mut self.indexes,
                    &mut self.old_gen,
                    &mut self.stats,
                    before_bytes,
                    mark_steps,
                    mark_rounds,
                    Self::saturating_duration_nanos(reclaim_prepare_start.elapsed()),
                    prepared_reclaim,
                    move |object| {
                        let mut runtime_state = runtime_state
                            .lock()
                            .expect("runtime state should not be poisoned");
                        runtime_state.enqueue_pending_finalizer(object)
                    },
                )
            }
        };
        cycle.pause_nanos = Self::saturating_duration_nanos(pause_start.elapsed());
        self.record_collection_stats(cycle);
        self.collector()
            .set_last_completed_plan(Some(CollectionPlan {
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
        if self.collector().has_active_major_mark() {
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
        if self.prepared_full_reclaim_active() {
            return Err(AllocError::CollectionInProgress);
        }
        let desc = self.descriptor_for::<T>();
        let payload_bytes = core::mem::size_of::<T>();
        let total_bytes = estimated_allocation_size::<T>()?;
        let space = self.select_space(desc, payload_bytes)?;
        if !self.collector().has_active_major_mark()
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
        assert!(
            !self.prepared_full_reclaim_active(),
            "cannot mutate heap edges while prepared full reclaim is active"
        );
        const MAX_BARRIER_EVENTS: usize = 1024;

        fn push_barrier_event(
            recent_barrier_events: &mut Vec<BarrierEvent>,
            kind: BarrierKind,
            owner: GcErased,
            slot: Option<usize>,
            old_value: Option<GcErased>,
            new_value: Option<GcErased>,
        ) {
            recent_barrier_events.push(BarrierEvent {
                kind,
                owner: unsafe { crate::root::Gc::from_erased(owner) },
                slot,
                old_value: old_value.map(|value| unsafe { crate::root::Gc::from_erased(value) }),
                new_value: new_value.map(|value| unsafe { crate::root::Gc::from_erased(value) }),
            });
            if recent_barrier_events.len() > MAX_BARRIER_EVENTS {
                let overflow = recent_barrier_events.len() - MAX_BARRIER_EVENTS;
                recent_barrier_events.drain(..overflow);
            }
        }

        push_barrier_event(
            &mut self.recent_barrier_events,
            BarrierKind::PostWrite,
            owner,
            slot,
            old_value,
            new_value,
        );

        if self.collector().has_active_major_mark() {
            if let Some(value) = old_value {
                push_barrier_event(
                    &mut self.recent_barrier_events,
                    BarrierKind::SatbPreWrite,
                    owner,
                    slot,
                    old_value,
                    new_value,
                );
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
            self.indexes.record_remembered_edge(owner, target);
        }

        self.assist_major_mark_in_place();
        if self.collector().has_active_major_mark() {
            self.refresh_recommended_plans();
        }
    }

    pub(crate) fn root_during_active_major_mark(&mut self, object: GcErased) {
        assert!(
            !self.prepared_full_reclaim_active(),
            "cannot add new roots while prepared full reclaim is active"
        );
        self.mark_for_active_major_session(object);
        self.assist_major_mark_in_place();
        self.refresh_recommended_plans();
    }

    pub(crate) fn prepared_full_reclaim_active(&self) -> bool {
        self.collector().has_prepared_full_reclaim()
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
        self.indexes
            .object_index
            .get(&object.object_key())
            .is_some_and(|&index| self.objects[index].is_marked())
    }

    fn mark_for_active_major_session(&self, object: GcErased) {
        if self.space_for_erased(object).is_none() {
            return;
        }

        let Some(&index) = self.indexes.object_index.get(&object.object_key()) else {
            return;
        };

        let record = &self.objects[index];
        if record.mark_if_unmarked() {
            let _enqueued = self.collector().enqueue_active_major_mark_index(index);
        }
    }

    fn assist_major_mark_in_place(&self) {
        let assist_slices = self.config.old.mutator_assist_slices;
        if assist_slices == 0 || !self.collector().has_active_major_mark() {
            return;
        }
        let _progress = self
            .assist_major_mark(assist_slices)
            .expect("mutator assist on active major-mark session should not fail");
    }

    fn sweep_minor_and_rebuild_post_collection(
        &mut self,
        kind: CollectionKind,
        completed_plan: Option<CollectionPlan>,
    ) -> crate::reclaim::MinorRebuildResult {
        let runtime_state = self.runtime_state_handle();
        rebuild_minor_after_collection(
            &mut self.objects,
            &mut self.indexes,
            &mut self.old_gen,
            &self.config.old,
            &mut self.stats,
            kind,
            completed_plan,
            move |object| {
                let mut runtime_state = runtime_state
                    .lock()
                    .expect("runtime state should not be poisoned");
                runtime_state.enqueue_pending_finalizer(object)
            },
        )
    }

    fn record_collection_stats(&mut self, cycle: CollectionStats) {
        self.stats.collections.saturating_add_assign(cycle);
    }

    fn record_phase(&self, phase: CollectionPhase) {
        self.collector().push_phase(phase);
    }

    fn saturating_duration_nanos(duration: Duration) -> u64 {
        duration.as_nanos().min(u128::from(u64::MAX)) as u64
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

    fn space_for_erased(&self, object: GcErased) -> Option<SpaceKind> {
        self.indexes
            .object_index
            .get(&object.object_key())
            .map(|&index| self.objects[index].space())
    }

    fn prepare_reclaim(&self, kind: CollectionKind, plan: &CollectionPlan) -> PreparedReclaim {
        prepare_reclaim(
            &self.objects,
            &self.indexes,
            &self.old_gen,
            &self.config.old,
            kind,
            plan,
        )
    }

    fn prepare_major_reclaim(&mut self, plan: &CollectionPlan) -> PreparedReclaim {
        orchestrate_major_reclaim(
            plan,
            |plan| {
                let empty_forwarding: ForwardingMap = HashMap::new();
                process_weak_references_for_candidates(
                    &self.objects,
                    &self.indexes.weak_candidates,
                    plan.kind,
                    plan.worker_count.max(1),
                    &empty_forwarding,
                    &self.indexes.object_index,
                );
            },
            |plan| self.prepare_reclaim(plan.kind, plan),
        )
    }

    fn prepare_full_reclaim(
        &mut self,
        plan: &CollectionPlan,
    ) -> Result<PreparedReclaim, AllocError> {
        self.record_phase(CollectionPhase::Evacuate);
        orchestrate_full_reclaim(
            self,
            plan,
            |heap| {
                let evacuation = evacuate_nursery_space(
                    &mut heap.objects,
                    &mut heap.indexes,
                    &mut heap.old_gen,
                    &heap.config.old,
                    &heap.config.nursery,
                    &mut heap.stats,
                )?;
                Ok((evacuation.forwarding, evacuation.promoted_bytes))
            },
            |heap, forwarding| {
                relocate_forwarded_roots_and_edges(
                    &mut heap.roots,
                    &heap.objects,
                    &mut heap.indexes,
                    forwarding,
                )
            },
            |heap, plan, forwarding| {
                process_weak_references_for_candidates(
                    &heap.objects,
                    &heap.indexes.weak_candidates,
                    plan.kind,
                    plan.worker_count.max(1),
                    forwarding,
                    &heap.indexes.object_index,
                );
            },
            |heap, plan| heap.prepare_reclaim(plan.kind, plan),
        )
    }

    pub(crate) fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        let request = {
            let collector = self.collector();
            active_reclaim_prep_request(&collector)
        };
        let Some(request) = request else {
            return Ok(false);
        };
        let (mark_steps_delta, mark_rounds_delta) = prepare_active_reclaim(
            &request,
            |tracer, plan| {
                trace_major_ephemerons_for_candidates(
                    &self.objects,
                    &self.indexes.object_index,
                    &self.indexes.ephemeron_candidates,
                    tracer,
                    plan.worker_count.max(1),
                    plan.mark_slice_budget,
                )
            },
            &self.objects,
            &self.indexes.object_index,
        );
        let prepared =
            build_prepared_active_reclaim(&request, mark_steps_delta, mark_rounds_delta, |plan| {
                match plan.kind {
                    CollectionKind::Major => Ok(self.prepare_major_reclaim(plan)),
                    CollectionKind::Full => self.prepare_full_reclaim(plan),
                    CollectionKind::Minor => Err(AllocError::UnsupportedCollectionKind {
                        kind: CollectionKind::Minor,
                    }),
                }
            })?;
        let result = complete_active_reclaim_prep(&mut self.collector(), prepared);
        Ok(result)
    }
}
