use crate::background::{
    BackgroundCollectionRuntime, BackgroundCollectorConfig, BackgroundWorker,
    BackgroundWorkerConfig, SharedBackgroundError, SharedBackgroundObservation,
    SharedBackgroundService, SharedBackgroundStatus, SharedBackgroundWaitResult,
    SharedCollectorHandle, SharedHeap, SharedHeapError, SharedHeapStatus, SharedRuntimeHandle,
};
use crate::collector_exec::{
    execute_collection_plan, prepare_major_reclaim_for_plan, trace_major_ephemerons_for_candidates,
};
use crate::collector_policy::refresh_cached_plans as refresh_cached_collector_plans;
use crate::collector_session::{self, build_prepared_active_reclaim, prepare_active_reclaim};
use crate::collector_state::{CollectorSharedSnapshot, CollectorState};
use crate::descriptor::{GcErased, TypeDesc};
use crate::heap::{AllocError, HeapCore};
use crate::object::SpaceKind;
use crate::plan::{
    BackgroundCollectionStatus, CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress,
    RuntimeWorkStatus,
};
use crate::reclaim::{PreparedReclaim, finish_prepared_reclaim_cycle};
use crate::stats::{CollectionStats, HeapStats};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

#[cfg(test)]
use crate::root::{HandleScope, Root};

/// Collector-side runtime bound to one heap.
///
/// The runtime carries a per-cycle `MutatorLocal` that it
/// either borrows from an outer [`crate::mutator::Mutator`]
/// or owns for the duration of the call. Every collector
/// entry point that needs to walk roots threads that local
/// through.
#[derive(Debug)]
pub struct CollectorRuntime<'heap> {
    heap: &'heap mut HeapCore,
    local: CollectorLocal<'heap>,
}

/// Borrow of the [`crate::mutator::MutatorLocal`] carried by
/// [`CollectorRuntime`]. The runtime always borrows from
/// either an outer [`crate::mutator::Mutator`] (the
/// production path) or a scratch local owned by
/// [`crate::heap::HeapCollectorRuntime`] (the non-mutator
/// path). The runtime itself never owns the local.
#[derive(Debug)]
pub(crate) struct CollectorLocal<'heap> {
    inner: &'heap mut crate::mutator::MutatorLocal,
}

impl CollectorLocal<'_> {
    pub(crate) fn get(&self) -> &crate::mutator::MutatorLocal {
        self.inner
    }

    pub(crate) fn get_mut(&mut self) -> &mut crate::mutator::MutatorLocal {
        self.inner
    }
}

/// Try to bump-allocate `layout` through the caller-supplied
/// nursery TLAB slab. On TLAB miss (including the "no TLAB
/// reserved yet" case and the "generation-stale" case after
/// a minor cycle), refill the slab from the shared from-
/// space cursor via `NurseryState::reserve_tlab` and retry
/// the bump once. Returns `None` if neither the existing
/// TLAB nor the refilled one can service the layout; the
/// caller falls through to the shared-cursor bump path.
///
/// The `tlab_slot` parameter is a `&mut Option<NurseryTlab>`
/// so the caller can own the slab wherever it likes —
/// currently on `MutatorLocal`, so each mutator has its own
/// per-mutator slab without serializing on the shared
/// cursor for the common case.
///
/// `tlab_bytes` comes from
/// [`crate::spaces::NurseryConfig::tlab_bytes`] and controls
/// how much from-space capacity each refill reserves.
pub(crate) fn try_bump_nursery_tlab_or_refill(
    tlab_slot: &mut Option<crate::spaces::nursery_arena::NurseryTlab>,
    nursery: &mut crate::spaces::nursery_arena::NurseryState,
    layout: core::alloc::Layout,
    tlab_bytes: usize,
) -> Option<core::ptr::NonNull<u8>> {
    let current_generation = nursery.generation();

    if let Some(tlab) = tlab_slot.as_mut()
        && let Some(base) = tlab.try_alloc(current_generation, layout)
    {
        return Some(base);
    }

    // The existing TLAB (if any) is either exhausted or stale.
    // Drop it and try to refill from the shared cursor. The
    // refill size is `max(layout.size(), tlab_bytes)` so a
    // single oversized allocation still has a chance of
    // fitting via the TLAB path even when tlab_bytes is set
    // smaller than the object.
    *tlab_slot = None;
    let refill_size = tlab_bytes.max(layout.size());
    if refill_size == 0 {
        return None;
    }
    let mut fresh = nursery.reserve_tlab(refill_size)?;
    let base = fresh.try_alloc(current_generation, layout)?;
    *tlab_slot = Some(fresh);
    Some(base)
}

/// Collector-side runtime bound to one shared heap.
#[derive(Clone, Debug)]
pub struct SharedCollectorRuntime {
    heap: SharedHeap,
    runtime: SharedRuntimeHandle,
    collector: SharedCollectorHandle,
}

impl<'heap> CollectorRuntime<'heap> {
    /// Build a runtime that borrows the supplied mutator
    /// local. Used by [`crate::heap::HeapCollectorRuntime::runtime`]
    /// (which carries a scratch local) and by
    /// [`crate::mutator::Mutator::with_runtime`] (which
    /// borrows the mutator's own local).
    pub(crate) fn with_local(
        heap: &'heap mut HeapCore,
        local: &'heap mut crate::mutator::MutatorLocal,
    ) -> Self {
        Self {
            heap,
            local: CollectorLocal { inner: local },
        }
    }

    /// Return current heap statistics.
    pub fn stats(&self) -> HeapStats {
        self.heap.stats()
    }

    /// Return the most recently completed collection plan, if any.
    pub fn last_completed_plan(&self) -> Option<CollectionPlan> {
        self.heap.last_completed_plan()
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

    /// Run at most `max` queued finalizers and return the number
    /// that actually ran. See [`crate::heap::Heap::drain_pending_finalizers_bounded`].
    pub fn drain_pending_finalizers_bounded(&mut self, max: usize) -> u64 {
        self.heap.drain_pending_finalizers_bounded(max)
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
        let roots = self.local.get_mut().roots_mut();
        let mut cycle = self.heap.with_flat_store_for_collection(
            |flat, old_gen, old_config, nursery_config, stats, nursery| {
                execute_collection_plan(
                    &plan,
                    roots,
                    &mut flat.objects,
                    &mut flat.indexes,
                    old_gen,
                    old_config,
                    nursery_config,
                    stats,
                    nursery,
                    &runtime_state,
                    |phase| phases.push(phase),
                )
            },
        )?;
        self.heap.collector_handle().push_phases(phases);
        // Physical compaction hook (physical-compaction step 6).
        //
        // After a synchronous major or full cycle commits, run
        // physical old-gen compaction if the heap is configured
        // for it. The hook is gated on
        // `physical_compaction_density_threshold > 0.0`, so
        // heaps with the default 0.0 threshold see no behavior
        // change. A matching hook in
        // `commit_finished_active_collection` covers the
        // background concurrent path.
        if matches!(plan.kind, CollectionKind::Major | CollectionKind::Full) {
            let density_threshold = self.heap.old_config().physical_compaction_density_threshold;
            if density_threshold > 0.0 {
                let roots = self.local.get_mut().roots_mut();
                self.heap.compact_old_gen_physical(roots, density_threshold);
            }
        }
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
                    self.dispatch_collection_plan(plan)?;
                    // Only count the trigger after dispatch
                    // succeeds so pacer_triggered_majors agrees
                    // with observed_cycles for the pacer path.
                    self.heap.pacer().record_pacer_triggered_major();
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
                    self.dispatch_collection_plan(plan)?;
                    self.heap.pacer().record_pacer_triggered_minor();
                }
            }
            crate::pacer::PacerDecision::Continue => {}
        }
        Ok(())
    }

    /// Try to allocate `layout` bytes of nursery storage
    /// by bumping within the carried local's TLAB. Returns
    /// `None` if both the TLAB refill and the shared
    /// from-space cursor fail.
    #[cfg(test)]
    fn try_alloc_nursery_with_local(
        &mut self,
        layout: core::alloc::Layout,
    ) -> Option<core::ptr::NonNull<u8>> {
        let tlab_bytes = self.heap.config().nursery.tlab_bytes;
        // Split borrow: self.local and self.heap are disjoint
        // fields of CollectorRuntime, so we can mutably
        // reference both at the same time.
        let local: &mut crate::mutator::MutatorLocal = self.local.get_mut();
        let tlab = &mut local.tlab;
        let heap: &mut HeapCore = &mut *self.heap;
        try_bump_nursery_tlab_or_refill(tlab, heap.nursery_mut(), layout, tlab_bytes)
            .or_else(|| heap.nursery_mut().try_alloc(layout))
    }

    /// Allocate a typed managed object through this
    /// runtime's carried `MutatorLocal`.
    ///
    /// Nursery allocations attempt to bump within the
    /// local's TLAB slab via
    /// [`try_bump_nursery_tlab_or_refill`]. On TLAB hit the
    /// allocation never touches the shared from-space
    /// cursor. On TLAB miss the slab is refilled from the
    /// shared cursor; on refill failure the allocation
    /// falls through to the shared-cursor bump path; on
    /// shared-cursor failure the allocation falls back to
    /// the system allocator.
    ///
    /// Direct old-gen allocations bump-allocate from the
    /// block pool with a system-alloc fallback, identical
    /// to the no-TLAB path.
    ///
    /// Pinned, large, and immortal-space allocations always
    /// bypass the TLAB and go through the system allocator.
    #[cfg(test)]
    pub(crate) fn alloc_typed_scoped<
        'scope,
        'handle_heap,
        T: crate::descriptor::Trace + 'static,
    >(
        &mut self,
        scope: &mut HandleScope<'scope, 'handle_heap>,
        value: T,
    ) -> Result<Root<'scope, T>, AllocError> {
        if self.heap.prepared_full_reclaim_active() {
            return Err(AllocError::CollectionInProgress);
        }
        let (desc, space, _) = self.typed_allocation_profile::<T>()?;

        let record = match space {
            SpaceKind::Nursery => {
                let (layout, payload_offset) = crate::object::allocation_layout_for::<T>()?;
                let base = self.try_alloc_nursery_with_local(layout);
                match base {
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
                let (layout, payload_offset) = crate::object::allocation_layout_for::<T>()?;
                let old_config = *self.heap.old_config();
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
        let commit = self.heap.commit_allocated_record(record)?;
        if commit.plans_dirty {
            self.heap.refresh_recommended_plans();
        }
        let gc = unsafe { crate::root::Gc::from_erased(commit.gc) };
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
        let objects = self.heap.objects();
        let _ = self
            .heap
            .collector_handle()
            .record_active_major_reachable_object_and_refresh(
                objects.raw(),
                object,
                self.heap.config().old.mutator_assist_slices,
                || self.heap.storage_stats(),
                self.heap.old_gen(),
                self.heap.old_config(),
                |kind| self.heap.plan_for(kind),
            )
            .expect("rooting during active major-mark should not fail");
    }

    // NOTE: `record_post_write` was removed after the barrier
    // fast path was moved onto the `HeapCore` read-lock path
    // in `Mutator::post_write_barrier`. The old write-lock
    // variant is no longer needed because the barrier no
    // longer requires exclusive access to the heap core.

    /// Begin a persistent major-mark session for one scheduler-provided plan.
    pub fn begin_major_mark(&mut self, plan: CollectionPlan) -> Result<(), AllocError> {
        let sources = self.heap.global_sources_with_roots(&self.local.get().roots);
        let objects = self.heap.objects();
        self.heap.collector_handle().begin_major_mark_and_refresh(
            objects.raw(),
            plan,
            sources,
            &self.heap.storage_stats(),
            self.heap.old_gen(),
            self.heap.old_config(),
            |kind| self.heap.plan_for(kind),
        )
    }

    /// Advance one scheduler-style concurrent major-mark round using the active plan worker count.
    pub fn poll_active_major_mark(&mut self) -> Result<Option<MajorMarkProgress>, AllocError> {
        let progress = {
            let objects = self.heap.objects();
            self.heap
                .collector_handle()
                .with_state(|state| collector_session::poll_active_major_mark_round(state, objects.raw()))
        }?;
        let auto_prepare_major_reclaim = progress.as_ref().is_some_and(|progress| progress.completed)
            && self
                .heap
                .collector_handle()
                .active_reclaim_prep_request()
                .is_some_and(|request| request.plan.kind == CollectionKind::Major);
        if auto_prepare_major_reclaim {
            let _ = self.prepare_active_reclaim_if_needed()?;
            return Ok(progress);
        }
        self.heap.collector_handle().refresh_cached_plans(
            &self.heap.storage_stats(),
            self.heap.old_gen(),
            self.heap.old_config(),
            |kind| self.heap.plan_for(kind),
        );
        Ok(progress)
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
                self.heap.objects().raw(),
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
        {
            let objects = self.heap.objects();
            collector_session::finish_major_mark(
                &mut state,
                objects.raw(),
                |tracer, plan| trace_heap_major_ephemerons(self.heap, tracer, plan),
            );
        }
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
        let (mark_steps_delta, mark_rounds_delta) = {
            let objects = self.heap.objects();
            prepare_active_reclaim(
                &request,
                |tracer, plan| trace_heap_major_ephemerons(self.heap, tracer, plan),
                objects.raw(),
            )
        };
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
            && self.prepare_active_reclaim_if_needed()?
        {
            return Ok(None);
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
                self.heap.objects().raw(),
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
            self.heap.objects().raw(),
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
                let roots = self.local.get_mut().roots_mut();
                let prepared = self.heap.with_flat_store_for_collection(
                    |flat, old_gen, old_config, nursery_config, stats, nursery| {
                        crate::collector_exec::prepare_full_reclaim_for_plan(
                            plan,
                            roots,
                            &mut flat.objects,
                            &mut flat.indexes,
                            old_gen,
                            old_config,
                            nursery_config,
                            stats,
                            nursery,
                            |phase| phases.push(phase),
                        )
                    },
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
        let mut cycle = self.heap.with_flat_store_for_reclaim_commit(|flat, old_gen, stats| {
            finish_prepared_reclaim_cycle(
                &mut flat.objects,
                &mut flat.indexes,
                old_gen,
                stats,
                &runtime_state,
                before_bytes,
                finished.mark_steps,
                finished.mark_rounds,
                finished.mark_elapsed_nanos,
                finished.reclaim_prepare_nanos,
                finished.prepared_reclaim,
                move |object| runtime_state_for_callback.enqueue_pending_finalizer(object),
            )
        });
        // Physical compaction hook (physical-compaction step 6).
        //
        // After the major reclaim commits, the objects vec
        // contains only survivors of the mark phase. Walk the
        // sparse old-gen blocks and physically evacuate every
        // live record whose home block is at or below the
        // configured density threshold. This genuinely moves
        // bytes: the source blocks become empty and get dropped
        // by the next sweep. The threshold defaults to 0.0
        // (disabled) so existing workloads that do not opt in
        // see no behavior change.
        let density_threshold = self.heap.old_config().physical_compaction_density_threshold;
        if density_threshold > 0.0 {
            let roots = self.local.get_mut().roots_mut();
            self.heap.compact_old_gen_physical(roots, density_threshold);
        }
        cycle.pause_nanos = pause_start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.record_completed_cycle(cycle, finished.completed_plan);
        cycle
    }

    fn record_completed_cycle(&mut self, cycle: CollectionStats, completed_plan: CollectionPlan) {
        self.heap.record_collection_stats(cycle);
        // Sync the atomic allocation counters from the
        // post-cycle HeapStats so the hot-path readers see
        // the GC-rebuilt values (apply_space_rebuild rewrites
        // all five per-space live_bytes/reserved_bytes).
        self.heap.sync_alloc_counters();
        self.heap.collector_handle().record_completed_plan(
            completed_plan,
            &self.heap.storage_stats(),
            self.heap.old_gen(),
            self.heap.old_config(),
            |kind| self.heap.plan_for(kind),
        );
    }
}

fn prepare_heap_major_reclaim(heap: &mut HeapCore, plan: &CollectionPlan) -> PreparedReclaim {
    let mut flat = heap.take_flat_store();
    let prepared = prepare_major_reclaim_for_plan(
        plan,
        &flat.objects,
        &flat.indexes,
        heap.old_gen(),
        heap.old_config(),
    );
    heap.restore_flat_store(flat);
    prepared
}

fn trace_heap_major_ephemerons(
    heap: &HeapCore,
    tracer: &mut crate::collector_exec::MarkTracer<'_>,
    plan: &CollectionPlan,
) -> (u64, u64) {
    let objects = heap.objects();
    let ephemeron_candidates = objects.ephemeron_candidates();
    trace_major_ephemerons_for_candidates(
        objects.raw(),
        &ephemeron_candidates,
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

    fn with_heap_read<R>(&self, f: impl FnOnce(&HeapCore) -> R) -> Result<R, SharedHeapError> {
        let heap = self
            .heap
            .read()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let core = heap.read_core();
        Ok(f(&core))
    }

    fn try_with_heap_read<R>(&self, f: impl FnOnce(&HeapCore) -> R) -> Result<R, SharedHeapError> {
        let heap = self.heap.try_read().map_err(|error| match error {
            std::sync::TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            std::sync::TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        let core = heap.read_core();
        Ok(f(&core))
    }

    fn with_heap_read_collector_update<R>(
        &self,
        f: impl FnOnce(&HeapCore, &mut CollectorState) -> Result<R, AllocError>,
    ) -> Result<Result<R, AllocError>, SharedHeapError> {
        let heap = self
            .heap
            .read()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let core = heap.read_core();
        let result = self.collector.with_state(|collector| {
            f(&core, collector).map(|value| (value, collector.shared_snapshot()))
        })?;
        match result {
            Ok((value, collector_snapshot)) => {
                drop(core);
                drop(heap);
                self.publish_collector_snapshot(collector_snapshot)?;
                Ok(Ok(value))
            }
            Err(error) => Ok(Err(error)),
        }
    }

    fn try_with_heap_read_collector_update<R>(
        &self,
        f: impl FnOnce(&HeapCore, &mut CollectorState) -> Result<R, AllocError>,
    ) -> Result<Result<R, AllocError>, SharedHeapError> {
        let heap = self.heap.try_read().map_err(|error| match error {
            std::sync::TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            std::sync::TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        let core = heap.read_core();
        let result = self.collector.try_with_state(|collector| {
            f(&core, collector).map(|value| (value, collector.shared_snapshot()))
        })?;
        match result {
            Ok((value, collector_snapshot)) => {
                drop(core);
                drop(heap);
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
        let heap = self
            .heap
            .lock()
            .map_err(|_| SharedHeapError::LockPoisoned)?;
        let mut guard = heap.collector_runtime();
        let mut runtime = guard.runtime();
        Ok(f(&mut runtime))
    }

    fn try_with_runtime_update<R>(
        &self,
        f: impl for<'heap> FnOnce(&mut CollectorRuntime<'heap>) -> Result<R, AllocError>,
    ) -> Result<Result<R, AllocError>, SharedHeapError> {
        let heap = self.heap.try_lock().map_err(|error| match error {
            std::sync::TryLockError::Poisoned(_) => SharedHeapError::LockPoisoned,
            std::sync::TryLockError::WouldBlock => SharedHeapError::WouldBlock,
        })?;
        let mut guard = heap.collector_runtime();
        let mut runtime = guard.runtime();
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

    /// Run at most `max` queued finalizers and return the number
    /// that actually ran. See [`crate::heap::Heap::drain_pending_finalizers_bounded`].
    pub fn drain_pending_finalizers_bounded(
        &self,
        max: usize,
    ) -> Result<u64, SharedBackgroundError> {
        self.runtime
            .drain_pending_finalizers_bounded(max)
            .map_err(Self::map_shared_heap_error)
    }

    /// Run and drain queued finalizers without blocking on heap contention.
    pub fn try_drain_pending_finalizers(&self) -> Result<u64, SharedBackgroundError> {
        self.runtime
            .try_drain_pending_finalizers()
            .map_err(Self::map_shared_heap_error)
    }

    /// Run at most `max` queued finalizers without blocking on
    /// heap contention. See
    /// [`crate::heap::Heap::drain_pending_finalizers_bounded`] for the
    /// blocking variant's semantics.
    pub fn try_drain_pending_finalizers_bounded(
        &self,
        max: usize,
    ) -> Result<u64, SharedBackgroundError> {
        self.runtime
            .try_drain_pending_finalizers_bounded(max)
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
        self.with_runtime_update(|runtime| runtime.begin_major_mark(plan))
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.publish_collector_snapshot(self.collector.state_snapshot())
            .map_err(Self::map_shared_heap_error)?;
        Ok(())
    }

    /// Begin a persistent major-mark session without blocking on heap contention.
    pub fn try_begin_major_mark(&self, plan: CollectionPlan) -> Result<(), SharedBackgroundError> {
        self.try_with_runtime_update(|runtime| runtime.begin_major_mark(plan))
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.publish_collector_snapshot(self.collector.state_snapshot())
            .map_err(Self::map_shared_heap_error)?;
        Ok(())
    }

    /// Advance one scheduler-style concurrent major-mark round using the active plan worker
    /// count.
    pub fn poll_active_major_mark(
        &self,
    ) -> Result<Option<MajorMarkProgress>, SharedBackgroundError> {
        let progress = self
            .with_runtime_update(|runtime| runtime.poll_active_major_mark())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.publish_collector_snapshot(self.collector.state_snapshot())
            .map_err(Self::map_shared_heap_error)?;
        Ok(progress)
    }

    /// Advance one scheduler-style concurrent major-mark round without blocking on heap
    /// contention.
    pub fn try_poll_active_major_mark(
        &self,
    ) -> Result<Option<MajorMarkProgress>, SharedBackgroundError> {
        let progress = self
            .try_with_runtime_update(|runtime| runtime.poll_active_major_mark())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.publish_collector_snapshot(self.collector.state_snapshot())
            .map_err(Self::map_shared_heap_error)?;
        Ok(progress)
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
        if request.plan.kind == CollectionKind::Major {
            let prepared = self
                .with_heap_read_collector_update(|core, collector| {
                    let objects = core.objects();
                    let (mark_steps_delta, mark_rounds_delta) = collector_session::prepare_active_reclaim(
                        &request,
                        |tracer, plan| trace_heap_major_ephemerons(core, tracer, plan),
                        objects.raw(),
                    );
                    Ok(collector.complete_active_major_reclaim_prep(
                        mark_steps_delta,
                        mark_rounds_delta,
                        Duration::ZERO,
                        None,
                    ))
                })
                .map_err(Self::map_shared_heap_error)?
                .map_err(SharedBackgroundError::Collection)?;
            return Ok(prepared);
        }
        let prepared = self
            .with_runtime_update(|runtime| runtime.prepare_active_reclaim_if_needed())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.publish_collector_snapshot(self.collector.state_snapshot())
            .map_err(Self::map_shared_heap_error)?;
        Ok(prepared)
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
        if request.plan.kind == CollectionKind::Major {
            let prepared = self
                .try_with_heap_read_collector_update(|core, collector| {
                    let objects = core.objects();
                    let (mark_steps_delta, mark_rounds_delta) = collector_session::prepare_active_reclaim(
                        &request,
                        |tracer, plan| trace_heap_major_ephemerons(core, tracer, plan),
                        objects.raw(),
                    );
                    Ok(collector.complete_active_major_reclaim_prep(
                        mark_steps_delta,
                        mark_rounds_delta,
                        Duration::ZERO,
                        None,
                    ))
                })
                .map_err(Self::map_shared_heap_error)?
                .map_err(SharedBackgroundError::Collection)?;
            return Ok(prepared);
        }
        let prepared = self
            .try_with_runtime_update(|runtime| runtime.prepare_active_reclaim_if_needed())
            .map_err(Self::map_shared_heap_error)?
            .map_err(SharedBackgroundError::Collection)?;
        self.publish_collector_snapshot(self.collector.state_snapshot())
            .map_err(Self::map_shared_heap_error)?;
        Ok(prepared)
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
            match self.try_prepare_active_reclaim_if_needed() {
                Ok(true) | Err(SharedBackgroundError::WouldBlock) => return Ok(None),
                Ok(false) => {}
                Err(error) => return Err(error),
            }
        }
        match self.try_with_runtime_update(|runtime| runtime.finish_active_major_collection_if_ready()) {
            Ok(result) => result.map_err(SharedBackgroundError::Collection),
            Err(SharedHeapError::WouldBlock) => Ok(None),
            Err(error) => Err(Self::map_shared_heap_error(error)),
        }
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
        match self.try_with_runtime_update(|runtime| runtime.commit_active_reclaim_if_ready()) {
            Ok(result) => result.map_err(SharedBackgroundError::Collection),
            Err(SharedHeapError::WouldBlock) => Ok(None),
            Err(error) => Err(Self::map_shared_heap_error(error)),
        }
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
            match self.try_prepare_active_reclaim_if_needed() {
                Ok(true) | Err(SharedBackgroundError::WouldBlock) => return Ok(None),
                Ok(false) => {}
                Err(error) => return Err(error),
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
