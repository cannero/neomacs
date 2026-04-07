use crate::background::{
    BackgroundCollectionRuntime, BackgroundCollectorConfig, BackgroundService, BackgroundWorker,
    BackgroundWorkerConfig, SharedBackgroundError, SharedBackgroundObservation,
    SharedBackgroundService, SharedBackgroundStatus, SharedBackgroundWaitResult,
    SharedCollectorHandle, SharedHeap, SharedHeapError, SharedHeapStatus, SharedRuntimeHandle,
};
use crate::barrier::BarrierKind;
use crate::collector_exec::{
    execute_collection_plan, prepare_major_reclaim_for_plan, trace_major_ephemerons_for_candidates,
};
use crate::collector_policy::refresh_cached_plans as refresh_cached_collector_plans;
use crate::collector_session::{self, build_prepared_active_reclaim, prepare_active_reclaim};
use crate::collector_state::{CollectorSharedSnapshot, CollectorState};
use crate::descriptor::{GcErased, TypeDesc};
use crate::heap::{AllocError, Heap};
use crate::object::SpaceKind;
use crate::plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
use crate::reclaim::{PreparedReclaim, finish_prepared_reclaim_cycle};
use crate::root::{HandleScope, Root};
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

    /// Create a background collection service loop bound to this runtime.
    pub fn background_service(self, config: BackgroundCollectorConfig) -> BackgroundService<'heap> {
        BackgroundService::from_runtime(self, config)
    }

    /// Run one stop-the-world collection cycle.
    pub fn collect(&mut self, kind: CollectionKind) -> Result<CollectionStats, AllocError> {
        self.execute_plan(self.heap.plan_for(kind))
    }

    /// Execute one scheduler-provided collection plan.
    pub fn execute_plan(&mut self, plan: CollectionPlan) -> Result<CollectionStats, AllocError> {
        if self.heap.collector_handle().has_active_major_mark() {
            return Err(AllocError::CollectionInProgress);
        }

        let pause_start = Instant::now();
        self.heap.collector_handle().clear_recent_phase_trace();
        let runtime_state = self.heap.runtime_state_handle();
        let mut phases = Vec::new();
        let (roots, objects, indexes, old_gen, stats, old_config, nursery_config, nursery) =
            self.heap.collection_exec_parts();
        let mut cycle = execute_collection_plan(
            &plan,
            roots,
            objects,
            indexes,
            old_gen,
            old_config,
            nursery_config,
            stats,
            nursery,
            &runtime_state,
            |phase| phases.push(phase),
        )?;
        self.heap.collector_handle().push_phases(phases);
        cycle.pause_nanos = pause_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.record_completed_cycle(
            cycle,
            CollectionPlan {
                phase: CollectionPhase::Reclaim,
                ..plan
            },
        );
        Ok(cycle)
    }

    pub(crate) fn service_allocation_pressure(
        &mut self,
        space: SpaceKind,
        bytes: usize,
    ) -> Result<(), AllocError> {
        if self.heap.collector_handle().has_active_major_mark() {
            return Ok(());
        }
        let Some(plan) = self.heap.allocation_pressure_plan(space, bytes) else {
            return Ok(());
        };
        self.dispatch_collection_plan(plan)
    }

    /// Dispatch one [`CollectionPlan`] either as a background
    /// concurrent major-mark session or as an immediate synchronous
    /// `execute_plan`. Used by both the static pressure path and
    /// the pacer-driven trigger so they pick the same path for the
    /// same plan kind.
    pub(crate) fn dispatch_collection_plan(
        &mut self,
        plan: CollectionPlan,
    ) -> Result<(), AllocError> {
        if plan.concurrent && matches!(plan.kind, CollectionKind::Major | CollectionKind::Full) {
            self.begin_major_mark(plan)
        } else {
            self.execute_plan(plan).map(|_| ())
        }
    }

    pub(crate) fn prepare_typed_allocation<T: crate::descriptor::Trace + 'static>(
        &mut self,
    ) -> Result<(), AllocError> {
        if self.heap.prepared_full_reclaim_active() {
            return Err(AllocError::CollectionInProgress);
        }
        let (_, space, total_bytes) = self.typed_allocation_profile::<T>()?;
        // Layer the adaptive pacer on top of the static thresholds.
        // The pacer never overrides the static path: if the static
        // pressure plan would already collect, that still wins. The
        // pacer only forces an additional collection when its model
        // believes the next major (or early minor) is due.
        //
        // We always advance the pacer's per-allocation accounting
        // here so its EWMA estimates stay current, then we run the
        // static plan, then we re-evaluate the pacer's decision.
        // The re-evaluation matters because the static path may
        // have completed a minor cycle in the meantime — that
        // resets the pacer's nursery counter and turns a stale
        // TriggerMinor back into Continue.
        self.heap.pacer().record_allocation(total_bytes, space);
        self.service_allocation_pressure(space, total_bytes)?;
        let pacer_decision = self.heap.pacer().decision();
        match pacer_decision {
            crate::pacer::PacerDecision::TriggerMajor => {
                if !self.heap.collector_handle().has_active_major_mark()
                    && self
                        .heap
                        .allocation_pressure_plan(space, total_bytes)
                        .is_none()
                {
                    // Honor the heap's `concurrent_mark_workers`
                    // configuration: a pacer-driven major should
                    // start a background mark session when the
                    // static path would have done so.
                    let plan = self.heap.plan_for(CollectionKind::Major);
                    self.heap.pacer().record_pacer_triggered_major();
                    self.dispatch_collection_plan(plan)?;
                }
            }
            crate::pacer::PacerDecision::TriggerMinor => {
                if !self.heap.collector_handle().has_active_major_mark()
                    && self
                        .heap
                        .allocation_pressure_plan(space, total_bytes)
                        .is_none()
                {
                    // Minor plans are never concurrent, so this
                    // dispatches to execute_plan via
                    // dispatch_collection_plan unconditionally.
                    let plan = self.heap.plan_for(CollectionKind::Minor);
                    self.heap.pacer().record_pacer_triggered_minor();
                    self.dispatch_collection_plan(plan)?;
                }
            }
            crate::pacer::PacerDecision::Continue => {}
        }
        Ok(())
    }

    pub(crate) fn alloc_typed<'scope, 'handle_heap, T: crate::descriptor::Trace + 'static>(
        &mut self,
        scope: &mut HandleScope<'scope, 'handle_heap>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        if self.heap.prepared_full_reclaim_active() {
            return Err(AllocError::CollectionInProgress);
        }
        let (desc, space, _) = self.typed_allocation_profile::<T>()?;

        // Phase 1: nursery allocations bump-allocate from the
        // semispace arena.
        // Phase 2: direct old-gen allocations bump-allocate from a
        // block pool. Both fall back to system allocation if the
        // relevant allocator cannot service the request.
        let mut record = match space {
            SpaceKind::Nursery => {
                let (layout, payload_offset) =
                    crate::object::allocation_layout_for::<T>()?;
                match self.heap.nursery_mut().try_alloc(layout) {
                    Some(base) => unsafe {
                        crate::object::ObjectRecord::allocate_in_arena::<T>(
                            desc,
                            space,
                            base,
                            layout,
                            payload_offset,
                            value,
                        )
                    },
                    None => crate::object::ObjectRecord::allocate(desc, space, value)?,
                }
            }
            SpaceKind::Old => {
                let (layout, payload_offset) =
                    crate::object::allocation_layout_for::<T>()?;
                let old_config = self.heap.old_config().clone();
                match self
                    .heap
                    .old_gen_mut()
                    .try_alloc_in_block(&old_config, layout)
                {
                    Some((placement, base)) => {
                        let mut record = unsafe {
                            crate::object::ObjectRecord::allocate_in_arena::<T>(
                                desc,
                                space,
                                base,
                                layout,
                                payload_offset,
                                value,
                            )
                        };
                        record.set_old_block_placement(placement);
                        record
                    }
                    None => crate::object::ObjectRecord::allocate(desc, space, value)?,
                }
            }
            _ => crate::object::ObjectRecord::allocate(desc, space, value)?,
        };
        let total_size = record.header().total_size();
        let (objects, indexes, old_gen, stats, old_config) = self.heap.allocation_commit_parts();
        if space == SpaceKind::Old {
            stats.old.reserved_bytes = old_gen.record_allocated_object(old_config, &mut record);
        }
        let gc = unsafe { crate::root::Gc::from_erased(record.erased()) };
        stats.record_allocation(space, total_size, old_gen.reserved_bytes());
        objects.push(record);
        let index = objects.len() - 1;
        let object_key = objects[index].object_key();
        let desc = objects[index].header().desc();
        indexes.record_allocated_object(object_key, index, desc);
        let had_active_major_mark = self.heap.collector_handle().has_active_major_mark();
        self.heap
            .collector_handle()
            .record_active_major_reachable_object_and_refresh(
                self.heap.objects(),
                &self.heap.indexes().object_index,
                gc.erase(),
                self.heap.config().old.mutator_assist_slices,
                &self.heap.storage_stats(),
                self.heap.old_gen(),
                self.heap.old_config(),
                |kind| self.heap.plan_for(kind),
            )?;
        if !had_active_major_mark {
            self.heap.refresh_recommended_plans();
        }
        Ok(scope.root(gc))
    }

    fn typed_allocation_profile<T: crate::descriptor::Trace + 'static>(
        &mut self,
    ) -> Result<(&'static TypeDesc, SpaceKind, usize), AllocError> {
        let desc = self.heap.descriptor_for::<T>();
        let payload_bytes = core::mem::size_of::<T>();
        let total_bytes = crate::object::estimated_allocation_size::<T>()?;
        let space = crate::collector_policy::select_allocation_space(
            self.heap.config(),
            desc,
            payload_bytes,
        );
        Ok((desc, space, total_bytes))
    }

    pub(crate) fn root_during_active_major_mark(&mut self, object: GcErased) {
        assert!(
            !self.heap.prepared_full_reclaim_active(),
            "cannot add new roots while prepared full reclaim is active"
        );
        let _ = self
            .heap
            .collector_handle()
            .record_active_major_reachable_object_and_refresh(
                self.heap.objects(),
                &self.heap.indexes().object_index,
                object,
                self.heap.config().old.mutator_assist_slices,
                &self.heap.storage_stats(),
                self.heap.old_gen(),
                self.heap.old_config(),
                |kind| self.heap.plan_for(kind),
            )
            .expect("rooting during active major-mark should not fail");
    }

    pub(crate) fn record_post_write(
        &mut self,
        owner: GcErased,
        slot: Option<usize>,
        old_value: Option<GcErased>,
        new_value: Option<GcErased>,
    ) {
        assert!(
            !self.heap.prepared_full_reclaim_active(),
            "cannot mutate heap edges while prepared full reclaim is active"
        );

        self.heap
            .push_barrier_event(BarrierKind::PostWrite, owner, slot, old_value, new_value);

        if old_value.is_some() && self.heap.collector_handle().has_active_major_mark() {
            self.heap.push_barrier_event(
                BarrierKind::SatbPreWrite,
                owner,
                slot,
                old_value,
                new_value,
            );
        }

        self.heap
            .collector_handle()
            .record_active_major_post_write_and_refresh(
                self.heap.objects(),
                &self.heap.indexes().object_index,
                owner,
                old_value,
                new_value,
                self.heap.config().old.mutator_assist_slices,
                &self.heap.storage_stats(),
                self.heap.old_gen(),
                self.heap.old_config(),
                |kind| self.heap.plan_for(kind),
            )
            .expect("post-write active major-mark assist should not fail");

        self.heap.record_remembered_edge_if_needed(owner, new_value);
    }

    /// Begin a persistent major-mark session for one scheduler-provided plan.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        self.heap.collector_handle().begin_major_mark_and_refresh(
            self.heap.objects(),
            &self.heap.indexes().object_index,
            plan,
            self.heap.global_sources(),
            &self.heap.storage_stats(),
            self.heap.old_gen(),
            self.heap.old_config(),
            |kind| self.heap.plan_for(kind),
        )
    }

    /// Advance one scheduler-style concurrent major-mark round using the active plan worker count.
    pub fn poll_active_major_mark(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        self.heap
            .collector_handle()
            .poll_active_major_mark_with_completion_and_refresh(
                self.heap.objects(),
                &self.heap.indexes().object_index,
                |tracer, plan| trace_heap_major_ephemerons(self.heap, tracer, plan),
                |plan| prepare_heap_major_reclaim(self.heap, plan),
                &self.heap.storage_stats(),
                self.heap.old_gen(),
                self.heap.old_config(),
                |kind| self.heap.plan_for(kind),
            )
    }

    /// Advance one slice of the current persistent major-mark session.
    pub fn advance_major_mark(&mut self) -> Result<MajorMarkProgress, AllocError> {
        let progress = self.assist_major_mark(1)?;
        let progress = progress.expect("single-slice assist should require an active session");
        Ok(progress)
    }

    /// Advance up to `max_slices` of the active major-mark session.
    pub fn assist_major_mark(
        &mut self,
        max_slices: usize,
    ) -> Result<Option<MajorMarkProgress>, AllocError> {
        if !self.heap.collector_handle().has_active_major_mark() {
            return Ok(None);
        }
        if max_slices == 0 {
            return Ok(self.heap.major_mark_progress());
        }
        self.heap
            .collector_handle()
            .assist_active_major_mark_slices_and_refresh(
                self.heap.objects(),
                &self.heap.indexes().object_index,
                max_slices,
                &self.heap.storage_stats(),
                self.heap.old_gen(),
                self.heap.old_config(),
                |kind| self.heap.plan_for(kind),
            )
    }

    /// Finish the current persistent major-mark session and reclaim.
    pub fn finish_major_collection(&mut self) -> Result<CollectionStats, AllocError> {
        let pause_start = Instant::now();
        let Some(state) = self.heap.collector_handle().take_major_mark_state() else {
            return Err(AllocError::NoCollectionInProgress);
        };
        let before_bytes = self.heap.stats().total_live_bytes();
        self.heap
            .collector_handle()
            .push_phase(CollectionPhase::Remark);
        let mut state = state;
        collector_session::finish_major_mark(
            &mut state,
            self.heap.objects(),
            &self.heap.indexes().object_index,
            |tracer, plan| trace_heap_major_ephemerons(self.heap, tracer, plan),
        );
        let finished = collector_session::finish_active_collection(state, |plan| {
            self.prepare_reclaim_for_plan(plan)
        })?;
        self.heap
            .collector_handle()
            .push_phase(CollectionPhase::Reclaim);
        Ok(self.commit_finished_active_collection(finished, before_bytes, pause_start))
    }

    /// Prepare reclaim for the active major collection once mark work is fully drained.
    pub fn prepare_active_reclaim_if_needed(&mut self) -> Result<bool, AllocError> {
        let snapshot = self.heap.collector_shared_snapshot();
        if snapshot.active_major_mark_plan.is_none() {
            return Ok(false);
        }
        if snapshot
            .major_mark_progress
            .is_some_and(|progress| !progress.completed)
        {
            return Ok(false);
        }
        let request = self.heap.collector_handle().active_reclaim_prep_request();
        let Some(request) = request else {
            return Ok(false);
        };
        if request.plan.kind == crate::plan::CollectionKind::Major {
            return self
                .heap
                .collector_handle()
                .prepare_active_collection_reclaim_with_request_and_refresh(
                    request,
                    self.heap.objects(),
                    &self.heap.indexes().object_index,
                    |tracer, plan| trace_heap_major_ephemerons(self.heap, tracer, plan),
                    |plan| Ok(prepare_heap_major_reclaim(self.heap, plan)),
                    &self.heap.storage_stats(),
                    self.heap.old_gen(),
                    self.heap.old_config(),
                    |kind| self.heap.plan_for(kind),
                );
        }

        let (mark_steps_delta, mark_rounds_delta) = prepare_active_reclaim(
            &request,
            |tracer, plan| trace_heap_major_ephemerons(self.heap, tracer, plan),
            self.heap.objects(),
            &self.heap.indexes().object_index,
        );
        let prepared =
            build_prepared_active_reclaim(&request, mark_steps_delta, mark_rounds_delta, |plan| {
                self.prepare_reclaim_for_plan(plan)
            })?;
        let prepared = self
            .heap
            .collector_handle()
            .complete_active_reclaim_prep_and_refresh(
                prepared,
                &self.heap.storage_stats(),
                self.heap.old_gen(),
                self.heap.old_config(),
                |kind| self.heap.plan_for(kind),
            );
        Ok(prepared)
    }

    /// Finish the active major collection if its mark work is fully drained.
    pub fn finish_active_major_collection_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        let snapshot = self.heap.collector_shared_snapshot();
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
            if self.prepare_active_reclaim_if_needed()? {
                return Ok(None);
            }
        }
        let snapshot = self.heap.collector_shared_snapshot();
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
        let before_bytes = self.heap.stats().total_live_bytes();
        let pause_start = Instant::now();
        let finished = self
            .heap
            .collector_handle()
            .finish_active_collection_if_ready(
                self.heap.objects(),
                &self.heap.indexes().object_index,
                |_tracer, _plan| panic!("reclaim-ready session should not re-run remark"),
                |plan| Err(AllocError::UnsupportedCollectionKind { kind: plan.kind }),
            )?;
        Ok(finished.map(|finished| {
            self.heap
                .collector_handle()
                .push_phase(CollectionPhase::Reclaim);
            self.commit_finished_active_collection(finished, before_bytes, pause_start)
        }))
    }

    /// Commit the active major collection once reclaim has already been prepared.
    pub fn commit_active_reclaim_if_ready(
        &mut self,
    ) -> Result<Option<CollectionStats>, AllocError> {
        let snapshot = self.heap.collector_shared_snapshot();
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
        let before_bytes = self.heap.stats().total_live_bytes();
        let pause_start = Instant::now();
        let finished = self.heap.collector_handle().finish_active_collection_now(
            self.heap.objects(),
            &self.heap.indexes().object_index,
            |_tracer, _plan| panic!("reclaim-ready session should not re-run remark"),
            |plan| Err(AllocError::UnsupportedCollectionKind { kind: plan.kind }),
        )?;
        self.heap
            .collector_handle()
            .push_phase(CollectionPhase::Reclaim);
        Ok(Some(self.commit_finished_active_collection(
            finished,
            before_bytes,
            pause_start,
        )))
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

    fn prepare_reclaim_for_plan(
        &mut self,
        plan: &CollectionPlan,
    ) -> Result<PreparedReclaim, AllocError> {
        match plan.kind {
            CollectionKind::Major => Ok(prepare_heap_major_reclaim(self.heap, plan)),
            CollectionKind::Full => {
                let mut phases = Vec::new();
                let (
                    roots,
                    objects,
                    indexes,
                    old_gen,
                    stats,
                    old_config,
                    nursery_config,
                    nursery,
                ) = self.heap.collection_exec_parts();
                let prepared = crate::collector_exec::prepare_full_reclaim_for_plan(
                    plan,
                    roots,
                    objects,
                    indexes,
                    old_gen,
                    old_config,
                    nursery_config,
                    stats,
                    nursery,
                    |phase| phases.push(phase),
                )?;
                self.heap.collector_handle().push_phases(phases);
                Ok(prepared)
            }
            CollectionKind::Minor => Err(AllocError::UnsupportedCollectionKind {
                kind: CollectionKind::Minor,
            }),
        }
    }

    fn commit_finished_active_collection(
        &mut self,
        finished: crate::collector_session::FinishedActiveCollection,
        before_bytes: usize,
        pause_start: Instant,
    ) -> CollectionStats {
        let runtime_state = self.heap.runtime_state_handle();
        let runtime_state_for_callback = runtime_state.clone();
        let (objects, indexes, old_gen, stats) = self.heap.finished_reclaim_commit_parts();
        let mut cycle = finish_prepared_reclaim_cycle(
            objects,
            indexes,
            old_gen,
            stats,
            &runtime_state,
            before_bytes,
            finished.mark_steps,
            finished.mark_rounds,
            finished.reclaim_prepare_nanos,
            finished.prepared_reclaim,
            move |object| runtime_state_for_callback.enqueue_pending_finalizer(object),
        );
        cycle.pause_nanos = pause_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.record_completed_cycle(cycle, finished.completed_plan);
        cycle
    }

    fn record_completed_cycle(&mut self, cycle: CollectionStats, completed_plan: CollectionPlan) {
        self.heap.record_collection_stats(cycle);
        self.heap.collector_handle().record_completed_plan(
            completed_plan,
            &self.heap.storage_stats(),
            self.heap.old_gen(),
            self.heap.old_config(),
            |kind| self.heap.plan_for(kind),
        );
    }
}

fn prepare_heap_major_reclaim(heap: &Heap, plan: &CollectionPlan) -> PreparedReclaim {
    prepare_major_reclaim_for_plan(
        plan,
        heap.objects(),
        heap.indexes(),
        heap.old_gen(),
        heap.old_config(),
    )
}

fn trace_heap_major_ephemerons(
    heap: &Heap,
    tracer: &mut crate::collector_exec::MarkTracer<'_>,
    plan: &CollectionPlan,
) -> (u64, u64) {
    trace_major_ephemerons_for_candidates(
        heap.objects(),
        &heap.indexes().object_index,
        &heap.indexes().ephemeron_candidates,
        tracer,
        plan.worker_count.max(1),
        plan.mark_slice_budget,
    )
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

    fn with_heap_read<R>(&self, f: impl FnOnce(&Heap) -> R) -> Result<R, SharedHeapError> {
        let heap = self
            .heap
            .read()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        Ok(f(&heap))
    }

    fn try_with_heap_read<R>(&self, f: impl FnOnce(&Heap) -> R) -> Result<R, SharedHeapError> {
        let heap = self.heap.try_read().map_err(|error| match error {
            std::sync::TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            std::sync::TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        Ok(f(&heap))
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

    /// Recommend the next collection plan from the current shared snapshot.
    pub fn recommended_plan(&self) -> Result<CollectionPlan, SharedBackgroundError> {
        self.collector
            .read_snapshot(|snapshot| snapshot.recommended_plan.clone())
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

    /// Return the last completed collection plan, if any.
    pub fn last_completed_plan(&self) -> Result<Option<CollectionPlan>, SharedBackgroundError> {
        self.collector
            .read_snapshot(|snapshot| snapshot.last_completed_plan.clone())
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
            collector_session::begin_major_mark(
                collector,
                heap.objects(),
                &heap.indexes().object_index,
                plan,
                heap.global_sources(),
            )?;
            refresh_cached_collector_plans(
                collector,
                &heap.storage_stats(),
                heap.old_gen(),
                heap.old_config(),
                |kind| heap.plan_for(kind),
            );
            Ok(())
        })
        .map_err(Self::map_shared_heap_error)?
        .map_err(SharedBackgroundError::Collection)
    }

    /// Begin a persistent major-mark session without blocking on heap contention.
    pub fn try_begin_major_mark(&self, plan: CollectionPlan) -> Result<(), SharedBackgroundError> {
        self.try_with_heap_read_collector_update(|heap, collector| {
            collector_session::begin_major_mark(
                collector,
                heap.objects(),
                &heap.indexes().object_index,
                plan,
                heap.global_sources(),
            )?;
            refresh_cached_collector_plans(
                collector,
                &heap.storage_stats(),
                heap.old_gen(),
                heap.old_config(),
                |kind| heap.plan_for(kind),
            );
            Ok(())
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
            let progress = collector_session::poll_active_major_mark_with_completion(
                collector,
                heap.objects(),
                &heap.indexes().object_index,
                |tracer, plan| trace_heap_major_ephemerons(heap, tracer, plan),
                |plan| prepare_heap_major_reclaim(heap, plan),
            )?;
            refresh_cached_collector_plans(
                collector,
                &heap.storage_stats(),
                heap.old_gen(),
                heap.old_config(),
                |kind| heap.plan_for(kind),
            );
            Ok(progress)
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
            let progress = collector_session::poll_active_major_mark_with_completion(
                collector,
                heap.objects(),
                &heap.indexes().object_index,
                |tracer, plan| trace_heap_major_ephemerons(heap, tracer, plan),
                |plan| prepare_heap_major_reclaim(heap, plan),
            )?;
            refresh_cached_collector_plans(
                collector,
                &heap.storage_stats(),
                heap.old_gen(),
                heap.old_config(),
                |kind| heap.plan_for(kind),
            );
            Ok(progress)
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
        let request = self.collector.active_reclaim_prep_request();
        let Some(request) = request else {
            return Ok(false);
        };
        if request.plan.kind == crate::plan::CollectionKind::Major {
            let prepared = self
                .with_heap_read(|heap| {
                    self.collector
                        .prepare_active_collection_reclaim_with_request_and_refresh(
                            request,
                            heap.objects(),
                            &heap.indexes().object_index,
                            |tracer, plan| trace_heap_major_ephemerons(heap, tracer, plan),
                            |plan| Ok(prepare_heap_major_reclaim(heap, plan)),
                            &heap.storage_stats(),
                            heap.old_gen(),
                            heap.old_config(),
                            |kind| heap.plan_for(kind),
                        )
                })
                .map_err(Self::map_shared_heap_error)?
                .map_err(SharedBackgroundError::Collection)?;
            self.publish_collector_snapshot(self.collector.state_snapshot())
                .map_err(Self::map_shared_heap_error)?;
            return Ok(prepared);
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
        let request = self.collector.active_reclaim_prep_request();
        let Some(request) = request else {
            return Ok(false);
        };
        if request.plan.kind == crate::plan::CollectionKind::Major {
            let prepared = self
                .try_with_heap_read(|heap| {
                    self.collector
                        .prepare_active_collection_reclaim_with_request_and_refresh(
                            request,
                            heap.objects(),
                            &heap.indexes().object_index,
                            |tracer, plan| trace_heap_major_ephemerons(heap, tracer, plan),
                            |plan| Ok(prepare_heap_major_reclaim(heap, plan)),
                            &heap.storage_stats(),
                            heap.old_gen(),
                            heap.old_config(),
                            |kind| heap.plan_for(kind),
                        )
                })
                .map_err(Self::map_shared_heap_error)?
                .map_err(SharedBackgroundError::Collection)?;
            self.publish_collector_snapshot(self.collector.state_snapshot())
                .map_err(Self::map_shared_heap_error)?;
            return Ok(prepared);
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
