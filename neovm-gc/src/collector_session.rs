use std::time::{Duration, Instant};

use crate::collector_exec::MarkTracer;
use crate::collector_state::{CollectorState, MajorMarkUpdate};
use crate::heap::AllocError;
use crate::index_state::ObjectIndex;
use crate::object::ObjectRecord;
use crate::plan::{CollectionKind, CollectionPhase, CollectionPlan, MajorMarkProgress};
use crate::reclaim::PreparedReclaim;

#[derive(Clone, Debug)]
pub(crate) struct ActiveReclaimPrepRequest {
    pub(crate) plan: CollectionPlan,
    pub(crate) ephemerons_processed: bool,
}

#[derive(Debug)]
pub(crate) struct PreparedActiveReclaim {
    pub(crate) mark_steps_delta: u64,
    pub(crate) mark_rounds_delta: u64,
    pub(crate) reclaim_prepare_time: Duration,
    pub(crate) prepared_reclaim: PreparedReclaim,
}

pub(crate) fn begin_major_mark(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
    plan: CollectionPlan,
    seed_roots: impl FnOnce(&mut MarkTracer<'_>),
) -> Result<(), AllocError> {
    if collector.has_active_major_mark() {
        return Err(AllocError::CollectionInProgress);
    }
    if !matches!(plan.kind, CollectionKind::Major | CollectionKind::Full) {
        return Err(AllocError::UnsupportedCollectionKind { kind: plan.kind });
    }

    collector.clear_recent_phase_trace();
    for object in objects {
        object.clear_mark();
    }

    collector.push_phase(CollectionPhase::InitialMark);
    if plan.concurrent {
        collector.push_phase(CollectionPhase::ConcurrentMark);
    }

    let mut tracer = MarkTracer::new(objects, index);
    seed_roots(&mut tracer);
    collector.begin_major_mark(plan, tracer.into_worklist());
    Ok(())
}

pub(crate) fn advance_major_mark_slice(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
) -> Result<MajorMarkProgress, AllocError> {
    collector.update_active_major_mark(|plan, worklist| {
        let mut tracer = MarkTracer::with_worklist(objects, index, worklist);
        let drained_objects = tracer.drain_one_slice(plan.mark_slice_budget);
        MajorMarkUpdate {
            worklist: tracer.into_worklist(),
            drained_objects,
            mark_steps_delta: u64::from(drained_objects > 0),
            mark_rounds_delta: u64::from(drained_objects > 0),
        }
    })
}

pub(crate) fn poll_active_major_mark_round(
    collector: &mut CollectorState,
    objects: &[ObjectRecord],
    index: &ObjectIndex,
) -> Result<Option<MajorMarkProgress>, AllocError> {
    if !collector.has_active_major_mark() {
        return Ok(None);
    }
    collector
        .update_active_major_mark(|plan, worklist| {
            let mut tracer = MarkTracer::with_worklist(objects, index, worklist);
            let (drained_objects, drained_slices) =
                tracer.drain_worker_round(plan.worker_count.max(1), plan.mark_slice_budget);
            MajorMarkUpdate {
                worklist: tracer.into_worklist(),
                drained_objects,
                mark_steps_delta: drained_slices,
                mark_rounds_delta: u64::from(drained_objects > 0),
            }
        })
        .map(Some)
}

pub(crate) fn active_reclaim_prep_request(
    collector: &CollectorState,
) -> Option<ActiveReclaimPrepRequest> {
    collector
        .active_major_mark_needs_reclaim_prep_plan()
        .map(|plan| ActiveReclaimPrepRequest {
            plan,
            ephemerons_processed: collector.active_major_mark_ephemerons_processed(),
        })
}

pub(crate) fn prepare_active_reclaim(
    request: &ActiveReclaimPrepRequest,
    trace_ephemerons: impl FnOnce(&mut MarkTracer<'_>, &CollectionPlan) -> (u64, u64),
    objects: &[ObjectRecord],
    index: &ObjectIndex,
) -> (u64, u64) {
    let mut mark_steps_delta = 0u64;
    let mut mark_rounds_delta = 0u64;
    if !request.ephemerons_processed {
        let mut tracer = MarkTracer::with_worklist(objects, index, Default::default());
        let (ephemeron_steps, ephemeron_rounds) = trace_ephemerons(&mut tracer, &request.plan);
        mark_steps_delta = mark_steps_delta.saturating_add(ephemeron_steps);
        mark_rounds_delta = mark_rounds_delta.saturating_add(ephemeron_rounds);
    }
    (mark_steps_delta, mark_rounds_delta)
}

pub(crate) fn build_prepared_active_reclaim(
    request: &ActiveReclaimPrepRequest,
    mark_steps_delta: u64,
    mark_rounds_delta: u64,
    prepare_reclaim: impl FnOnce(&CollectionPlan) -> Result<PreparedReclaim, AllocError>,
) -> Result<PreparedActiveReclaim, AllocError> {
    let reclaim_prepare_start = Instant::now();
    let prepared_reclaim = prepare_reclaim(&request.plan)?;
    Ok(PreparedActiveReclaim {
        mark_steps_delta,
        mark_rounds_delta,
        reclaim_prepare_time: reclaim_prepare_start.elapsed(),
        prepared_reclaim,
    })
}

pub(crate) fn complete_active_reclaim_prep(
    collector: &mut CollectorState,
    prepared: PreparedActiveReclaim,
) -> bool {
    let completed = collector.complete_active_major_reclaim_prep(
        prepared.mark_steps_delta,
        prepared.mark_rounds_delta,
        prepared.reclaim_prepare_time,
        prepared.prepared_reclaim,
    );
    debug_assert!(
        completed,
        "active major reclaim prep should only complete while the session stays active"
    );
    completed
}

#[cfg(test)]
#[path = "collector_session_test.rs"]
mod tests;
